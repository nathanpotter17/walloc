# WALLOC: A Cohesive High-Performance Memory Allocator

Walloc is a streamlined tiered memory management system optimized for both WebAssembly and Native applications. It provides efficient memory management with direct control over memory spaces, enabling high-performance memory utilization across browser environments and native applications.

## Key Features

- **Lock-Free Allocation**: Thread-safe memory operations without mutex contention
- **Tiered Memory Architecture**: Optimized for different allocation patterns and lifetimes
- **Global Offset Architecture**: Cross-platform pointer safety with zero-cost abstraction
- **Vectorized SIMD Operations**: Accelerated memory operations using platform-specific SIMD
- **Asset Registry**: Fast asset registration and lookup with HTTP loading support
- **Zero-Cost Memory Recycling**: Fast compaction without memory movement
- **Direct Memory Access**: Low-level memory manipulation with typed array views
- **Browser Compatibility**: Designed to work around browser-specific memory limitations

## Technical Architecture

### Tiered Memory Organization

Walloc features a three-tier memory system optimized for different use cases:

| Tier   | Allocation | Alignment | Use Case                    |
| ------ | ---------- | --------- | --------------------------- |
| Top    | 50%        | 128-byte  | GPU/Render assets           |
| Middle | 30%        | 64-byte   | Scene/Assets                |
| Bottom | 20%        | 8-byte    | Temporary/short-lived items |

### Global Offset Architecture

The implementation uses an offset-based addressing system that solves cross-platform memory management challenges:

```rust
pub struct LockFreeArena {
    base_offset: usize,  // Global offset from GLOBAL_MEMORY_BASE
}

pub struct MemoryHandle(usize);  // Always stores global offset
```

**Key Benefits:**

1. **WASM Memory Growth Safety**: Handles remain valid when WebAssembly memory grows
2. **Unified Handle Representation**: Consistent 64-bit representation across platforms
3. **Zero-Cost Platform Abstraction**: Platform-specific translation only at pointer conversion
4. **Serializable Handles**: Can be stored/transmitted as simple integers

**Platform Translation:**

```rust
impl MemoryHandle {
    pub fn to_ptr(self) -> *mut u8 {
        #[cfg(target_arch = "wasm32")]
        { self.0 as *mut u8 }  // Direct offset in WASM

        #[cfg(not(target_arch = "wasm32"))]
        { unsafe { GLOBAL_MEMORY_BASE.add(self.0) } }  // Base + offset
    }
}
```

### Allocation Strategy

The `LockFreeArena` employs a hybrid allocation approach:

1. **Primary**: Atomic bump allocation for O(1) performance
2. **Secondary**: Size-classed freelists for memory recycling
3. **Fallback**: Platform-specific memory growth (WASM only)

**Size Class Calculation:**

```rust
fn size_class_for(size: usize) -> usize {
    (size.max(32).trailing_zeros() as usize).saturating_sub(5).min(7)
}
```

This provides 8 size classes starting from 32 bytes, doubling each tier.

### Thread Safety Model

The allocator is thread-safe:

- All shared state uses atomic operations
- No data races possible through the public API
- Arena isolation prevents cross-arena interference
- Deallocation safety checks prevent cross-tier corruption

```rust
pub fn deallocate(&self, handle: MemoryHandle, size: usize) -> bool {
    let handle_offset = handle.offset();
    if handle_offset < self.base_offset ||
       handle_offset >= self.base_offset + self.size.load(Ordering::Relaxed) {
        return false;  // Prevents cross-arena deallocation
    }
}
```

**Memory Ordering:**

- `Relaxed` for stat updates
- `Acquire/Release` for freelist operations
- `SeqCst` only for reset operations

### Fast Memory Compaction

The `fast_compact_tier` function provides zero-cost memory recycling:

```rust
pub fn fast_compact_tier(&self, tier: Tier, preserve_bytes: usize) -> bool {
    arena.allocation_head.store(preserve_bytes, Ordering::SeqCst);

    // Clear freelists as they contain pointers beyond preserve_bytes
    for freelist in &arena.freelists {
        freelist.store(std::ptr::null_mut(), Ordering::SeqCst);
    }
}
```

**Visual Example:**

```
Before: |Important data (1MB)|Current objects (2MB)|Recently freed (1MB)|
After:  |Important data (1MB)|<-- Available for reuse (3MB) -->|
```

## Performance Optimizations

### SIMD Operations

Platform-specific SIMD acceleration:

| Copy Size           | Strategy                          |
| ------------------- | --------------------------------- |
| 1-32 bytes          | Direct unaligned loads/stores     |
| 33-128 bytes        | Overlapping 128-bit operations    |
| >128 bytes (x86_64) | AVX2 with 4x unrolling + prefetch |
| >128 bytes (WASM)   | SIMD128 with 4x16-byte unrolling  |

Prefetching activates for copies >4KB to optimize cache usage.

#### Performance Benchmarks

Real-world SIMD performance comparison between Native (x86_64 AVX2) and WASM (SIMD128):

