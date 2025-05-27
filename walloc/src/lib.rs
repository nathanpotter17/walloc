use wasm_bindgen::prelude::*;
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};
use reqwest::Client;
use wasm_bindgen_futures::{future_to_promise};
use std::collections::HashMap;
use js_sys::Promise;

#[wasm_bindgen]
pub struct Walloc {
    strategy: TieredAllocator,
    memory_base: *mut u8,
    memory_size: usize,
}

#[repr(C)]
struct BlockHeader {
    size: usize,
    next: *mut BlockHeader,
    is_free: bool,
    tier: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    Render = 0,   // Top tier: Mesh data, render targets (frequent reallocation, cache-aligned)
    Scene = 1,    // Middle tier: Scene data, gameplay systems (medium lifecycle)
    Entity = 2,   // Bottom tier: Actors, particles, effects (short lifecycle)
}

impl Tier {
    fn from_u8(value: u8) -> Option<Tier> {
        match value {
            0 => Some(Tier::Render),
            1 => Some(Tier::Scene),
            2 => Some(Tier::Entity),
            _ => None,
        }
    }
}

pub struct Arena {
    base: *mut u8,
    size: usize,
    current_offset: AtomicUsize,
    tier: Tier,

    high_water_mark: AtomicUsize,  // Track the highest allocation point
    total_allocated: AtomicUsize,  // Track total bytes allocated, even when recycled
}

pub struct MemoryOwner {
    arena: Arc<Mutex<Arena>>,
    allocations: Vec<(usize, usize)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssetType {
    Image = 0,
    Json = 1,
}

#[derive(Clone)]
struct AssetMetadata {
    asset_type: AssetType,
    size: usize,
    offset: usize,
}

pub struct TieredAllocator {
    render_arena: Arc<Mutex<Arena>>,
    scene_arena: Arc<Mutex<Arena>>,
    entity_arena: Arc<Mutex<Arena>>,

    assets: Arc<Mutex<HashMap<String, AssetMetadata>>>,
    base_url: Arc<Mutex<String>>,
    http_client: Client,
}

// Arena implementation for tiered allocation
impl Arena {
    pub fn new(base: *mut u8, size: usize, tier: Tier) -> Self {
        Self {
            base,
            size,
            current_offset: AtomicUsize::new(0),
            tier,
            high_water_mark: AtomicUsize::new(0),
            total_allocated: AtomicUsize::new(0),
        }
    }
    
    // Bump allocation - very fast track total allocated memory and high water mark
    pub fn allocate(&self, size: usize) -> Option<(*mut u8, usize)> {
        // Align size to appropriate boundary based on tier
        let aligned_size = match self.tier {
            Tier::Render => (size + 127) & !127,  // 128-byte alignment for GPU warp access
            Tier::Scene => (size + 63) & !63,     // 64-byte alignment for cache lines
            Tier::Entity => (size + 7) & !7,      // 8-byte alignment for other tiers
        };
        
        // Atomic compare-and-swap to reserve space
        let mut current_offset = self.current_offset.load(Ordering::Relaxed);
        loop {
            // Check if we have enough space
            if current_offset + aligned_size > self.size {
                return None; // Not enough space
            }
            
            // Try to advance the offset
            let new_offset = current_offset + aligned_size;
            match self.current_offset.compare_exchange(
                current_offset, 
                new_offset,
                Ordering::SeqCst,
                Ordering::Relaxed
            ) {
                Ok(_) => {
                    // Success! Update the high water mark if needed
                    let hwm = self.high_water_mark.load(Ordering::Relaxed);
                    if new_offset > hwm {
                        self.high_water_mark.store(new_offset, Ordering::Relaxed);
                    }
                    
                    // Update total allocated bytes
                    self.total_allocated.fetch_add(aligned_size, Ordering::Relaxed);
                    
                    // Return pointer to the allocated memory
                    let ptr = unsafe { self.base.add(current_offset) };
                    return Some((ptr, aligned_size));
                }
                Err(actual) => {
                    // Try again with the updated offset
                    current_offset = actual;
                }
            }
        }
    }
    
    // Reset the entire arena - very efficient way to free everything at once
    pub fn reset(&self) {
        self.current_offset.store(0, Ordering::SeqCst);
    }
    
    // Check if a pointer belongs to this arena
    pub fn contains(&self, ptr: *mut u8) -> bool {
        let end = unsafe { self.base.add(self.size) };
        ptr >= self.base && ptr < end
    }
    
    // Get current usage
    pub fn usage(&self) -> usize {
        self.current_offset.load(Ordering::Relaxed)
    }
    
    // Get capacity
    pub fn capacity(&self) -> usize {
        self.size
    }

