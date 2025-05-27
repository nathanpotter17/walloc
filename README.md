# WALLOC: A WebAssembly memory allocator using Rust

Walloc is a custom memory allocator implemented in Rust for WebAssembly applications, optimized for state machines that require aggressive re-allocation, while retaining safety.
It provides efficient memory management with direct control over the WASM linear memory space, enabling high-performance memory utilization within browser environments.

To test:

- Ensure you have Cargo, Rustup, and the Rust toolchain installed.
- Get the `wasm32-unknown-unknown` target using `rustup target add ...`.
- Run `bash build.sh` from the `walloc/` directory.
- Serve `index.html` and click the 'Start Simulation' button.
- View the Console Output for test results.

## Key Features

- Efficient Memory Utilization: Intelligently manages WebAssembly's linear memory, gradually growing as needed up to near the full 4GB address space.
- Direct Memory Access: Provides low-level memory manipulation with typed array views, supporting raw memory operations critical for graphics applications.
- Configurable Allocation Strategy: Uses a first-fit allocation strategy for speed with block splitting and coalescing to minimize fragmentation.
- Memory Lifecycle Management: Supports allocation, deallocation, reallocation, and complete memory reset functionality.
- Browser Compatibility: Designed to work around browser-specific memory limitations while maximizing available memory.
- Intelligent Design: Walloc allocator is automatically configured so we dont accidentally grow into the stack memory occupied by our program.

## Aside: Memory in WASM

- WebAssembly linear memory: WebAssembly memories represent a contiguous array of bytes that have a size that is always a multiple of the
  WebAssembly page size (64KiB).
- The Wasm heap is located in its own linear memory space. There are two typical usages of heap: Wasm code calls malloc/free on its own heap.
  The native code of the host calls wasm_runtime_module_malloc to allocate buffer from the Wasm app's heap, and then use it
  for sharing data between host and Wasm.
- The WebAssembly module starts with a certain number of memory pages by default. Emscripten's default is 16 pages. This initial allocation is determined by EMCC or the Rust/WebAssembly compiler toolchain based on the static requirements of the program, with some extra space allocated for the heap.

## Technical Details

- The allocator manages WebAssembly memory pages (64KB chunks) and provides a familiar malloc/free interface. It includes mechanisms for safely transferring data between JavaScript and WebAssembly memory spaces via typed arrays, with built-in bounds checking for memory safety.
- Rust is the perfect language to implement this because of its ownership and scope models help prevent unsafe memory patterns, and a lot of the built in memory functions for Rust are safe wrappers of C instructions.
- This component forms the foundation layer of a 3D rendering engine, enabling optimized memory patterns for graphics data like geometry buffers, textures, and scene graph information, with a focus on supporting safe and efficient memory reuse.
- To allow for a smooth memory usage experience, the TieredAllocator doesnt auto-rebalance its memory, although it does start with a defined reserved space, where the render tier takes 50%, the scene tier takes 30%, and the entity tier takes the final 20%. The limits are up to the implementor though, each tier is allowed to grow to the maximum available memory for scene flexibility.

## Technical Specs

- Walloc Allocator

  - Walloc's WASM Binary is only 356 KB
  - Walloc's JS Glue Code is 30 KB
  - Walloc's Startup Runtime Memory for WASM is 1.125MB (Default, Reserved) (~18 Pages)

    - This initial memory allocation includes:

      - Compiled Rust code (the Walloc implementation and all other functions)
      - WebAssembly runtime overhead
      - The static data segment (global variables, constant data)
      - Initial stack space
      - The heap area that Walloc will manage

    - Walloc has both a default allocator and a tiered allocator that uses the default as fallback.

      - The default allocator exposes itself to the Web via JS, constructed by wasm-bindgen.

        - Walloc::new() yields a new default allocator, and new_tiered yields the tiered allocator.

          - Memory Layout & Design - Technical Details

            - Layout

              ```
              Render Tier (50%)
                - 128-byte aligned
                - Optimized for GPU access
              Scene Tier (30%)

                - 64-byte aligned
                - Medium lifecycle objects

              Entity Tier (15%)

                - 8-byte aligned
                - Short-lived objects

              Fallback (5%)
                - Traditional allocator
              ```

            - Performance Considerations

              - Allocation in arenas is O(1) using atomic bump allocation
              - Deallocation of entire tiers is O(1)
              - Individual deallocations within arenas are not supported (use contexts instead)
              - Arena-based allocation avoids fragmentation

            - Thread Safety

              - All arena operations use atomic operations for thread safety
              - Mutexes protect concurrent access to arenas
              - Arc enables safe sharing of arenas between contexts

            - Implementation Notes
              - Uses WebAssembly's linear memory model
              - Memory pages are 64KB each
              - The allocator automatically grows memory when needed
              - Proper memory alignment ensures optimal performance for GPU access

