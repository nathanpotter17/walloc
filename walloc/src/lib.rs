//! # Walloc - Cohesive High-Performance Memory Allocator (ENHANCED)
//! 
//! Enhanced with WASM-inspired optimizations for better memory management

use std::sync::atomic::{AtomicUsize, AtomicPtr, AtomicU64, Ordering};
use std::collections::HashMap;
use std::sync::{Arc, RwLock, Weak};
use reqwest::Client;
use futures::stream::{self, StreamExt};

// SIMD imports
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
use std::arch::x86_64::*;
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
use std::arch::wasm32::*;

// WASM-specific imports
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::future_to_promise;
#[cfg(target_arch = "wasm32")]
use js_sys::Promise;

// ================================
// === CORE CONSTANTS ===
// ================================

const CACHE_LINE_SIZE: usize = 64;
const SIMD_ALIGNMENT: usize = 32;
const PARALLEL_LOAD_FACTOR: usize = 8;

// Platform-specific memory limits
#[cfg(target_arch = "wasm32")]
const MAX_MEMORY_LIMIT: usize = usize::MAX; // Maximum addressable on 32-bit

#[cfg(not(target_arch = "wasm32"))]
const MAX_MEMORY_LIMIT: usize = 4 * 1024 * 1024 * 1024; // 4GB limit

#[cfg(not(target_arch = "wasm32"))]
static mut GLOBAL_MEMORY_BASE: *mut u8 = std::ptr::null_mut();

// ================================
// === CORE TYPES ===
// ================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Tier {
    Top = 0,     // GPU/Render: 50% memory, 128-byte aligned
    Middle = 1,  // Scene/Assets: 30% memory, 64-byte aligned
    Bottom = 2,  // Temporary: 20% memory, 8-byte aligned
}

impl Tier {
    #[inline(always)]
    pub fn from_u8(value: u8) -> Option<Tier> {
        match value {
            0 => Some(Tier::Top),
            1 => Some(Tier::Middle),
            2 => Some(Tier::Bottom),
            _ => None,
        }
    }

    #[inline(always)]
    pub fn alignment(&self) -> usize {
        match self {
            Tier::Top => 128,
            Tier::Middle => 64,
            Tier::Bottom => 8,
        }
    }
    