    // Fast compact operation that preserves the first 'preserve_bytes' of memory
    // Note: This will return false if preserve_bytes > current_offset.
    // The TieredAllocator::fast_compact_tier handles the case of growing
    // memory when needed before calling this method.
    pub fn fast_compact(&self, preserve_bytes: usize) -> bool {
        // Ensure we don't preserve more than our current offset
        let current = self.current_offset.load(Ordering::Relaxed);
        if preserve_bytes > current {
            return false; // Can't preserve more than we've allocated
        }
        
        // Simple atomic store to update the allocation pointer
        // This effectively "recycles" all memory after the preserved section
        self.current_offset.store(preserve_bytes, Ordering::SeqCst);
        
        true
    }

    pub fn get_stats(&self) -> (usize, usize, usize, usize) {
        (
            self.usage(),
            self.capacity(),
            self.high_water_mark.load(Ordering::Relaxed),
            self.total_allocated.load(Ordering::Relaxed)
        )
    }
}

// TieredAllocator implementation
impl TieredAllocator {
    pub fn new(memory_base: *mut u8, memory_size: usize) -> Self {
        // Calculate sizes for each arena
        // Render tier: 50% of memory, Scene tier: 30%, Entity tier: 20%
        let render_size = (memory_size * 50) / 100;
        let scene_size = (memory_size * 30) / 100;
        let entity_size = (memory_size * 20) / 100;
        
        // Create arenas
        let render_base = memory_base;
        let scene_base = unsafe { render_base.add(render_size) };
        let entity_base = unsafe { scene_base.add(scene_size) };
        
        let render_arena = Arena::new(render_base, render_size, Tier::Render);
        let scene_arena = Arena::new(scene_base, scene_size, Tier::Scene);
        let entity_arena = Arena::new(entity_base, entity_size, Tier::Entity);
        
        TieredAllocator {
            render_arena: Arc::new(Mutex::new(render_arena)),
            scene_arena: Arc::new(Mutex::new(scene_arena)),
            entity_arena: Arc::new(Mutex::new(entity_arena)),

            assets: Arc::new(Mutex::new(HashMap::new())),
            base_url: Arc::new(Mutex::new(String::new())),
            http_client: Client::new(),
        }
    }