## Review: Borrowing & Ownership Model

- Independent Reference Counting: Each arena (Render, Scene, Entity) has its own Arc<Mutex<>>, meaning its lifetime is managed independently.
- No Hierarchical Ownership: When a SceneContext is dropped, it doesn't automatically drop the EntityContext objects created from it. Each has its own separate reference count.
- Manual Reset Required: Without nested lifetimes, you need to explicitly call reset_tier() to clear a tier - dropping a SceneContext doesn't automatically reset its arena.
  This is optimal considering Rust's approach to borrowing and ownership.

  - Each arrow represents an Arc reference, and when all references to an arena are gone, the Arc is dropped, but the memory isn't fully reclaimed until you explicitly reset the arena.
    - While this may seem problematic to not enforce garbage collection or a full reset after the Arc is dropped, it allows for the engine to maintain its speed, and leaves the region
      in the hands of the developer.
  - ```
    Scene ------> has reference to ----> Scene Arena
          |
          +--> creates --> Entity A ------> has reference to ----> Entity Arena
          |
          +--> creates --> Entity B ------> has reference to ----> Entity Arena
    ```

## Review: Recycle Model

When you call fast_compact_tier(TIER.SCENE, 1 \* MB), here's what happens:

1. The first 1 \* MB of memory in the SCENE tier is preserved exactly as-is
2. The allocation pointer is simply reset to the position right after this preserved section
3. Any new allocations will automatically start from this new position (after the preserved area)
4. The old data beyond the preserved area remains in memory as "garbage" but will be overwritten by new allocations

This enables a very efficient way to keep important data while recycling the rest of the memory. There's no expensive memory copying involved - it's just a pointer adjustment, which is extremely fast.

```
Before fast_compact_tier(TIER.SCENE, 1MB):
[TIER BASE]
|-------------------------|-------------------------|-------------------------|
| Important level data    | Current scene objects   | Recently culled objects |
| (1MB)                   | (2MB)                   | (1MB)                   |
|-------------------------|-------------------------|-------------------------|
                          ^                                                   ^
                          |                                                   |
                  current_offset = 3MB                               capacity = 4MB


After fast_compact_tier(TIER.SCENE, 1MB):
[TIER BASE]
|-------------------------|-------------------------|-------------------------|
| Important level data    | "Garbage" data, but     | "Garbage" data, but     |
| (preserved, 1MB)        | available for reuse     | available for reuse     |
|-------------------------|-------------------------|-------------------------|
                          ^                                                   ^
                          |                                                   |
                  current_offset = 1MB                               capacity = 4MB


After new allocations:
[TIER BASE]
|-------------------------|-------------------------|-------------------------|
| Important level data    | Newly allocated objects | "Garbage" data, but     |
| (preserved, 1MB)        | (1.5MB)                 | available for reuse     |
|-------------------------|-------------------------|-------------------------|
                                              ^                               ^
                                              |                               |
                                    current_offset = 2.5MB          capacity = 4MB
```

This approach is perfect for game loops because:

You can organize your memory so that persistent data (level geometry, shared textures) is at the beginning
Transient data (dynamic objects, particles) comes after.

When you need to recycle memory, you just preserve the persistent part and reuse the rest.
The operation is incredibly fast since it's just an atomic store to update the allocation pointer.

All new allocations will automatically respect the preserved area because the allocator's internal current_offset is pointing just after it. This is all handled seamlessly by the bump allocator design.

Matches game scene lifecycle perfectly:

- Persistent level data stays at the beginning (preserved section)
- Current visible/active objects occupy the middle (new allocations)
- Previously visible but now culled objects' memory is automatically recycled