    #[inline(always)]
    pub fn memory_percentage(&self) -> usize {
        match self {
            Tier::Top => 50,
            Tier::Middle => 30,
            Tier::Bottom => 20,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum AssetType {
    Image = 0,
    Json = 1,
    Binary = 2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryHandle(usize);

impl MemoryHandle {
    #[inline(always)]
    pub fn to_ptr(self) -> *mut u8 {
        if self.is_null() {
            return std::ptr::null_mut();
        }
        
        #[cfg(target_arch = "wasm32")]
        { 
            self.0 as *mut u8 
        }
        
        #[cfg(not(target_arch = "wasm32"))]
        { 
            unsafe { 
                if GLOBAL_MEMORY_BASE.is_null() {
                    return std::ptr::null_mut();
                }
                GLOBAL_MEMORY_BASE.add(self.0) 
            } 
        }
    }
    
    #[inline(always)]
    pub fn from_ptr(ptr: *mut u8) -> Self {
        if ptr.is_null() {
            return MemoryHandle::null();
        }
        
        #[cfg(target_arch = "wasm32")]
        { 
            MemoryHandle(ptr as usize) 
        }
        
        #[cfg(not(target_arch = "wasm32"))]
        { 
            let offset = unsafe { ptr.offset_from(GLOBAL_MEMORY_BASE) as usize };
            MemoryHandle(offset)
        }
    }
    
    #[inline(always)]
    pub fn offset(self) -> usize { self.0 }
    
    #[inline(always)]
    pub fn is_null(self) -> bool { self.0 == usize::MAX }
    
    #[inline(always)]
    pub fn null() -> Self { MemoryHandle(usize::MAX) }
    
    #[inline(always)]
    pub fn advance(self, offset: usize) -> Self {
        MemoryHandle(self.0.wrapping_add(offset))
    }
}

// ================================
// === MEMORY OWNER TRACKING ===
// ================================

pub struct MemoryOwner {
    arena_index: usize,
    allocations: Vec<(MemoryHandle, usize)>, // (handle, size) pairs
    walloc: Weak<Walloc>,
}

impl MemoryOwner {
    fn new(arena_index: usize, walloc: Weak<Walloc>) -> Self {
        Self {
            arena_index,
            allocations: Vec::new(),
            walloc,
        }
    }
    
    fn add_allocation(&mut self, handle: MemoryHandle, size: usize) {
        self.allocations.push((handle, size));
    }
    
    pub fn total_size(&self) -> usize {
        self.allocations.iter().map(|(_, size)| size).sum()
    }
}

impl Drop for MemoryOwner {
    fn drop(&mut self) {
        if let Some(walloc) = self.walloc.upgrade() {
            let arena = &walloc.arenas[self.arena_index];
            
            // Deallocate all owned allocations
            for &(handle, size) in &self.allocations {
                arena.deallocate(handle, size);
            }
            
            #[cfg(target_arch = "wasm32")]
            {
                // On WASM, trigger a compaction if we freed significant memory
                // This is done after deallocation to potentially reclaim fragmented space
                let total_freed = self.total_size();
                
                // Only compact if we freed more than 64KB
                if total_freed > 65536 {
                    let tier = match self.arena_index {
                        0 => Tier::Top,
                        1 => Tier::Middle,
                        2 => Tier::Bottom,
                        _ => return,
                    };
                    
                    // Get current usage to preserve existing allocations
                    let current_usage = arena.usage();
                    
                    // Fast compact to current usage level (preserving all current allocations)
                    walloc.fast_compact_tier(tier, current_usage);
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct AssetMetadata {
    pub asset_type: AssetType,
    pub size: usize,
    pub offset: usize,
    pub tier: Tier,
    pub handle: MemoryHandle,
}

// ================================
// === VECTORIZED SIMD OPERATIONS ===
// ================================

pub struct SIMDOps;

impl SIMDOps {
    #[inline(always)]
    pub unsafe fn fast_copy(src: *const u8, dst: *mut u8, len: usize) {
        // Optimize for common sizes first
        match len {
            0 => return,
            1..=8 => {
                if len >= 4 {
                    let v = (src as *const u32).read_unaligned();
                    (dst as *mut u32).write_unaligned(v);
                    let v = (src.add(len - 4) as *const u32).read_unaligned();
                    (dst.add(len - 4) as *mut u32).write_unaligned(v);
                } else {
                    std::ptr::copy_nonoverlapping(src, dst, len);
                }
            }
            9..=16 => {
                let v = (src as *const u64).read_unaligned();
                (dst as *mut u64).write_unaligned(v);
                let v = (src.add(len - 8) as *const u64).read_unaligned();
                (dst.add(len - 8) as *mut u64).write_unaligned(v);
            }
            17..=32 => {
                let v1 = (src as *const u128).read_unaligned();
                let v2 = (src.add(len - 16) as *const u128).read_unaligned();
                (dst as *mut u128).write_unaligned(v1);
                (dst.add(len - 16) as *mut u128).write_unaligned(v2);
            }
            _ => Self::copy_vectorized(src, dst, len),
        }
    }

    #[inline(never)]
    unsafe fn copy_vectorized(src: *const u8, dst: *mut u8, len: usize) {
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            if len > 4096 {
                _mm_prefetch(src as *const i8, _MM_HINT_T0);
                _mm_prefetch(src.add(64) as *const i8, _MM_HINT_T0);
            }
            
            let mut offset = 0;
            while offset + 128 <= len {
                let v0 = _mm256_loadu_si256(src.add(offset) as *const __m256i);
                let v1 = _mm256_loadu_si256(src.add(offset + 32) as *const __m256i);
                let v2 = _mm256_loadu_si256(src.add(offset + 64) as *const __m256i);
                let v3 = _mm256_loadu_si256(src.add(offset + 96) as *const __m256i);
                
                _mm256_storeu_si256(dst.add(offset) as *mut __m256i, v0);
                _mm256_storeu_si256(dst.add(offset + 32) as *mut __m256i, v1);
                _mm256_storeu_si256(dst.add(offset + 64) as *mut __m256i, v2);
                _mm256_storeu_si256(dst.add(offset + 96) as *mut __m256i, v3);
                
                offset += 128;
            }
            
            if offset < len {
                let remaining = len - offset;
                if remaining >= 32 {
                    let chunks = remaining / 32;
                    for _ in 0..chunks {
                        let v = _mm256_loadu_si256(src.add(offset) as *const __m256i);
                        _mm256_storeu_si256(dst.add(offset) as *mut __m256i, v);
                        offset += 32;
                    }
                }
                if offset < len {
                    Self::fast_copy(src.add(offset), dst.add(offset), len - offset);
                }
            }
        }
        
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            let mut offset = 0;
            while offset + 64 <= len {
                let v0 = v128_load(src.add(offset) as *const v128);
                let v1 = v128_load(src.add(offset + 16) as *const v128);
                let v2 = v128_load(src.add(offset + 32) as *const v128);
                let v3 = v128_load(src.add(offset + 48) as *const v128);
                
                v128_store(dst.add(offset) as *mut v128, v0);
                v128_store(dst.add(offset + 16) as *mut v128, v1);
                v128_store(dst.add(offset + 32) as *mut v128, v2);
                v128_store(dst.add(offset + 48) as *mut v128, v3);
                
                offset += 64;
            }
            
            while offset + 16 <= len {
                let v = v128_load(src.add(offset) as *const v128);
                v128_store(dst.add(offset) as *mut v128, v);
                offset += 16;
            }
            
            if offset < len {
                Self::fast_copy(src.add(offset), dst.add(offset), len - offset);
            }
        }
        
        #[cfg(not(any(
            all(target_arch = "x86_64", target_feature = "avx2"),
            all(target_arch = "wasm32", target_feature = "simd128")
        )))]
        {
            std::ptr::copy_nonoverlapping(src, dst, len);
        }
    }
    
    pub unsafe fn bulk_copy_optimized(operations: &[(MemoryHandle, MemoryHandle, usize)]) {
        if operations.is_empty() {
            return;
        }
        
        for &(src, dst, len) in operations {
            if len == 0 || src.is_null() || dst.is_null() {
                continue;
            }
            
            let src_ptr = src.to_ptr();
            let dst_ptr = dst.to_ptr();
            
            if !src_ptr.is_null() && !dst_ptr.is_null() {
                Self::fast_copy(src_ptr, dst_ptr, len);
            }
        }
    }
}

// ================================
// === LOCK-FREE ARENA ALLOCATOR ===
// ================================

#[repr(C)]
struct FreeNode {
    next: *mut FreeNode,
    size: usize,
}

#[repr(C, align(64))]
pub struct LockFreeArena {
    base_offset: usize,
    size: AtomicUsize,
    allocation_head: AtomicUsize,
    freelists: [AtomicPtr<FreeNode>; 8],
    tier: Tier,
    allocated: AtomicUsize,
    peak_usage: AtomicUsize,
    allocation_count: AtomicUsize,
    // Enhanced tracking from WASM version
    high_water_mark: AtomicUsize,
    total_allocated: AtomicUsize,
}

unsafe impl Send for LockFreeArena {}
unsafe impl Sync for LockFreeArena {}

#[inline(always)]
fn size_class_for(size: usize) -> usize {
    (size.max(32).trailing_zeros() as usize).saturating_sub(5).min(7)
}

impl LockFreeArena {
    pub fn new(base: *mut u8, size: usize, tier: Tier, memory_base: *mut u8) -> Self {
        let aligned_base = {
            let offset = (base as usize + CACHE_LINE_SIZE - 1) & !(CACHE_LINE_SIZE - 1);
            offset as *mut u8
        };
        let adj_size = size.saturating_sub((aligned_base as usize) - (base as usize));

        let base_offset = unsafe { aligned_base.offset_from(memory_base) as usize };

        Self {
            base_offset,
            size: AtomicUsize::new(adj_size),
            allocation_head: AtomicUsize::new(0),
            freelists: Default::default(),
            tier,
            allocated: AtomicUsize::new(0),
            peak_usage: AtomicUsize::new(0),
            allocation_count: AtomicUsize::new(0),
            high_water_mark: AtomicUsize::new(0),
            total_allocated: AtomicUsize::new(0),
        }
    }
    
    #[inline(always)]
    pub fn allocate(&self, size: usize) -> Option<usize> {
        let aligned_size = self.align_size(size);
        
        let size_class = size_class_for(aligned_size);
        if size_class < 8 {
            let freelist = &self.freelists[size_class];
            let head = freelist.load(Ordering::Acquire);
            
            if !head.is_null() {
                let next = unsafe { (*head).next };
                if freelist.compare_exchange_weak(
                    head, next, Ordering::Release, Ordering::Acquire
                ).is_ok() {
                    #[cfg(target_arch = "wasm32")]
                    return Some(head as usize);
                    
                    #[cfg(not(target_arch = "wasm32"))]
                    return Some(unsafe { (head as *const u8).offset_from(GLOBAL_MEMORY_BASE) as usize });
                }
            }
        }
        
        let mut arena_offset = self.allocation_head.load(Ordering::Relaxed);
        let arena_size = self.size.load(Ordering::Relaxed);
        
        loop {
            let new_offset = arena_offset + aligned_size;
            if new_offset > arena_size {
                return None;
            }
            
            match self.allocation_head.compare_exchange_weak(
                arena_offset,
                new_offset,
                Ordering::Relaxed,
                Ordering::Relaxed
            ) {
                Ok(_) => {
                    self.allocated.fetch_add(aligned_size, Ordering::Relaxed);
                    self.allocation_count.fetch_add(1, Ordering::Relaxed);
                    self.total_allocated.fetch_add(aligned_size, Ordering::Relaxed);
                    
                    let current_peak = self.peak_usage.load(Ordering::Relaxed);
                    if new_offset > current_peak {
                        let _ = self.peak_usage.compare_exchange_weak(
                            current_peak, new_offset, 
                            Ordering::Relaxed, Ordering::Relaxed
                        );
                    }
                    
                    let hwm = self.high_water_mark.load(Ordering::Relaxed);
                    if new_offset > hwm {
                        self.high_water_mark.store(new_offset, Ordering::Relaxed);
                    }
                    
                    return Some(self.base_offset + arena_offset);
                }
                Err(current) => arena_offset = current,
            }
        }
    }
    
    #[inline(always)]
    fn align_size(&self, size: usize) -> usize {
        let alignment = self.tier.alignment().max(SIMD_ALIGNMENT);
        (size + alignment - 1) & !(alignment - 1)
    }
    
    pub fn capacity(&self) -> usize {
        self.size.load(Ordering::Relaxed)
    }
    
    pub fn usage(&self) -> usize {
        self.allocation_head.load(Ordering::Relaxed)
    }
    
    pub fn base_ptr(&self) -> *mut u8 {
        #[cfg(target_arch = "wasm32")]
        { self.base_offset as *mut u8 }
        
        #[cfg(not(target_arch = "wasm32"))]
        { unsafe { GLOBAL_MEMORY_BASE.add(self.base_offset) } }
    }

    #[inline(always)]
    pub fn deallocate(&self, handle: MemoryHandle, size: usize) -> bool {
        if handle.is_null() {
            return false;
        }
        
        let handle_offset = handle.offset();
        if handle_offset < self.base_offset || 
        handle_offset >= self.base_offset + self.size.load(Ordering::Relaxed) {
            return false;
        }
        
        let local_offset = handle_offset - self.base_offset;
        let aligned_size = self.align_size(size);
        
        if aligned_size < std::mem::size_of::<FreeNode>() {
            self.allocated.fetch_sub(aligned_size, Ordering::Relaxed);
            self.allocation_count.fetch_sub(1, Ordering::Relaxed);
            return true;
        }
        
        let node_ptr = handle.to_ptr() as *mut FreeNode;
        
        let size_class = (aligned_size.max(8).trailing_zeros() as usize).min(7).saturating_sub(3);
        let freelist = &self.freelists[size_class];
        
        loop {
            let current_head = freelist.load(Ordering::Acquire);
            
            unsafe { 
                std::ptr::write(node_ptr, FreeNode {
                    next: current_head,
                    size: aligned_size,
                });
            }
            
            if freelist.compare_exchange_weak(
                current_head, node_ptr, Ordering::Release, Ordering::Relaxed
            ).is_ok() {
                self.allocated.fetch_sub(aligned_size, Ordering::Relaxed);
                self.allocation_count.fetch_sub(1, Ordering::Relaxed);
                return true;
            }
        }
    }
    
    pub fn reset(&self) {
        self.allocation_head.store(0, Ordering::SeqCst);
        for freelist in &self.freelists {
            freelist.store(std::ptr::null_mut(), Ordering::SeqCst);
        }
        self.allocated.store(0, Ordering::SeqCst);
    }
    
    pub fn stats(&self) -> (usize, usize, usize, usize) {
        (
            self.usage(),
            self.capacity(),
            self.peak_usage.load(Ordering::Relaxed),
            self.allocated.load(Ordering::Relaxed),
        )
    }
    
    #[cfg(target_arch = "wasm32")]
    pub unsafe fn extend_capacity(&self, new_size: usize) {
        self.size.store(new_size, Ordering::SeqCst);
    }
    
    // Enhanced: Fast compact with preservation
    pub fn fast_compact(&self, preserve_bytes: usize) -> bool {
        let current_offset = self.allocation_head.load(Ordering::Relaxed);
        
        if preserve_bytes > current_offset {
            return false;
        }
        
        self.allocation_head.store(preserve_bytes, Ordering::SeqCst);
        
        // Clear freelists as they may point to memory beyond preserve_bytes
        for freelist in &self.freelists {
            freelist.store(std::ptr::null_mut(), Ordering::SeqCst);
        }
        
        true
    }
}

// ================================
// === SIMPLE ASSET REGISTRY ===
// ================================

#[derive(Default)]
pub struct SimpleAssetRegistry {
    assets: RwLock<HashMap<String, AssetMetadata>>,
}

impl SimpleAssetRegistry {
    pub fn new() -> Self {
        Self {
            assets: RwLock::new(HashMap::with_capacity(256)),
        }
    }
    
    pub fn insert(&self, key: String, metadata: AssetMetadata) -> bool {
        let mut assets = self.assets.write().unwrap();
        assets.insert(key, metadata).is_none()
    }
    
    pub fn get(&self, key: &str) -> Option<AssetMetadata> {
        let assets = self.assets.read().unwrap();
        assets.get(key).cloned()
    }
    
    pub fn remove(&self, key: &str) -> bool {
        let mut assets = self.assets.write().unwrap();
        assets.remove(key).is_some()
    }
    
    pub fn remove_batch(&self, keys: &[String]) -> usize {
        let mut assets = self.assets.write().unwrap();
        let mut count = 0;
        
        for key in keys {
            if assets.remove(key).is_some() {
                count += 1;
            }
        }
        
        count
    }
    
    pub fn clear(&self) {
        let mut assets = self.assets.write().unwrap();
        assets.clear();
    }
    
    pub fn len(&self) -> usize {
        let assets = self.assets.read().unwrap();
        assets.len()
    }
    
    pub fn is_empty(&self) -> bool {
        let assets = self.assets.read().unwrap();
        assets.is_empty()
    }
    
    // Enhanced: Get all assets for a tier
    pub fn get_assets_by_tier(&self, tier: Tier) -> Vec<(String, AssetMetadata)> {
        let assets = self.assets.read().unwrap();
        assets.iter()
            .filter(|(_, metadata)| metadata.tier == tier)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

unsafe impl Send for SimpleAssetRegistry {}
unsafe impl Sync for SimpleAssetRegistry {}

// ================================
// === PLATFORM STRATEGIES ===
// ================================

#[cfg(target_arch = "wasm32")]
pub struct WasmStrategy {
    initial_pages: AtomicUsize,
}

#[cfg(target_arch = "wasm32")]
impl WasmStrategy {
    pub fn new() -> Self {
        Self {
            initial_pages: AtomicUsize::new(core::arch::wasm32::memory_size(0)),
        }
    }
    
    pub fn try_grow(&self, arena: &LockFreeArena, size: usize) -> Option<usize> {
        let current_usage = arena.usage();
        let available = arena.capacity().saturating_sub(current_usage);
        
        if available >= size {
            return None;
        }
        
        let needed = size - available;
        let pages_needed = (needed + 65535) / 65536;
        let actual_pages = pages_needed.max(16);
        
        let old_pages = core::arch::wasm32::memory_grow(0, actual_pages);
        if old_pages == usize::MAX {
            return None;
        }
        
        let new_total_pages = old_pages + actual_pages;
        let new_total_size = new_total_pages * 65536;
        let tier_percentage = arena.tier.memory_percentage();
        let new_tier_size = (new_total_size * tier_percentage) / 100;
        
        unsafe {
            arena.extend_capacity(new_tier_size);
        }
        
        arena.allocate(size)
    }
}

// ================================
// === MAIN WALLOC IMPLEMENTATION ===
// ================================

pub struct Walloc {
    arenas: [LockFreeArena; 3],
    pub assets: Arc<SimpleAssetRegistry>,
    http_client: Client,
    base_url: String,  // Removed RwLock - set before into_arc()
    memory_base: *mut u8,
    memory_size: usize,
    // For MemoryOwner support - keeping RwLock as it's accessed after Arc conversion
    self_ref: RwLock<Option<Arc<Walloc>>>,
    
    #[cfg(target_arch = "wasm32")]
    wasm_strategy: WasmStrategy,
}

impl Walloc {
    pub fn new() -> Result<Self, &'static str> {
        #[cfg(target_arch = "wasm32")]
        {
            let memory_pages = core::arch::wasm32::memory_size(0);
            let memory_base = 0 as *mut u8;
            let memory_size = memory_pages * 65536;
            
            Self::with_memory(memory_base, memory_size)
        }
        
        #[cfg(not(target_arch = "wasm32"))]
        {
            let memory_size = 64 * 1024 * 1024;
            let layout = std::alloc::Layout::from_size_align(memory_size, 4096)
                .map_err(|_| "Invalid memory layout")?;
            let memory_base = unsafe { std::alloc::alloc(layout) };
            
            if memory_base.is_null() {
                return Err("Failed to allocate memory for Walloc");
            }
            
            Self::with_memory(memory_base, memory_size)
        }
    }
    
    fn with_memory(memory_base: *mut u8, memory_size: usize) -> Result<Self, &'static str> {
        #[cfg(not(target_arch = "wasm32"))]
        unsafe {
            GLOBAL_MEMORY_BASE = memory_base;
        }
        
        let aligned_base = (memory_base as usize + CACHE_LINE_SIZE - 1) & !(CACHE_LINE_SIZE - 1);
        let adjusted_size = memory_size.saturating_sub(aligned_base - memory_base as usize);
        
        let render_size = ((adjusted_size * 50 / 100) + CACHE_LINE_SIZE - 1) & !(CACHE_LINE_SIZE - 1);
        let scene_size = ((adjusted_size * 30 / 100) + CACHE_LINE_SIZE - 1) & !(CACHE_LINE_SIZE - 1);
        let entity_size = adjusted_size - render_size - scene_size;
        
        let render_base = aligned_base as *mut u8;
        let scene_base = unsafe { render_base.add(render_size) };
        let entity_base = unsafe { scene_base.add(scene_size) };
        
        Ok(Self {
            arenas: [
                LockFreeArena::new(render_base, render_size, Tier::Top, memory_base),
                LockFreeArena::new(scene_base, scene_size, Tier::Middle, memory_base),
                LockFreeArena::new(entity_base, entity_size, Tier::Bottom, memory_base),
            ],
            assets: Arc::new(SimpleAssetRegistry::new()),
            http_client: Client::new(),
            base_url: String::new(),
            memory_base,
            memory_size,
            self_ref: RwLock::new(None),
            
            #[cfg(target_arch = "wasm32")]
            wasm_strategy: WasmStrategy::new(),
        })
    }
    
    // Set self reference after construction for MemoryOwner support
    pub fn into_arc(self) -> Arc<Self> {
        let arc = Arc::new(self);
        // Set self reference in a thread-safe way
        {
            let mut self_ref = arc.self_ref.write().unwrap();
            *self_ref = Some(Arc::clone(&arc));
        }
        arc
    }
    
    // Builder method to set base URL before converting to Arc
    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }
    
    // ================================
    // === ENHANCED ALLOCATION API ===
    // ================================
    
    // Allocate with memory owner tracking
    pub fn allocate_with_owner(&self, size: usize, tier: Tier) -> Option<(MemoryOwner, MemoryHandle)> {
        let arena = &self.arenas[tier as usize];
        
        if let Some(global_offset) = arena.allocate(size) {
            let handle = MemoryHandle(global_offset);
            if let Ok(self_ref_guard) = self.self_ref.read() {
                if let Some(ref self_arc) = *self_ref_guard {
                    let mut owner = MemoryOwner::new(tier as usize, Arc::downgrade(self_arc));
                    owner.add_allocation(handle, size);
                    return Some((owner, handle));
                }
            }
        }
        
        None
    }
    
    #[inline(always)]
    pub fn allocate(&self, size: usize, tier: Tier) -> Option<MemoryHandle> {
        let arena = &self.arenas[tier as usize];
        
        if let Some(global_offset) = arena.allocate(size) {
            return Some(MemoryHandle(global_offset));
        }
        
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(global_offset) = self.wasm_strategy.try_grow(arena, size) {
                return Some(MemoryHandle(global_offset));
            }
        }
        
        None
    }
    
    pub fn allocate_batch(&self, requests: &[(usize, Tier)]) -> Vec<Option<MemoryHandle>> {
        let mut results = Vec::with_capacity(requests.len());
        
        let mut tier_groups: [Vec<(usize, usize)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        
        for (idx, &(size, tier)) in requests.iter().enumerate() {
            tier_groups[tier as usize].push((idx, size));
        }
        
        results.resize(requests.len(), None);
        
        for (tier_idx, group) in tier_groups.iter().enumerate() {
            let arena = &self.arenas[tier_idx];
            
            for &(original_idx, size) in group {
                if let Some(global_offset) = arena.allocate(size) {
                    results[original_idx] = Some(MemoryHandle(global_offset));
                }
            }
        }
        
        results
    }

    // Enhanced: Fast compact tier with proper data preservation
    pub fn fast_compact_tier(&self, tier: Tier, preserve_bytes: usize) -> bool {
        let arena = &self.arenas[tier as usize];
        
        let current_usage = arena.usage();
        let capacity = arena.capacity();
        
        // If we need more space than currently allocated
        if preserve_bytes > current_usage {
            // Check if the requested size exceeds our capacity
            if preserve_bytes > capacity {
                #[cfg(target_arch = "wasm32")]
                {
                    // Get current memory stats
                    let current_memory = self.get_memory_limit();
                    let additional_needed = preserve_bytes - capacity;
                    
                    // Check if growth would exceed limit
                    if current_memory + additional_needed > MAX_MEMORY_LIMIT {
                        return false;
                    }
                    
                    // Preserve existing data if needed
                    let preserve_data = if current_usage > 0 {
                        let mut data = Vec::with_capacity(current_usage);
                        unsafe {
                            data.set_len(current_usage);
                            SIMDOps::fast_copy(arena.base_ptr(), data.as_mut_ptr(), current_usage);
                        }
                        Some(data)
                    } else {
                        None
                    };
                    
                    // Try to grow memory
                    let pages_needed = (additional_needed + 65535) / 65536;
                    let grow_result = core::arch::wasm32::memory_grow(0, pages_needed);
                    
                    if grow_result == usize::MAX {
                        return false;
                    }
                    
                    // Calculate new tier size
                    let new_total_pages = grow_result + pages_needed;
                    let new_total_size = new_total_pages * 65536;
                    let tier_percentage = arena.tier.memory_percentage();
                    let new_tier_size = (new_total_size * tier_percentage) / 100;
                    
                    // Extend arena capacity
                    unsafe {
                        arena.extend_capacity(new_tier_size);
                    }
                    
                    // Restore preserved data
                    if let Some(data) = preserve_data {
                        unsafe {
                            SIMDOps::fast_copy(data.as_ptr(), arena.base_ptr(), data.len());
                        }
                    }
                    
                    // Set allocation head to preserve_bytes
                    arena.allocation_head.store(preserve_bytes, Ordering::SeqCst);
                    arena.allocated.store(preserve_bytes, Ordering::SeqCst);
                    
                    // Clear freelists
                    for freelist in &arena.freelists {
                        freelist.store(std::ptr::null_mut(), Ordering::SeqCst);
                    }
                    
                    return true;
                }
                
                #[cfg(not(target_arch = "wasm32"))]
                {
                    // For non-WASM, we can't grow arena in place
                    // But we can still preserve data by creating a temporary buffer
                    return false;
                }
            }
            
            // We have enough capacity, just update allocation head
            arena.allocation_head.store(preserve_bytes, Ordering::SeqCst);
            arena.allocated.store(preserve_bytes, Ordering::SeqCst);
            
            // Clear freelists
            for freelist in &arena.freelists {
                freelist.store(std::ptr::null_mut(), Ordering::SeqCst);
            }
            
            return true;
        }
        
        // Standard case: preserve_bytes <= current_usage
        // Use arena's fast compact
        arena.fast_compact(preserve_bytes)
    }
    
    // ================================
    // === DATA OPERATIONS ===
    // ================================
    
    #[inline(always)]
    fn get_memory_limit(&self) -> usize {
        #[cfg(target_arch = "wasm32")]
        {
            core::arch::wasm32::memory_size(0) * 65536
        }
        
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.memory_size
        }
    }
    
    pub fn write_data(&self, handle: MemoryHandle, data: &[u8]) -> Result<(), &'static str> {
        if handle.is_null() {
            return Err("Memory handle is null");
        }
        
        let end_offset = handle.offset().saturating_add(data.len());
        if end_offset > self.get_memory_limit() {
            return Err("Memory access out of bounds");
        }
        
        unsafe {
            SIMDOps::fast_copy(data.as_ptr(), handle.to_ptr(), data.len());
        }
        Ok(())
    }
    