    // Fast compact for a specific tier with intelligent growing
    pub fn fast_compact_tier(&mut self, tier: Tier, preserve_bytes: usize) -> bool {
        // Get current allocation and capacity for the specified tier
        let (current_offset, capacity) = match tier {
            Tier::Render => {
                if let Ok(arena) = self.render_arena.lock() {
                    (arena.current_offset.load(Ordering::Relaxed), arena.capacity())
                } else {
                    return false;
                }
            },
            Tier::Scene => {
                if let Ok(arena) = self.scene_arena.lock() {
                    (arena.current_offset.load(Ordering::Relaxed), arena.capacity())
                } else {
                    return false;
                }
            },
            Tier::Entity => {
                if let Ok(arena) = self.entity_arena.lock() {
                    (arena.current_offset.load(Ordering::Relaxed), arena.capacity())
                } else {
                    return false;
                }
            },
        };
        
        // If we need more space than currently allocated
        if preserve_bytes > current_offset {
            // Check if the requested size exceeds our capacity
            if preserve_bytes > capacity {
                // We need to grow the heap, but first check if it's feasible
                
                // Get total WebAssembly memory size (can't exceed 4GB in wasm32)
                let total_current_pages = core::arch::wasm32::memory_size(0);
                let max_pages = 65536; // Max 4GB (65536 pages * 64KB per page)
                
                // Calculate how many more pages we need
                let additional_bytes_needed = preserve_bytes - current_offset;
                let additional_pages_needed = (additional_bytes_needed + 65535) / 65536;
                
                // Check if growing would exceed the 4GB limit
                if total_current_pages + additional_pages_needed > max_pages {
                    #[cfg(target_arch = "wasm32")]
                    {
                        web_sys::console::log_1(&format!(
                            "Cannot grow memory - would exceed 4GB limit. Current pages: {}, needed: {}, max: {}",
                            total_current_pages, additional_pages_needed, max_pages
                        ).into());
                    }
                    return false;
                }
                
                // Try to grow the heap
                #[cfg(target_arch = "wasm32")]
                {   
                    web_sys::console::log_1(&format!(
                        "Growing heap for tier {:?} compact - current: {}, preserve: {}, growing by: {} pages",
                        tier, current_offset, preserve_bytes, additional_pages_needed
                    ).into());
                }
                
                // Create temporary storage to hold data we want to preserve
                let preserve_data = if current_offset > 0 {
                    // Get a reference to the arena to copy data from
                    let arena_ref = match tier {
                        Tier::Render => self.render_arena.clone(),
                        Tier::Scene => self.scene_arena.clone(),
                        Tier::Entity => self.entity_arena.clone(),
                    };
                    
                    // Copy the data we want to preserve
                    if let Ok(arena) = arena_ref.lock() {
                        // Only copy what's currently allocated (not what we'll grow to)
                        let bytes_to_copy = current_offset.min(preserve_bytes);
                        let mut data = Vec::with_capacity(bytes_to_copy);
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                arena.base,
                                data.as_mut_ptr(),
                                bytes_to_copy
                            );
                            data.set_len(bytes_to_copy);
                        }
                        Some(data)
                    } else {
                        None
                    }
                } else {
                    None
                };
                
                // Grow the heap
                let new_mem = self.grow_heap(additional_bytes_needed, tier);
                if new_mem.is_null() {
                    #[cfg(target_arch = "wasm32")]
                    {   
                        web_sys::console::log_1(&JsValue::from_str("Failed to grow memory for compact operation"));
                    }
                    return false;
                }
                
                // Copy preserved data to the new arena if needed
                if let Some(data) = preserve_data {
                    match tier {
                        Tier::Render => {
                            if let Ok(arena) = self.render_arena.lock() {
                                unsafe {
                                    std::ptr::copy_nonoverlapping(
                                        data.as_ptr(),
                                        arena.base,
                                        data.len()
                                    );
                                }
                                // Set the current offset to include our preserved data
                                arena.current_offset.store(data.len(), Ordering::SeqCst);
                            }
                        },
                        Tier::Scene => {
                            if let Ok(arena) = self.scene_arena.lock() {
                                unsafe {
                                    std::ptr::copy_nonoverlapping(
                                        data.as_ptr(),
                                        arena.base,
                                        data.len()
                                    );
                                }
                                arena.current_offset.store(data.len(), Ordering::SeqCst);
                            }
                        },
                        Tier::Entity => {
                            if let Ok(arena) = self.entity_arena.lock() {
                                unsafe {
                                    std::ptr::copy_nonoverlapping(
                                        data.as_ptr(),
                                        arena.base,
                                        data.len()
                                    );
                                }
                                arena.current_offset.store(data.len(), Ordering::SeqCst);
                            }
                        },
                    }
                }
                
                // Now ensure the offset is correctly set to preserve_bytes
                match tier {
                    Tier::Render => {
                        if let Ok(arena) = self.render_arena.lock() {
                            arena.current_offset.store(preserve_bytes, Ordering::SeqCst);
                        }
                    },
                    Tier::Scene => {
                        if let Ok(arena) = self.scene_arena.lock() {
                            arena.current_offset.store(preserve_bytes, Ordering::SeqCst);
                        }
                    },
                    Tier::Entity => {
                        if let Ok(arena) = self.entity_arena.lock() {
                            arena.current_offset.store(preserve_bytes, Ordering::SeqCst);
                        }
                    },
                }
                
                return true; // Successfully grew and preserved
            } else {
                // We have enough capacity, just need to allocate up to preserve_bytes
                match tier {
                    Tier::Render => {
                        if let Ok(arena) = self.render_arena.lock() {
                            // Set the current offset to preserve_bytes
                            arena.current_offset.store(preserve_bytes, Ordering::SeqCst);
                            return true;
                        }
                    },
                    Tier::Scene => {
                        if let Ok(arena) = self.scene_arena.lock() {
                            arena.current_offset.store(preserve_bytes, Ordering::SeqCst);
                            return true;
                        }
                    },
                    Tier::Entity => {
                        if let Ok(arena) = self.entity_arena.lock() {
                            arena.current_offset.store(preserve_bytes, Ordering::SeqCst);
                            return true;
                        }
                    },
                }
            }
        } else {
            // Current allocation is sufficient, proceed with normal compact
            match tier {
                Tier::Render => {
                    if let Ok(arena) = self.render_arena.lock() {
                        return arena.fast_compact(preserve_bytes);
                    }
                },
                Tier::Scene => {
                    if let Ok(arena) = self.scene_arena.lock() {
                        return arena.fast_compact(preserve_bytes);
                    }
                },
                Tier::Entity => {
                    if let Ok(arena) = self.entity_arena.lock() {
                        return arena.fast_compact(preserve_bytes);
                    }
                },
            }
        }
        
