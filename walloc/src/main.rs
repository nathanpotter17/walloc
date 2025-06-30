#[cfg(not(target_arch = "wasm32"))]
use walloc::{create_walloc, Tier, AssetType, AssetMetadata};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Barrier};
#[cfg(not(target_arch = "wasm32"))]
use std::thread;

#[cfg(target_arch = "wasm32")]
fn main() {
    println!("WASM builds don't support the main binary - use `bash build.sh 1` instead");
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Enhanced Walloc Test Suite");
    
    let start = Instant::now();
    
    // Create walloc and convert to Arc for new features
    let walloc = create_walloc()?
        .with_base_url("https://jsonplaceholder.typicode.com/".to_string())
        .into_arc();
    println!("Allocator created in {:?}", start.elapsed());

    // Test 1: Basic allocation across tiers
    print!("Testing tier allocations... ");
    let render_handle = walloc.allocate(1024, Tier::Top).expect("Failed to allocate render memory");
    let scene_handle = walloc.allocate(2048, Tier::Middle).expect("Failed to allocate scene memory");
    let temp_handle = walloc.allocate(512, Tier::Bottom).expect("Failed to allocate temp memory");
    println!("✓");

    // Test 2: Memory operations
    print!("Testing memory write/read... ");
    let test_data = b"Hello, Walloc! This is a test string for memory operations.";
    walloc.write_data(scene_handle, test_data)?;
    let read_data = walloc.read_data(scene_handle, test_data.len()).expect("Failed to read data");
    assert_eq!(test_data, read_data.as_slice());
    println!("✓");

    // NEW Test 3: Memory owner tracking
    print!("Testing memory owner tracking... ");
    let (_, _, _, allocated_start) = walloc.tier_stats(Tier::Middle);
    {
        // Create allocations with owner
        let (_owner1, handle1) = walloc.allocate_with_owner(1024, Tier::Middle)
            .expect("Failed to allocate with owner");
        let (_owner2, handle2) = walloc.allocate_with_owner(2048, Tier::Middle)
            .expect("Failed to allocate with owner");
        
        // Write data to verify handles work
        walloc.write_data(handle1, b"Owner 1 data")?;
        walloc.write_data(handle2, b"Owner 2 data")?;
        
        let (_, _, _, allocated_with_owners) = walloc.tier_stats(Tier::Middle);
        assert!(allocated_with_owners > allocated_start, "Memory should be allocated");
        
        // Drop owner1 explicitly - memory should be freed
        drop(_owner1);
        
        // Give deallocation time to complete
        std::thread::sleep(std::time::Duration::from_millis(10));
        
        // On native, memory is immediately deallocated
        // On WASM, compaction would happen if > 64KB was freed
        let (_, _, _, allocated_after_drop) = walloc.tier_stats(Tier::Middle);
        
        // Owner2 is still alive, so some memory should still be allocated
        assert!(allocated_after_drop < allocated_with_owners, "Some memory should be freed");
        assert!(allocated_after_drop >= allocated_start, "Owner2's memory should still be allocated");
    }
    // Both owners dropped - all memory should be freed
    std::thread::sleep(std::time::Duration::from_millis(10));
    
    let (_, _, _, allocated_final) = walloc.tier_stats(Tier::Middle);
    assert_eq!(allocated_final, allocated_start, "All memory should be freed after owners drop");
    println!("✓");

    // NEW Test 5: Fast compact tier with data preservation
    print!("Testing fast_compact_tier... ");
    {
        // Allocate some data in Middle tier
        let data1 = b"Important data to preserve";
        let handle1 = walloc.allocate(data1.len(), Tier::Middle).unwrap();
        walloc.write_data(handle1, data1)?;
        
        let data2 = b"Another important piece";
        let handle2 = walloc.allocate(data2.len(), Tier::Middle).unwrap();
        walloc.write_data(handle2, data2)?;
        
        let (used_before, _, _, _) = walloc.tier_stats(Tier::Middle);
        
        // Compact to preserve only the first allocation
        let preserve_bytes = data1.len() + 64; // Add some padding for alignment
        let compact_result = walloc.fast_compact_tier(Tier::Middle, preserve_bytes);
        
        let (used_after, _, _, _) = walloc.tier_stats(Tier::Middle);
        
        // On native, this just updates the allocation head
        // On WASM, it would actually compact and preserve data
        assert!(compact_result, "Compaction should succeed");
        assert!(used_after <= used_before, "Usage pointer should not increase");
        
        // Verify we can still allocate
        let new_handle = walloc.allocate(32, Tier::Middle).unwrap();
        walloc.write_data(new_handle, b"New data after compact")?;
    }
    println!("✓");

    // Test 6: SIMD operations (simplified)
    print!("Testing SIMD operations... ");
    let simd_sizes = [8, 32, 128, 1024, 4096, 65536];
    
    for size in &simd_sizes {
        let src_data = vec![0x42u8; *size];
        let src_handle = walloc.allocate(*size, Tier::Middle).unwrap();
        let dst_handle = walloc.allocate(*size, Tier::Middle).unwrap();
        
        walloc.write_data(src_handle, &src_data)?;
        
        unsafe {
            walloc.bulk_copy(&[(src_handle, dst_handle, *size)]);
        }
        
        let dst_data = walloc.read_data(dst_handle, *size).unwrap();
        assert_eq!(src_data, dst_data, "SIMD copy failed for size {}", size);
    }
    println!("✓");

    // Test 7: Asset eviction with platform-aware compaction
    print!("Testing asset eviction... ");
    {
        // Register multiple assets
        let asset_count = 5;
        for i in 0..asset_count {
            let data = format!("Asset data {}", i).into_bytes();
            let handle = walloc.allocate(data.len(), Tier::Middle).unwrap();
            walloc.write_data(handle, &data)?;
            
            let metadata = AssetMetadata {
                asset_type: AssetType::Binary,
                size: data.len(),
                offset: handle.offset(),
                tier: Tier::Middle,
                handle,
            };
            
            walloc.register_asset(format!("asset_{}", i), metadata);
        }
        
        // Verify all registered
        for i in 0..asset_count {
            assert!(walloc.get_asset(&format!("asset_{}", i)).is_some());
        }
        
        // Evict middle asset - on WASM this would compact
        assert!(walloc.evict_asset("asset_2"));
        assert!(walloc.get_asset("asset_2").is_none());
        
        // Others should still exist
        assert!(walloc.get_asset("asset_1").is_some());
        assert!(walloc.get_asset("asset_3").is_some());
        
        // Batch eviction
        let evicted = walloc.evict_assets_batch(&[
            "asset_0".to_string(),
            "asset_4".to_string(),
            "nonexistent".to_string(),
        ]);
        assert_eq!(evicted, 2);
    }
    println!("✓");

    // Test 8: HTTP asset loading (if network available)
    print!("Testing HTTP asset loading... ");
    // NOTE: Base URL is already set to jsonplaceholder.typicode.com
    // Using 'posts/1' endpoint which returns JSON data
    match walloc.load_asset("posts/1".to_string(), AssetType::Json).await {
        Ok(handle) => {
            let data = walloc.read_data(handle, 100).unwrap_or_default();
            println!("Success! Loaded {} bytes", data.len());
            
            // Try test_fetch_json as well
            match walloc.test_fetch_json().await {
                Ok(json) => println!("test_fetch_json success: {} chars", json.len()),
                Err(e) => println!("test_fetch_json failed: {}", e),
            }
        }
        Err(e) => println!("Network test failed: {}", e),
    }

    // Test 9: Memory stats
    print!("Memory statistics:\n");
    for tier in [Tier::Top, Tier::Middle, Tier::Bottom] {
        let (used, capacity, peak, total) = walloc.tier_stats(tier);
        let tier_name = match tier {
            Tier::Top => "Render  ",
            Tier::Middle => "Scene   ",
            Tier::Bottom => "Temp    ",
        };
        println!("   {} | Used: {:>8} | Cap: {:>8} | Peak: {:>8} | Total: {:>8}", 
                tier_name, used, capacity, peak, total);
    }
    println!("   Overall utilization: {:.2}%\n", walloc.memory_utilization());

    // Test 10: Concurrent operations with Arc
    print!("Testing concurrent operations... ");
    {
        let barrier = Arc::new(Barrier::new(3));
        let threads: Vec<_> = (0..3).map(|thread_id| {
            let walloc_clone = Arc::clone(&walloc);
            let barrier_clone = Arc::clone(&barrier);
            
            thread::spawn(move || {
                barrier_clone.wait();
                
                // Each thread allocates and registers assets
                for i in 0..10 {
                    let key = format!("thread_{}_asset_{}", thread_id, i);
                    if let Some(handle) = walloc_clone.allocate(64, Tier::Bottom) {
                        let metadata = AssetMetadata {
                            asset_type: AssetType::Binary,
                            size: 64,
                            offset: handle.offset(),
                            tier: Tier::Bottom,
                            handle,
                        };
                        walloc_clone.register_asset(key, metadata);
                    }
                }
            })
        }).collect();
        
        for thread in threads {
            thread.join().unwrap();
        }
        
        // Count registered assets
        let mut count = 0;
        for tid in 0..3 {
            for i in 0..10 {
                if walloc.get_asset(&format!("thread_{}_asset_{}", tid, i)).is_some() {
                    count += 1;
                }
            }
        }
        println!("✓ ({} assets registered)", count);
    }

    // Test 11: Error handling
    print!("Testing error conditions... ");
    {
        let huge_alloc = walloc.allocate(1_000_000_000, Tier::Top);
        assert!(huge_alloc.is_none(), "Should fail huge allocation");
        
        let invalid_write = walloc.write_data(walloc::MemoryHandle::null(), b"test");
        assert!(invalid_write.is_err(), "Should fail null write");
        
        // Test invalid tier in fast_compact
        assert!(!walloc.fast_compact_tier(Tier::Top, usize::MAX));
    }
    println!("✓");

    // Test 12: Bulk copy operations
    print!("Testing bulk copy operations... ");
    let src_handle = walloc.allocate(1024, Tier::Middle).unwrap();
    let dst_handle = walloc.allocate(1024, Tier::Bottom).unwrap();
    let bulk_data = vec![0x42; 1024];
    walloc.write_data(src_handle, &bulk_data)?;
    
    unsafe {
        walloc.bulk_copy(&[(src_handle, dst_handle, 1024)]);
    }
    
    let copied_data = walloc.read_data(dst_handle, 1024).unwrap();
    assert_eq!(bulk_data, copied_data);
    println!("✓");

    println!("\nAll tests completed in {:?}", start.elapsed());
    
    Ok(())
}