    pub fn read_data(&self, handle: MemoryHandle, length: usize) -> Option<Vec<u8>> {
        if handle.is_null() || handle.offset().saturating_add(length) > self.get_memory_limit() {
            return None;
        }
        
        let mut buffer = Vec::with_capacity(length);
        unsafe {
            buffer.set_len(length);
            SIMDOps::fast_copy(handle.to_ptr(), buffer.as_mut_ptr(), length);
        }
        Some(buffer)
    }
    
    pub unsafe fn bulk_copy(&self, operations: &[(MemoryHandle, MemoryHandle, usize)]) {
        unsafe { SIMDOps::bulk_copy_optimized(operations); }
    }
    
    // ================================
    // === ENHANCED ASSET MANAGEMENT ===
    // ================================
    
    pub fn set_base_url(&mut self, url: String) {
        self.base_url = url;
    }

    pub fn register_asset(&self, key: String, metadata: AssetMetadata) -> bool {
        self.assets.insert(key, metadata)
    }

    // Enhanced: Evict asset with automatic compaction on supported platforms
    pub fn evict_asset(&self, path: &str) -> bool {
        let metadata_opt = self.assets.get(path);
        
        if let Some(metadata) = metadata_opt {
            let handle = metadata.handle;
            let size = metadata.size;
            let tier = metadata.tier;
            
            if handle.is_null() || tier as usize >= self.arenas.len() {
                return self.assets.remove(path);
            }
            
            // On WASM, always compact to reduce fragmentation
            #[cfg(target_arch = "wasm32")]
            {
                // Get all assets for this tier
                let tier_assets = self.assets.get_assets_by_tier(tier);
                
                // Calculate total size needed (excluding the asset being evicted)
                let mut preserve_size = 0;
                let mut preserved_assets = Vec::new();
                
                for (asset_path, asset_meta) in tier_assets {
                    if asset_path != path {
                        preserve_size += asset_meta.size;
                        preserved_assets.push((asset_path, asset_meta));
                    }
                }
                
                if preserve_size > 0 {
                    // Create temporary buffer for preserved data
                    let mut preserve_buffer = Vec::with_capacity(preserve_size);
                    let mut new_offsets = Vec::new();
                    
                    // Copy all assets except the one being evicted
                    for (asset_path, asset_meta) in &preserved_assets {
                        let new_offset = preserve_buffer.len();
                        
                        unsafe {
                            let src_ptr = asset_meta.handle.to_ptr();
                            if !src_ptr.is_null() {
                                let mut temp = vec![0u8; asset_meta.size];
                                SIMDOps::fast_copy(src_ptr, temp.as_mut_ptr(), asset_meta.size);
                                preserve_buffer.extend_from_slice(&temp);
                                new_offsets.push((asset_path.clone(), new_offset, asset_meta.clone()));
                            }
                        }
                    }
                    
                    // Reset the tier
                    self.reset_tier(tier);
                    
                    // Allocate space for preserved data
                    if let Some(new_handle) = self.allocate(preserve_buffer.len(), tier) {
                        // Copy preserved data back
                        unsafe {
                            SIMDOps::fast_copy(
                                preserve_buffer.as_ptr(),
                                new_handle.to_ptr(),
                                preserve_buffer.len()
                            );
                        }
                        
                        // Update asset registry with new offsets
                        for (asset_path, offset_in_buffer, mut asset_meta) in new_offsets {
                            let new_global_offset = new_handle.offset() + offset_in_buffer;
                            asset_meta.handle = MemoryHandle(new_global_offset);
                            asset_meta.offset = new_global_offset;
                            self.assets.insert(asset_path, asset_meta);
                        }
                    }
                }
                
                // Remove the target asset
                return self.assets.remove(path);
            }
            
            // On native platforms, just deallocate without compaction
            #[cfg(not(target_arch = "wasm32"))]
            {
                let removed = self.assets.remove(path);
                
                if removed {
                    let arena = &self.arenas[tier as usize];
                    let _ = arena.deallocate(handle, size);
                }
                
                return removed;
            }
        }
        
        false
    }
    