        false
    }

    // Grow heap for a specific tier - exact allocation, no overhead
    pub fn grow_heap(&mut self, size_needed: usize, tier: Tier) -> *mut u8 {
        // Calculate how many WebAssembly pages we need (64KiB per page)
        let pages_needed = (size_needed + 65535) / 65536;
        
        // Try to grow memory
        let old_pages = core::arch::wasm32::memory_grow(0, pages_needed);
        if old_pages == usize::MAX {
            // Failed to grow memory - log failure
            return std::ptr::null_mut();
        }
        
        // We successfully grew the memory
        let new_block_size = pages_needed * 65536;
        
        // Calculate the base address for the new memory
        let new_memory_base = (old_pages * 65536) as *mut u8;
        
        // Create a new arena for the specific tier
        let new_arena = Arena::new(new_memory_base, new_block_size, tier);
        
        // Based on the tier, update or replace the corresponding arena
        match tier {
            Tier::Render => {
                if let Ok(mut old_arena) = self.render_arena.lock() {
                    *old_arena = new_arena;
                }
            },
            Tier::Scene => {
                if let Ok(mut old_arena) = self.scene_arena.lock() {
                    *old_arena = new_arena;
                }
            },
            Tier::Entity => {
                if let Ok(mut old_arena) = self.entity_arena.lock() {
                    *old_arena = new_arena;
                }
            },
        }
        
        // Return a non-null pointer to indicate success
        // The actual allocation will happen in the caller
        new_memory_base
    }
    
    pub fn allocate_with_owner(&mut self, size: usize, tier: Tier) -> Option<(MemoryOwner, *mut u8)> {
        let arena = match tier {
            Tier::Render => &self.render_arena,
            Tier::Scene => &self.scene_arena,
            Tier::Entity => &self.entity_arena,
        };
        
        // Try to allocate from the selected arena
        if let Ok(arena_lock) = arena.lock() {
            if let Some((ptr, alloc_size)) = arena_lock.allocate(size) {
                // Create a memory owner for this allocation
                let offset = (ptr as usize) - (arena_lock.base as usize);
                let owner = MemoryOwner {
                    arena: Arc::clone(arena),
                    allocations: vec![(offset, alloc_size)],
                };
                
                return Some((owner, ptr));
            }
        }
        
        // If the arena allocation failed, try to grow the heap
        // First grow the heap
        let ptr = self.grow_heap(size, tier);
        
        // If growth failed, return None
        if ptr.is_null() {
            return None;
        }
        
        // Try allocation again with the newly expanded arena
        let arena = match tier {
            Tier::Render => &self.render_arena,
            Tier::Scene => &self.scene_arena,
            Tier::Entity => &self.entity_arena,
        };
        
        // Try to allocate from the selected arena after growing
        if let Ok(arena_lock) = arena.lock() {
            if let Some((new_ptr, alloc_size)) = arena_lock.allocate(size) {
                // Create a memory owner for this allocation
                let offset = (new_ptr as usize) - (arena_lock.base as usize);
                let owner = MemoryOwner {
                    arena: Arc::clone(arena),
                    allocations: vec![(offset, alloc_size)],
                };
                
                return Some((owner, new_ptr));
            }
        }
        
        // If allocation still fails after growing, return None, we're out of memory.
        None
    }
    
    pub fn allocate(&mut self, size: usize, tier: Tier) -> *mut u8 {
        // First attempt: try to allocate from the selected arena
        let arena = match tier {
            Tier::Render => &self.render_arena,
            Tier::Scene => &self.scene_arena,
            Tier::Entity => &self.entity_arena,
        };
        
        if let Ok(arena_lock) = arena.lock() {
            if let Some((ptr, _)) = arena_lock.allocate(size) {
                return ptr; // Allocation succeeded
            }
        }
        
        // First attempt failed - try to grow the heap
        let ptr = self.grow_heap(size, tier);
        
        // If growth succeeded, try allocation again
        if !ptr.is_null() {
            let arena = match tier {
                Tier::Render => &self.render_arena,
                Tier::Scene => &self.scene_arena,
                Tier::Entity => &self.entity_arena,
            };
            
            if let Ok(arena_lock) = arena.lock() {
                if let Some((new_ptr, _)) = arena_lock.allocate(size) {
                    return new_ptr;
                }
            }
        } else {
            // Growth failed - try recycling and then allocating
            
            // Get current stats for this tier to determine how much we're using
            let (current_usage, _, _, _) = match tier {
                Tier::Render => {
                    if let Ok(arena) = self.render_arena.lock() {
                        arena.get_stats()
                    } else {
                        (0, 0, 0, 0)
                    }
                },
                Tier::Scene => {
                    if let Ok(arena) = self.scene_arena.lock() {
                        arena.get_stats()
                    } else {
                        (0, 0, 0, 0)
                    }
                },
                Tier::Entity => {
                    if let Ok(arena) = self.entity_arena.lock() {
                        arena.get_stats()
                    } else {
                        (0, 0, 0, 0)
                    }
                },
            };
            
            // If we're using enough memory that recycling might help
            if current_usage > size {
                web_sys::console::log_1(&format!(
                    "Growth failed, attempting to reset tier {:?} completely to make space",
                    tier
                ).into());
                
                // Reset this tier completely - clearer than preserving 0 bytes
                self.reset_tier(tier);
                
                // Try allocation again after resetting
                let arena = match tier {
                    Tier::Render => &self.render_arena,
                    Tier::Scene => &self.scene_arena,
                    Tier::Entity => &self.entity_arena,
                };
                
                if let Ok(arena_lock) = arena.lock() {
                    if let Some((new_ptr, _)) = arena_lock.allocate(size) {
                        return new_ptr; // Allocation succeeded after resetting
                    }
                }
            }
        }
        
        // If all attempts fail, return null
        std::ptr::null_mut()
    }
    
    // Check if pointer is in any arena
    pub fn is_ptr_in_arena(&self, ptr: *mut u8) -> bool {
        if let Ok(arena) = self.render_arena.lock() {
            if arena.contains(ptr) {
                return true;
            }
        }
        
        if let Ok(arena) = self.scene_arena.lock() {
            if arena.contains(ptr) {
                return true;
            }
        }
        
        if let Ok(arena) = self.entity_arena.lock() {
            if arena.contains(ptr) {
                return true;
            }
        }
        
        false
    }
    
    // Reset a specific tier
    pub fn reset_tier(&mut self, tier: Tier) {
        match tier {
            Tier::Render => {
                if let Ok(arena) = self.render_arena.lock() {
                    arena.reset();
                }
            },
            Tier::Scene => {
                if let Ok(arena) = self.scene_arena.lock() {
                    arena.reset();
                }
            },
            Tier::Entity => {
                if let Ok(arena) = self.entity_arena.lock() {
                    arena.reset();
                }
            },
        }
    }
    
    pub fn tier_stats(&self, tier: Tier) -> (usize, usize, usize, usize) {
        match tier {
            Tier::Render => {
                if let Ok(arena) = self.render_arena.lock() {
                    arena.get_stats()
                } else {
                    (0, 0, 0, 0)
                }
            },
            Tier::Scene => {
                if let Ok(arena) = self.scene_arena.lock() {
                    arena.get_stats()
                } else {
                    (0, 0, 0, 0)
                }
            },
            Tier::Entity => {
                if let Ok(arena) = self.entity_arena.lock() {
                    arena.get_stats()
                } else {
                    (0, 0, 0, 0)
                }
            },
        }
    }
    
    // Check if a pointer is valid
    pub fn is_ptr_valid(&self, ptr: *mut u8) -> bool {
        self.is_ptr_in_arena(ptr)
    }

    pub fn set_base_url(&self, url: String) {
        if let Ok(mut base_url) = self.base_url.lock() {
            *base_url = url;
        }
    }

    pub async fn load_asset(&mut self, path: String, asset_type: u8) -> Result<usize, JsValue> {
        let asset_type = match asset_type {
            0 => AssetType::Image,
            1 => AssetType::Json,
            _ => return Err(JsValue::from_str("Invalid asset type: must be 0 (Image) or 1 (Json)")),
        };

        // Get the base URL from the mutex
        let full_url = {
            let base_url = match self.base_url.lock() {
                Ok(guard) => guard.clone(),
                Err(_) => return Err(JsValue::from_str("Failed to lock base_url")),
            };
            format!("{}{}", base_url, path)
        };
        
        web_sys::console::log_1(&format!("Loading asset from: {}", full_url).into());

        // Fetch the asset
        let response = match self.http_client.get(&full_url).send().await {
            Ok(resp) => resp,
            Err(e) => return Err(JsValue::from_str(&format!("Failed to fetch: {}", e))),
        };
        
        if !response.status().is_success() {
            return Err(JsValue::from_str(&format!(
                "HTTP error: {} for {}", 
                response.status(), 
                full_url
            )));
        }

        // Get the bytes
        let bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => return Err(JsValue::from_str(&format!("Failed to get bytes: {}", e))),
        };

        let data_size = bytes.len();

        // Allocate memory in the Scene tier
        let ptr = self.allocate(data_size, Tier::Scene);
        
        if ptr.is_null() {
            return Err(JsValue::from_str("Failed to allocate memory for asset"));
        }
        
        // Calculate offset from memory base
        let memory_base = self.get_memory_base(Tier::Scene);
        let offset = (ptr as usize) - (memory_base as usize);
        
        // Copy bytes into memory
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, data_size);
        }

        // Save metadata
        if let Ok(mut assets) = self.assets.lock() {
            assets.insert(
                path.clone(),
                AssetMetadata {
                    asset_type,
                    size: data_size,
                    offset,
                },
            );
        } else {
            return Err(JsValue::from_str("Failed to acquire assets lock"));
        }

        Ok(offset)
    }

    // Get memory base pointer for a specific tier
    fn get_memory_base(&self, tier: Tier) -> *mut u8 {
        match tier {
            Tier::Render => {
                if let Ok(arena) = self.render_arena.lock() {
                    arena.base
                } else {
                    std::ptr::null_mut()
                }
            },
            Tier::Scene => {
                if let Ok(arena) = self.scene_arena.lock() {
                    arena.base
                } else {
                    std::ptr::null_mut()
                }
            },
            Tier::Entity => {
                if let Ok(arena) = self.entity_arena.lock() {
                    arena.base
                } else {
                    std::ptr::null_mut()
                }
            },
        }
    }

    pub async fn test_fetch_json(&self) -> Result<JsValue, JsValue> {
        web_sys::console::log_1(&"Testing JSON fetch".into());
        
        let test_url = "https://jsonplaceholder.typicode.com/todos/1";
        
        let response = match self.http_client.get(test_url).send().await {
            Ok(resp) => resp,
            Err(e) => return Err(JsValue::from_str(&format!("Failed to fetch: {}", e))),
        };
        
        if !response.status().is_success() {
            return Err(JsValue::from_str(&format!("HTTP error: {}", response.status())));
        }
        
        let text = match response.text().await {
            Ok(t) => t,
            Err(e) => return Err(JsValue::from_str(&format!("Failed to get text: {}", e))),
        };
        
        web_sys::console::log_1(&format!("Received JSON: {}", text).into());
        
        Ok(JsValue::from_str(&text))
    }

    pub fn evict_asset(&mut self, path: &str) -> Result<(), JsValue> {
        // First, get information about the target asset
        let target_metadata = {
            let assets_lock = match self.assets.lock() {
                Ok(lock) => lock,
                Err(_) => return Err(JsValue::from_str("Failed to acquire assets lock")),
            };
            
            // Check if the asset exists
            match assets_lock.get(path) {
                Some(meta) => meta.clone(),
                None => return Err(JsValue::from_str(&format!("Asset not found: {}", path))),
            }
        };
        
        // Get a temporary buffer for the content we want to preserve
        let (preserve_buffer, preserve_map) = {
            let assets_lock = match self.assets.lock() {
                Ok(lock) => lock,
                Err(_) => return Err(JsValue::from_str("Failed to acquire assets lock")),
            };
            
            let mut preserve_buffer = Vec::new();
            let mut preserve_map = HashMap::new();
            
            // Get memory base for Scene tier
            let memory_base = self.get_memory_base(Tier::Scene);
            
            // Copy all assets except the one being evicted
            for (asset_path, metadata) in assets_lock.iter() {
                if asset_path != path {
                    // Record the current position in our buffer
                    let new_offset = preserve_buffer.len();
                    
                    // Read the asset's bytes
                    unsafe {
                        let src_ptr = memory_base.add(metadata.offset);
                        let src_data = std::slice::from_raw_parts(src_ptr, metadata.size);
                        
                        // Append to our buffer
                        preserve_buffer.extend_from_slice(src_data);
                        
                        // Add to our map with updated offset
                        preserve_map.insert(asset_path.clone(), AssetMetadata {
                            asset_type: metadata.asset_type,
                            size: metadata.size,
                            offset: new_offset,
                        });
                    }
                }
            }
            
            (preserve_buffer, preserve_map)
        };
        
        // Now reset the entire scene tier
        self.reset_tier(Tier::Scene);
        
        // If we have assets to preserve, reallocate and copy them back
        if !preserve_buffer.is_empty() {
            // Allocate new memory for the preserved data
            let buffer_size = preserve_buffer.len();
            let ptr = self.allocate(buffer_size, Tier::Scene);
            
            if ptr.is_null() {
                return Err(JsValue::from_str("Failed to allocate memory for preserved assets"));
            }
            
            // Calculate offset
            let memory_base = self.get_memory_base(Tier::Scene);
            let offset = (ptr as usize) - (memory_base as usize);
            
            // Copy the preserved data back to WebAssembly memory
            unsafe {
                std::ptr::copy_nonoverlapping(
                    preserve_buffer.as_ptr(),
                    ptr,
                    buffer_size
                );
            }
            
            // Update the offsets in our preserve map
            let mut updated_preserve_map = HashMap::new();
            for (asset_path, metadata) in preserve_map {
                updated_preserve_map.insert(asset_path, AssetMetadata {
                    asset_type: metadata.asset_type,
                    size: metadata.size,
                    offset: offset + metadata.offset,
                });
            }
            
            // Update the assets HashMap with the preserved assets (removing the target)
            let mut assets_lock = match self.assets.lock() {
                Ok(lock) => lock,
                Err(_) => return Err(JsValue::from_str("Failed to acquire assets lock")),
            };
            
            // Clear and repopulate with preserved assets
            assets_lock.clear();
            for (asset_path, metadata) in updated_preserve_map {
                assets_lock.insert(asset_path, metadata);
            }
        } else {
            // If no assets to preserve, just clear the HashMap
            let mut assets_lock = match self.assets.lock() {
                Ok(lock) => lock,
                Err(_) => return Err(JsValue::from_str("Failed to acquire assets lock")),
            };
            assets_lock.clear();
        }
        
        web_sys::console::log_1(&format!(
            "Evicted asset: {} and freed {} bytes",
            path, target_metadata.size
        ).into());
        
        Ok(())
    }
    
    pub fn get_asset(&self, path: &str) -> Result<js_sys::Uint8Array, JsValue> {
        // Get the assets lock
        let assets_lock = match self.assets.lock() {
            Ok(lock) => lock,
            Err(_) => return Err(JsValue::from_str("Failed to acquire assets lock")),
        };
        
        // Get the metadata
        let metadata = match assets_lock.get(path) {
            Some(meta) => meta.clone(), // Clone to avoid lifetime issues
            None => return Err(JsValue::from_str(&format!("Asset not found: {}", path))),
        };
        
        // Drop assets lock before accessing memory
        drop(assets_lock);
        
        let memory_base = self.get_memory_base(Tier::Scene);
        
        unsafe {
            let ptr = memory_base.add(metadata.offset);
            let mem_slice = std::slice::from_raw_parts(ptr, metadata.size);
            Ok(js_sys::Uint8Array::from(mem_slice))
        }
    }
}