Zero-cost memory recycling:

- When objects get culled from view, you don't need to explicitly free each one
- Simply call fast_compact_tier() with your preservation size, and all memory beyond that point becomes available instantaneously
- No fragmentation to worry about - the "garbage" data is simply overwritten

Perfect alignment with visibility culling:

- As the player moves through the game world, new objects come into view while others leave
- This allocator naturally accommodates this pattern without complex memory tracking

Efficient for WASM environments:

- WebAssembly has a linear memory model with growing costs
- This allocator minimizes the need to grow memory by efficiently recycling existing pages
- The high water mark tracking helps you optimize memory usage over time

Tiered Reserve & Grow Behviour:

- When asked for reservation that exceeds the available tier space, grow, but check if the grow is feasible within the max 4GB memory limit by looking at the preserved contents of the other tiers.
- When a tier asks for reservation, but 4GB max has already been hit, Attempt to recycle memory in the appropriate tier, Try the allocation again with the newly reclaimed space,
  & Only fail if recycling doesn't free enough space.

## Caching Considerations

When implementing a producer-consumer system with caching:
Problem: If you use a flag to indicate available memory and write to cache first, subsequent reads may retrieve stale data.
Explanation: In a producer-consumer setup with caching:

- The producer writes data to cache
- The producer sets a flag to true indicating memory is available
- The consumer checks the flag, sees it's true, and reads from cache
- However, if memory was updated directly (bypassing cache), the cache becomes stale

Solution: Always invalidate the cache before setting the availability flag. This ensures that:

- The next read operation will fetch fresh data from memory
- The consumer will always see the most recent updates

This prevents the race condition where cache contains outdated information while the flag indicates data is ready.
This technique is called "polling" or "scheduled polling" and is common in page based memory allocators.

## Advanced Considerations - For Frequent Allocations

### Vectorization and SIMD

Since you're in a WebAssembly context, you can use SIMD (Single Instruction, Multiple Data) instructions to process multiple bytes at once:

```rust
// Import WASM SIMD intrinsics
use core::arch::wasm32::*;

// Example: Fill memory with a value using v128 operations (16 bytes at once)
pub fn fast_fill(ptr: *mut u8, size: usize, value: u8) {
    let aligned_size = size & !15; // Round down to multiple of 16
    let simd_value = v128_set_splat_i8(value as i8);

    // Process 16 bytes at a time
    for i in (0..aligned_size).step_by(16) {
        unsafe {
            let dest = ptr.add(i) as *mut v128;
            v128_store(dest, simd_value);
        }
    }

    // Handle remaining bytes
    for i in aligned_size..size {
        unsafe {
            *ptr.add(i) = value;
        }
    }
}
```

### Type Punning for Wider Access

```rust
pub fn fast_copy_u32(src: *const u8, dst: *mut u8, count_bytes: usize) {
    let count_u32 = count_bytes / 4;

    // Reinterpret as u32 pointers
    let src_u32 = src as *const u32;
    let dst_u32 = dst as *mut u32;

    // Copy 4 bytes at a time
    for i in 0..count_u32 {
        unsafe {
            *dst_u32.add(i) = *src_u32.add(i);
        }
    }

    // Handle remaining bytes
    for i in (count_u32 * 4)..count_bytes {
        unsafe {
            *dst.add(i) = *src.add(i);
        }
    }
}
```

### Alignment Operations

Ensuring your memory operations are aligned to cache line boundaries (64 bytes) can significantly improve performance:

```rust
pub fn aligned_copy(src: *const u8, dst: *mut u8, size: usize) {
    // Check if pointers are aligned to cache line (64 bytes)
    if (src as usize % 64 == 0) && (dst as usize % 64 == 0) && (size % 64 == 0) {
        // Fast path: 64-byte aligned copy
        for i in (0..size).step_by(64) {
            // Copy an entire cache line at once
            unsafe {
                let src_ptr = src.add(i) as *const [u8; 64];
                let dst_ptr = dst.add(i) as *mut [u8; 64];
                *dst_ptr = *src_ptr;
            }
        }
    } else {
        // Fallback for unaligned memory
        for i in 0..size {
            unsafe {
                *dst.add(i) = *src.add(i);
            }
        }
    }
}
```