    pub fn evict_assets_batch(&self, paths: &[String]) -> usize {
        #[cfg(target_arch = "wasm32")]
        {
            // On WASM, evict each asset with compaction
            let mut evicted = 0;
            for path in paths {
                if self.evict_asset(path) {
                    evicted += 1;
                }
            }
            evicted
        }
        
        #[cfg(not(target_arch = "wasm32"))]
        {
            // On native, batch process without compaction for efficiency
            let mut evicted = 0;
            
            let mut to_evict = Vec::with_capacity(paths.len());
            
            for path in paths {
                if let Some(metadata) = self.assets.get(path) {
                    to_evict.push((path.clone(), metadata.handle, metadata.size, metadata.tier));
                }
            }
            
            for (path, handle, size, tier) in to_evict {
                if handle.is_null() || tier as usize >= self.arenas.len() {
                    if self.assets.remove(&path) {
                        evicted += 1;
                    }
                    continue;
                }
                
                if self.assets.remove(&path) {
                    let arena = &self.arenas[tier as usize];
                    let _ = arena.deallocate(handle, size);
                    evicted += 1;
                }
            }
            
            evicted
        }
    }
    
    pub async fn load_asset_unified(&self, path: String, asset_type: AssetType) -> Result<MemoryHandle, String> {
        let full_url = if self.base_url.is_empty() {
            path.clone()
        } else {
            format!("{}{}", self.base_url, path)
        };
        
        let response = self.http_client
            .get(&full_url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch '{}': {}", full_url, e))?;
        
        if !response.status().is_success() {
            return Err(format!("HTTP error {}: {}", response.status(), full_url));
        }
        
        let content_length = response.content_length().unwrap_or(0) as usize;
        
        if content_length > 1024 * 1024 {
            let handle = self.allocate(content_length, Tier::Middle)
                .ok_or_else(|| format!("Failed to allocate {} bytes", content_length))?;
            
            let bytes = response.bytes().await
                .map_err(|e| format!("Failed to get bytes: {}", e))?;
            
            unsafe {
                SIMDOps::fast_copy(bytes.as_ptr(), handle.to_ptr(), bytes.len());
            }
            
            self.assets.insert(path, AssetMetadata {
                asset_type,
                size: bytes.len(),
                offset: handle.offset(),
                tier: Tier::Middle,
                handle,
            });
            
            Ok(handle)
        } else {
            let bytes = response.bytes().await
                .map_err(|e| format!("Failed to get bytes: {}", e))?;
            
            let handle = self.allocate(bytes.len(), Tier::Middle)
                .ok_or_else(|| format!("Failed to allocate {} bytes", bytes.len()))?;
            
            unsafe {
                SIMDOps::fast_copy(bytes.as_ptr(), handle.to_ptr(), bytes.len());
            }
            
            self.assets.insert(path, AssetMetadata {
                asset_type,
                size: bytes.len(),
                offset: handle.offset(),
                tier: Tier::Middle,
                handle,
            });
            
            Ok(handle)
        }
    }

    pub async fn load_asset(&self, path: String, asset_type: AssetType) -> Result<MemoryHandle, String> {
        self.load_asset_unified(path, asset_type).await
    }
    
    pub async fn load_assets_batch(&self, requests: Vec<(String, AssetType)>) -> Vec<Result<MemoryHandle, String>> {
        stream::iter(requests)
            .map(|(path, asset_type)| async move {
                self.load_asset(path, asset_type).await
            })
            .buffer_unordered(PARALLEL_LOAD_FACTOR)
            .collect()
            .await
    }
    
    pub fn load_asset_zero_copy(&self, data: &[u8], tier: Tier) -> Option<MemoryHandle> {
        let handle = self.allocate(data.len(), tier)?;
        
        unsafe {
            let ptr = handle.to_ptr();
            SIMDOps::fast_copy(data.as_ptr(), ptr, data.len());
        }
        
        Some(handle)
    }
    
    pub fn get_asset(&self, path: &str) -> Option<AssetMetadata> {
        self.assets.get(path)
    }
    
    // ================================
    // === MANAGEMENT & STATS ===
    // ================================
    
    pub fn reset_tier(&self, tier: Tier) {
        self.arenas[tier as usize].reset();
    }
    
    pub fn tier_stats(&self, tier: Tier) -> (usize, usize, usize, usize) {
        self.arenas[tier as usize].stats()
    }
    
    pub fn memory_utilization(&self) -> f64 {
        let mut total_used = 0;
        
        for tier in [Tier::Top, Tier::Middle, Tier::Bottom] {
            let (used, _, _, _) = self.tier_stats(tier);
            total_used += used;
        }
        
        let total_memory = self.get_memory_limit();
        
        if total_memory > 0 {
            (total_used as f64 / total_memory as f64) * 100.0
        } else {
            0.0
        }
    }
    
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn test_fetch_json(&self) -> Result<String, String> {
        let test_url = "https://jsonplaceholder.typicode.com/todos/1";
        
        let response = self.http_client.get(test_url).send().await
            .map_err(|e| format!("Failed to fetch: {}", e))?;
        
        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }
        
        let text = response.text().await
            .map_err(|e| format!("Failed to get text: {}", e))?;
        
        Ok(text)
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn test_fetch_json(&self) -> Result<String, String> {
        let test_url = "https://jsonplaceholder.typicode.com/todos/1";
        
        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&format!("Testing HTTP: {}", test_url)));
        