impl Clone for TieredAllocator {
    fn clone(&self) -> Self {
        TieredAllocator {
            render_arena: Arc::clone(&self.render_arena),
            scene_arena: Arc::clone(&self.scene_arena),
            entity_arena: Arc::clone(&self.entity_arena),
            assets: Arc::clone(&self.assets),
            base_url: Arc::clone(&self.base_url),
            http_client: self.http_client.clone(),
        }
    }
}

#[wasm_bindgen]
impl Walloc {
    pub fn new() -> Self {
        let memory_base = core::arch::wasm32::memory_size(0) as *mut u8;
        let memory_size = (core::arch::wasm32::memory_size(0) * 65536) as usize;

        let strategy = TieredAllocator::new(memory_base, memory_size);
        
        Walloc {
            strategy,
            memory_base,
            memory_size,
        }
    }
    
    #[wasm_bindgen]
    pub fn set_base_url(&self, url: String) -> Result<(), JsValue> {
        self.strategy.set_base_url(url);
        Ok(())
    }
    
    // Async methods return Promise
    #[wasm_bindgen]
    pub fn load_asset(&mut self, path: String, asset_type: u8) -> Promise {
        let mut allocator_clone = self.strategy.clone();
        
        future_to_promise(async move {
            match allocator_clone.load_asset(path, asset_type).await {
                Ok(offset) => Ok(JsValue::from_f64(offset as f64)),
                Err(e) => Err(e),
            }
        })
    }
    