| Buffer Size | Native Time (ns) | Native (MB/s) | WASM Time (ns) | WASM (MB/s) | Native Advantage |
| ----------- | ---------------- | ------------- | -------------- | ----------- | ---------------- |
| 6 bytes     | 25               | 228.88        | 261            | 22.90       | 10.4x faster     |
| 8 bytes     | 25               | 305.18        | 227            | 34.43       | 9.1x faster      |
| 13 bytes    | 26               | 476.84        | 210            | 60.36       | 8.1x faster      |
| 14 bytes    | 25               | 534.06        | 193            | 70.90       | 7.7x faster      |
| 19 bytes    | 25               | 724.79        | 197            | 93.99       | 7.9x faster      |
| 27 bytes    | 26               | 990.35        | 354            | 74.51       | 13.6x faster     |
| 32 bytes    | 24               | 1,271.57      | 172            | 181.34      | 7.2x faster      |
| 64 bytes    | 30               | 2,034.51      | 275            | 227.03      | 9.2x faster      |
| 100 bytes   | 26               | 3,667.98      | 194            | 502.25      | 7.5x faster      |
| 256 bytes   | 27               | 9,042.25      | 173            | 1,410.30    | 6.4x faster      |
| 1 KB        | 31               | 31,502.02     | 188            | 5,326.80    | 6.1x faster      |
| 4 KB        | 52               | 75,120.19     | 225            | 17,745.60   | 4.3x faster      |
| 16 KB       | 147              | 106,292.52    | 433            | 36,921.60   | 2.9x faster      |
| 64 KB       | 1,302            | 48,003.07     | 2,476          | 25,856.00   | 1.9x faster      |

**Key Observations:**

- Native AVX2 achieves remarkably consistent low latency (24-52ns) for buffers up to 4KB
- WASM SIMD128 shows more variable latency, particularly for small buffers
- The performance gap is most pronounced for small buffers (7-13x faster)
- For large buffers (64KB), both implementations are memory-bandwidth limited
- WASM still achieves respectable throughput (>25 GB/s) for large transfers

### Platform-Specific Optimizations

```rust
// Native: Global base pointer
#[cfg(not(target_arch = "wasm32"))]
static mut GLOBAL_MEMORY_BASE: *mut u8 = std::ptr::null_mut();

// WASM: Linear memory always starts at 0
#[cfg(target_arch = "wasm32")]
let memory_base = 0 as *mut u8;

// Null handling via MAX (0 is valid in WASM)
pub fn is_null(self) -> bool { self.0 == usize::MAX }
pub fn null() -> Self { MemoryHandle(usize::MAX) }
```

## Core API

### Memory Management

```rust
// Allocation
allocate(size: usize, tier: Tier) -> Option<MemoryHandle>
allocate_batch(requests: &[(usize, Tier)]) -> Vec<Option<MemoryHandle>>

// Memory recycling (WASM only)
fast_compact_tier(tier: Tier, preserve_bytes: usize) -> bool

// Tier management
reset_tier(tier: Tier)
tier_stats(tier: Tier) -> (usize, usize, usize, usize)
```

### Data Operations

```rust
write_data(handle: MemoryHandle, data: &[u8]) -> Result<(), &'static str>
read_data(handle: MemoryHandle, length: usize) -> Option<Vec<u8>>
bulk_copy(operations: &[(MemoryHandle, MemoryHandle, usize)])
```

### Asset Management

```rust
// Configuration
set_base_url(url: String)

// Asset operations
register_asset(key: String, metadata: AssetMetadata) -> bool
evict_asset(path: &str) -> bool
evict_assets_batch(paths: &[String]) -> usize
get_asset(path: &str) -> Option<AssetMetadata>

// Loading
load_asset(path: String, asset_type: AssetType) -> Result<MemoryHandle, String>
load_assets_batch(requests: Vec<(String, AssetType)>) -> Vec<Result<MemoryHandle, String>>
load_asset_zero_copy(data: &[u8], tier: Tier) -> Option<MemoryHandle>
```

## WebAssembly Integration

The `WallocWrapper` provides JavaScript-friendly bindings:

- Direct TypedArray access via `get_memory_view`
- Async asset loading with Promises
- Memory growth management
- Real-time statistics and diagnostics

## Binary Sizes

| Component   | Native | WASM  |
| ----------- | ------ | ----- |
| Raw Library | 55KB   | 55KB  |
| Rlib        | 286KB  | 822KB |
| Module      | 15KB   | 798KB |
| JS Glue     | -      | 30KB  |

## Setup & Testing

```bash
# Install prerequisites
rustup target add wasm32-unknown-unknown

# Build
cd walloc/
bash build.sh  # Choose (1) WASM or (2) Native

# Test
# Native: Auto-runs test binary
# WASM: Serve index.html and check console
```

## Security & Safety

- Bounds checking on all public APIs
- Arena boundary validation
- Null pointer protection with graceful degradation
- No buffer overruns through safe API
- Cross-tier corruption prevention

## Recommendations for Future Enhancement

1. **Configurable Tier Ratios**: Runtime memory distribution specification
2. **Block Coalescing**: Reduce long-term fragmentation
3. **Per-Tier Growth**: Independent tier expansion for WASM
4. **Allocation Profiling**: Track hot allocation sites
5. **API Documentation**: Explicit handle stability guarantees

## Design Philosophy

Walloc embodies these principles:

- **Ma (間)**: Empty space as design element.
- **Kanso (簡素)**: Simplicity - no decorative abstractions.
- **Shizen (自然)**: Naturalness - memory flows like water finding its level.
- **Shibui (渋い)**: Understated elegance - beauty in what's NOT there.