        let response = self.http_client.get(test_url).send().await
            .map_err(|e| format!("Failed to fetch: {}", e))?;
        
        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }
        
        let text = response.text().await
            .map_err(|e| format!("Failed to get text: {}", e))?;
        
        Ok(text)
    }
}

unsafe impl Send for Walloc {}
unsafe impl Sync for Walloc {}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Walloc {
    fn drop(&mut self) {
        if !self.memory_base.is_null() {
            for arena in &self.arenas {
                arena.reset();
            }
            
            self.assets.clear();

            std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
            
            let layout = std::alloc::Layout::from_size_align(self.memory_size, 4096)
                .unwrap_or_else(|_| std::alloc::Layout::from_size_align(self.memory_size, 8).unwrap());
            
            unsafe {
                std::alloc::dealloc(self.memory_base, layout);
                GLOBAL_MEMORY_BASE = std::ptr::null_mut();
            }
        }
    }
}

// ================================
// === WASM BINDINGS ===
// ================================

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct WallocWrapper {
    inner: Arc<Walloc>,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl WallocWrapper {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<WallocWrapper, JsValue> {
        Walloc::new()
            .map(|walloc| WallocWrapper { inner: walloc.into_arc() })
            .map_err(|e| JsValue::from_str(e))
    }
    
    // New constructor with base URL
    #[wasm_bindgen]
    pub fn new_with_base_url(base_url: String) -> Result<WallocWrapper, JsValue> {
        Walloc::new()
            .map(|walloc| WallocWrapper { 
                inner: walloc.with_base_url(base_url).into_arc() 
            })
            .map_err(|e| JsValue::from_str(e))
    }
    
    // Note: base_url must be set before creating WallocWrapper
    // This method is removed as base_url is immutable after Arc conversion
    
    #[wasm_bindgen]
    pub fn allocate(&self, size: usize, tier_number: u8) -> usize {
        match (Tier::from_u8(tier_number), self.inner.allocate(size, Tier::from_u8(tier_number).unwrap_or(Tier::Bottom))) {
            (Some(_), Some(handle)) => handle.offset(),
            _ => usize::MAX,
        }
    }

    #[wasm_bindgen]
    pub fn allocate_with_owner(&self, size: usize, tier_number: u8) -> js_sys::Object {
        let tier = Tier::from_u8(tier_number).unwrap_or(Tier::Bottom);
        
        let obj = js_sys::Object::new();
        
        if let Some((owner, handle)) = self.inner.allocate_with_owner(size, tier) {
            js_sys::Reflect::set(&obj, &"offset".into(), &JsValue::from_f64(handle.offset() as f64)).unwrap();
            js_sys::Reflect::set(&obj, &"size".into(), &JsValue::from_f64(owner.total_size() as f64)).unwrap();
            
            // Store the owner in a way that JS can track
            let owner_box = Box::new(owner);
            let owner_ptr = Box::into_raw(owner_box);
            js_sys::Reflect::set(&obj, &"owner_ptr".into(), &JsValue::from_f64(owner_ptr as usize as f64)).unwrap();
        } else {
            js_sys::Reflect::set(&obj, &"offset".into(), &JsValue::from_f64(usize::MAX as f64)).unwrap();
        }
        
        obj
    }

    #[wasm_bindgen]
    pub fn fast_compact_tier(&self, tier_number: u8, preserve_bytes: usize) -> bool {
        let tier = match Tier::from_u8(tier_number) {
            Some(t) => t,
            None => return false,
        };
        
        self.inner.fast_compact_tier(tier, preserve_bytes)
    }

    #[wasm_bindgen]
    pub fn register_asset(&self, key: String, asset_type: u8, size: usize, handle: usize, tier_number: u8) -> bool {
        let tier = Tier::from_u8(tier_number).unwrap_or(Tier::Middle);
        
        let metadata = AssetMetadata {
            asset_type: match asset_type {
                0 => AssetType::Image,
                1 => AssetType::Json,
                _ => AssetType::Binary,
            },
            size,
            offset: handle,
            tier,
            handle: MemoryHandle(handle),
        };
        
        self.inner.register_asset(key, metadata)
    }

    #[wasm_bindgen]
    pub fn evict_asset(&self, path: String) -> bool {
        self.inner.evict_asset(&path)
    }
    
    #[wasm_bindgen]
    pub fn evict_assets_batch(&self, paths: js_sys::Array) -> usize {
        let mut path_vec = Vec::with_capacity(paths.length() as usize);
        
        for i in 0..paths.length() {
            if let Some(path) = paths.get(i).as_string() {
                path_vec.push(path);
            }
        }
        
        self.inner.evict_assets_batch(&path_vec)
    }
    
    #[wasm_bindgen]
    pub fn load_asset_zero_copy(&self, data: &js_sys::Uint8Array, tier_number: u8) -> usize {
        let tier = Tier::from_u8(tier_number).unwrap_or(Tier::Bottom);
        let data_vec = data.to_vec();
        self.inner.load_asset_zero_copy(&data_vec, tier)
            .map(|h| h.offset())
            .unwrap_or(usize::MAX)
    }
    
    #[wasm_bindgen]
    pub fn reset_tier(&self, tier_number: u8) -> bool {
        if let Some(tier) = Tier::from_u8(tier_number) {
            self.inner.reset_tier(tier);
            true
        } else {
            false
        }
    }
    
    #[wasm_bindgen]
    pub fn load_asset(&self, path: String, asset_type: u8) -> Promise {
        let inner = self.inner.clone();
        
        future_to_promise(async move {
            let asset_type = match asset_type {
                0 => AssetType::Image,
                1 => AssetType::Json,
                2 => AssetType::Binary,
                _ => return Err(JsValue::from_str("Invalid asset type")),
            };
            
            web_sys::console::log_1(&JsValue::from_str(&format!(
                "WallocWrapper: Loading asset {} of type {}", path, asset_type as u8
            )));
            
            match inner.load_asset_unified(path, asset_type).await {
                Ok(handle) => {
                    let offset = handle.offset();
                    web_sys::console::log_1(&JsValue::from_str(&format!(
                        "Asset loaded successfully: {}", offset
                    )));
                    Ok(JsValue::from_f64(offset as f64))
                },
                Err(e) => {
                    web_sys::console::error_1(&JsValue::from_str(&format!(
                        "Error loading asset: {}", e
                    )));
                    Err(JsValue::from_str(&e))
                },
            }
        })
    }
    
    #[wasm_bindgen]
    pub fn get_asset_data(&self, path: String) -> Result<js_sys::Uint8Array, JsValue> {
        let metadata = self.inner.get_asset(&path)
            .ok_or_else(|| JsValue::from_str(&format!("WASM Asset not found: {}", path)))?;
        
        unsafe {
            let ptr = metadata.handle.to_ptr();
            let mem_slice = std::slice::from_raw_parts(ptr, metadata.size);
            Ok(js_sys::Uint8Array::from(mem_slice))
        }
    }
    
    #[wasm_bindgen]
    pub fn get_memory_view(&self, offset: usize, length: usize) -> Result<js_sys::Uint8Array, JsValue> {
        let limit = core::arch::wasm32::memory_size(0) * 65536;
        if offset >= limit || offset.saturating_add(length) > limit {
            return Err(JsValue::from_str("WASM Memory access out of bounds"));
        }
        
        unsafe {
            Ok(js_sys::Uint8Array::view(std::slice::from_raw_parts(
                offset as *const u8,
                length
            )))
        }
    }
    
    #[wasm_bindgen]
    pub fn write_memory(&self, offset: usize, data: &js_sys::Uint8Array) -> Result<(), JsValue> {
        let handle = MemoryHandle(offset);
        let data_vec = data.to_vec();
        
        let current_memory_pages = core::arch::wasm32::memory_size(0);
        let current_memory_size = current_memory_pages * 65536;
        
        if handle.is_null() || handle.offset().saturating_add(data_vec.len()) > current_memory_size {
            return Err(JsValue::from_str("WASM Memory access out of bounds"));
        }
        
        unsafe {
            let ptr = handle.to_ptr();
            SIMDOps::fast_copy(data_vec.as_ptr(), ptr, data_vec.len());
        }
        
        Ok(())
    }

    #[wasm_bindgen]
    pub fn test_http_connection(&self) -> Promise {
        let inner = self.inner.clone();
        
        future_to_promise(async move {
            web_sys::console::log_1(&JsValue::from_str("Testing HTTP connection..."));
            
            match inner.test_fetch_json().await {
                Ok(text) => {
                    web_sys::console::log_1(&JsValue::from_str(&text));
                    Ok(JsValue::from_str(&text))
                },
                Err(e) => {
                    web_sys::console::error_1(&JsValue::from_str(&format!(
                        "HTTP test failed: {}", e
                    )));
                    Err(JsValue::from_str(&e))
                }
            }
        })
    }

    #[wasm_bindgen]
    pub fn get_current_memory_size(&self) -> usize {
        let current_memory_pages = core::arch::wasm32::memory_size(0);
        current_memory_pages * 65536
    }
    
    #[wasm_bindgen]
    pub fn memory_stats(&self) -> js_sys::Object {
        let obj = js_sys::Object::new();
        
        let current_pages = core::arch::wasm32::memory_size(0);
        let current_size = current_pages * 65536;
        
        let mut total_in_use = 0;
        let tiers = js_sys::Array::new();
        
        for tier_num in 0..3 {
            if let Some(tier) = Tier::from_u8(tier_num) {
                let (used, capacity, high_water, total_allocated) = self.inner.tier_stats(tier);
                let tier_obj = js_sys::Object::new();
                
                total_in_use += used;
                
                let tier_name = match tier {
                    Tier::Top => "render",
                    Tier::Middle => "scene", 
                    Tier::Bottom => "entity",
                };
                
                js_sys::Reflect::set(&tier_obj, &"name".into(), &JsValue::from_str(tier_name)).unwrap();
                js_sys::Reflect::set(&tier_obj, &"used".into(), &JsValue::from_f64(used as f64)).unwrap();
                js_sys::Reflect::set(&tier_obj, &"capacity".into(), &JsValue::from_f64(capacity as f64)).unwrap();
                js_sys::Reflect::set(&tier_obj, &"highWaterMark".into(), &JsValue::from_f64(high_water as f64)).unwrap();
                js_sys::Reflect::set(&tier_obj, &"totalAllocated".into(), &JsValue::from_f64(total_allocated as f64)).unwrap();
                
                let saved = if total_allocated > used { total_allocated - used } else { 0 };
                js_sys::Reflect::set(&tier_obj, &"memorySaved".into(), &JsValue::from_f64(saved as f64)).unwrap();
                
                tiers.push(&tier_obj);
            }
        }
        
        js_sys::Reflect::set(&obj, &"tiers".into(), &tiers).unwrap();
        js_sys::Reflect::set(&obj, &"totalUsed".into(), &JsValue::from_f64(total_in_use as f64)).unwrap();
        js_sys::Reflect::set(&obj, &"pages".into(), &JsValue::from_f64(current_pages as f64)).unwrap();
        js_sys::Reflect::set(&obj, &"rawMemorySize".into(), &JsValue::from_f64(current_size as f64)).unwrap();
        js_sys::Reflect::set(&obj, &"allocatorType".into(), &JsValue::from_str("lock-free-tiered")).unwrap();
        js_sys::Reflect::set(&obj, &"memoryUtilization".into(), &JsValue::from_f64(self.inner.memory_utilization())).unwrap();
        
        obj
    }
}

impl Clone for Walloc {
    fn clone(&self) -> Self {
        // Deep clone creates a new Walloc instance
        let mut new_walloc = Self::with_memory(self.memory_base, self.memory_size)
            .expect("Failed to clone Walloc");

        // Clone base_url
        new_walloc.base_url = self.base_url.clone();
        // Don't clone self_ref - it will be set when into_arc is called
        
        new_walloc
    }
}

// ================================
// === PUBLIC API ===
// ================================

pub fn create_walloc() -> Result<Walloc, &'static str> {
    Walloc::new()
}