    #[wasm_bindgen]
    pub fn test_fetch_json(&self) -> Promise {
        let allocator_clone = self.strategy.clone();
        
        future_to_promise(async move {
            allocator_clone.test_fetch_json().await
        })
    }

    #[wasm_bindgen]
    pub fn evict_asset(&mut self, path: String) -> Result<(), JsValue> {
        self.strategy.evict_asset(&path)
    }
    
    #[wasm_bindgen]
    pub fn get_asset(&self, path: String) -> Result<js_sys::Uint8Array, JsValue> {
        self.strategy.get_asset(&path)
    }
    
    // Get a direct view into WASM memory as a typed array
    #[wasm_bindgen]
    pub fn get_memory_view(&self, offset: usize, length: usize) -> Result<js_sys::Uint8Array, JsValue> {
        if offset + length > self.memory_size {
            return Err(JsValue::from_str("Memory access out of bounds"));
        }
        
        unsafe {
            let ptr = self.memory_base.add(offset);
            let mem_slice = std::slice::from_raw_parts(ptr, length);
            Ok(js_sys::Uint8Array::from(mem_slice))
        }
    }
    
    // Allocate memory from a specific tier
    #[wasm_bindgen]
    pub fn allocate_tiered(&mut self, size: usize, tier_number: u8) -> usize {
        let tier = match Tier::from_u8(tier_number) {
            Some(t) => t,
            None => Tier::Entity, // Default to Entity tier if invalid
        };

        let ptr = self.strategy.allocate(size, tier);

        self.memory_size = core::arch::wasm32::memory_size(0) * 65536;
        
        // Return offset from memory base
        if ptr.is_null() {
            0 // Error case, return 0 (null) pointer
        } else {
            (ptr as usize) - (self.memory_base as usize)
        }
    }

    #[wasm_bindgen]
    pub fn fast_compact_tier(&mut self, tier_number: u8, preserve_bytes: usize) -> bool {
        let tier = match Tier::from_u8(tier_number) {
            Some(t) => t,
            None => return false,
        };
        
        self.strategy.fast_compact_tier(tier, preserve_bytes)
    }
    
    // Reset a specific tier
    #[wasm_bindgen]
    pub fn reset_tier(&mut self, tier_number: u8) -> bool {
        let tier = match Tier::from_u8(tier_number) {
            Some(t) => t,
            None => return false,
        };
        
        self.strategy.reset_tier(tier);
        true
    }

    // Copy data from JS to WASM memory
    #[wasm_bindgen]
    pub fn copy_from_js(&mut self, offset: usize, data: &js_sys::Uint8Array) -> Result<(), JsValue> {
        let data_len = data.length() as usize;
        if offset + data_len > self.memory_size {
            return Err(JsValue::from_str("Memory access out of bounds"));
        }
        
        unsafe {
            let dest_ptr = self.memory_base.add(offset);
            let dest_slice = std::slice::from_raw_parts_mut(dest_ptr, data_len);
            data.copy_to(dest_slice);
            Ok(())
        }
    }
    
    // Copy data from WASM memory to JS
    #[wasm_bindgen]
    pub fn copy_to_js(&self, offset: usize, length: usize) -> Result<js_sys::Uint8Array, JsValue> {
        self.get_memory_view(offset, length)
    }
    
    // Memory statistics
    #[wasm_bindgen]
    pub fn memory_stats(&self) -> js_sys::Object {
        let obj = js_sys::Object::new();
        
        // Get current memory size from WebAssembly directly
        let current_pages = core::arch::wasm32::memory_size(0);
        let current_size = current_pages * 65536;
        
        // Track total in-use memory
        let mut total_in_use = 0;
        
        // Add tier information
        let tiers = js_sys::Array::new();
        
        for tier_num in 0..3 {
            if let Some(tier) = Tier::from_u8(tier_num) {
                let (used, capacity, high_water, total_allocated) = self.strategy.tier_stats(tier);
                let tier_obj = js_sys::Object::new();
                
                // Add current usage to total
                total_in_use += used;
                
                js_sys::Reflect::set(
                    &tier_obj,
                    &JsValue::from_str("name"),
                    &JsValue::from_str(match tier {
                        Tier::Render => "render",
                        Tier::Scene => "scene",
                        Tier::Entity => "entity",
                    })
                ).unwrap();
                
                js_sys::Reflect::set(
                    &tier_obj,
                    &JsValue::from_str("used"),
                    &JsValue::from_f64(used as f64)
                ).unwrap();
                
                js_sys::Reflect::set(
                    &tier_obj,
                    &JsValue::from_str("capacity"),
                    &JsValue::from_f64(capacity as f64)
                ).unwrap();
                
                js_sys::Reflect::set(
                    &tier_obj,
                    &JsValue::from_str("highWaterMark"),
                    &JsValue::from_f64(high_water as f64)
                ).unwrap();
                
                js_sys::Reflect::set(
                    &tier_obj,
                    &JsValue::from_str("totalAllocated"),
                    &JsValue::from_f64(total_allocated as f64)
                ).unwrap();
                
                // Calculate memory savings
                let saved = if total_allocated > used {
                    total_allocated - used
                } else {
                    0
                };
                
                js_sys::Reflect::set(
                    &tier_obj,
                    &JsValue::from_str("memorySaved"),
                    &JsValue::from_f64(saved as f64)
                ).unwrap();
                
                tiers.push(&tier_obj);
            }
        }
        
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("tiers"),
            &tiers
        ).unwrap();
        
        // Set the total size to the in-use memory (not just raw WASM memory size)
        js_sys::Reflect::set(
            &obj, 
            &JsValue::from_str("totalSize"), 
            &JsValue::from_f64(total_in_use as f64)
        ).unwrap();
        
        // Add raw memory pages info
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("pages"),
            &JsValue::from_f64(current_pages as f64)
        ).unwrap();
        
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("rawMemorySize"),
            &JsValue::from_f64(current_size as f64)
        ).unwrap();
        
        // Add allocator type
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("allocatorType"),
            &JsValue::from_str("tiered")
        ).unwrap();
        
        // Add useful utilization percentage
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("memoryUtilization"),
            &JsValue::from_f64((total_in_use as f64 / current_size as f64) * 100.0)
        ).unwrap();
        
        obj
    }
}