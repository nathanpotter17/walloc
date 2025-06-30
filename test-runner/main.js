import init, { WallocWrapper } from './wbg/walloc.js';

let walloc = null;

const TIER = {
  TOP: 0,
  MIDDLE: 1,
  BOTTOM: 2,
};

const ASSET_TYPE = {
  IMAGE: 0,
  JSON: 1,
  BINARY: 2,
};

function log(message, type = 'info') {
  const time = performance.now();
  const consoleDiv = document.getElementById('console');
  if (consoleDiv) {
    const line = document.createElement('div');
    line.textContent = `${time.toFixed(2)}ms : ${type} : ${message}`;
    consoleDiv.appendChild(line);
    consoleDiv.scrollTop = consoleDiv.scrollHeight;
  }
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(`Assertion failed: ${message}`);
  }
}

function isValidHandle(handle) {
  return handle !== null && handle !== undefined && handle !== 0xffffffff;
}

async function test1_BasicAllocation() {
  log('Test 1: Basic allocation across tiers...');

  const renderHandle = walloc.allocate(1024, TIER.TOP);
  assert(isValidHandle(renderHandle), 'Failed to allocate render memory');

  const sceneHandle = walloc.allocate(2048, TIER.MIDDLE);
  assert(isValidHandle(sceneHandle), 'Failed to allocate scene memory');

  const tempHandle = walloc.allocate(512, TIER.BOTTOM);
  assert(isValidHandle(tempHandle), 'Failed to allocate temp memory');

  log('✓ Basic allocation test passed', 'success');
}

async function test2_MemoryOperations() {
  log('Test 2: Memory write/read operations...');

  const testData = new TextEncoder().encode(
    'Hello, Walloc! This is a test string for memory operations.'
  );
  const handle = walloc.allocate(testData.length, TIER.MIDDLE);
  assert(isValidHandle(handle), 'Failed to allocate memory');

  // Write data
  walloc.write_memory(handle, testData);

  // Read data back
  const readData = walloc.get_memory_view(handle, testData.length);

  assert(readData.length === testData.length, 'Read data length mismatch');
  for (let i = 0; i < testData.length; i++) {
    assert(readData[i] === testData[i], `Data mismatch at index ${i}`);
  }

  log('✓ Memory operations test passed', 'success');
}

async function test3_MemoryOwnerTracking() {
  log('Test 3: Memory owner tracking...');

  const stats1 = walloc.memory_stats();
  const allocatedStart = stats1.totalUsed;

  // Create allocations with owner
  const result1 = walloc.allocate_with_owner(1024, TIER.MIDDLE);
  assert(result1 !== null, 'Failed to allocate with owner');
  assert(
    isValidHandle(result1.offset),
    'Invalid handle from allocate_with_owner'
  );

  const result2 = walloc.allocate_with_owner(2048, TIER.MIDDLE);
  assert(result2 !== null, 'Failed to allocate with owner');
  assert(
    isValidHandle(result2.offset),
    'Invalid handle from allocate_with_owner'
  );

  // Write data to verify handles work
  const data1 = new Uint8Array([1, 2, 3, 4]);
  const data2 = new Uint8Array([5, 6, 7, 8]);
  walloc.write_memory(result1.offset, data1);
  walloc.write_memory(result2.offset, data2);

  const stats2 = walloc.memory_stats();
  assert(stats2.totalUsed > allocatedStart, 'Memory should be allocated');

  // Note: In JS/WASM, we can't directly free the owners like in Rust
  // The memory will be freed when the owner objects are garbage collected
  // For testing purposes, we'll just verify the allocations worked

  log(
    '✓ Memory owner tracking test passed (Note: JS GC handles cleanup)',
    'success'
  );
}

async function test4_FastCompactTier() {
  log('Test 4: Fast compact tier with data preservation...');

  // Allocate some data in Middle tier
  const data1 = new TextEncoder().encode('Important data to preserve');
  const handle1 = walloc.allocate(data1.length, TIER.MIDDLE);
  walloc.write_memory(handle1, data1);

  const data2 = new TextEncoder().encode('Another important piece');
  const handle2 = walloc.allocate(data2.length, TIER.MIDDLE);
  walloc.write_memory(handle2, data2);

  const statsBefore = walloc.memory_stats();
  const tierBefore = statsBefore.tiers.find((t) => t.name === 'scene');
  const usedBefore = tierBefore.used;

  // Compact to preserve only the first allocation
  const preserveBytes = data1.length + 64; // Add some padding for alignment
  const compactResult = walloc.fast_compact_tier(TIER.MIDDLE, preserveBytes);

  const statsAfter = walloc.memory_stats();
  const tierAfter = statsAfter.tiers.find((t) => t.name === 'scene');
  const usedAfter = tierAfter.used;

  assert(compactResult === true, 'Compaction should succeed');
  assert(usedAfter <= usedBefore, 'Usage pointer should not increase');

  // Verify we can still allocate
  const newHandle = walloc.allocate(32, TIER.MIDDLE);
  assert(
    isValidHandle(newHandle),
    'Should be able to allocate after compaction'
  );
  walloc.write_memory(
    newHandle,
    new TextEncoder().encode('New data after compact')
  );

  log('✓ Fast compact tier test passed', 'success');
}

async function test5_SimdOperations() {
  log('Test 5: SIMD operations (bulk copy)...');

  const simdSizes = [8, 32, 128, 1024, 4096, 65536];

  for (const size of simdSizes) {
    const srcData = new Uint8Array(size).fill(0x42);
    const srcHandle = walloc.allocate(size, TIER.MIDDLE);
    const dstHandle = walloc.allocate(size, TIER.MIDDLE);

    walloc.write_memory(srcHandle, srcData);

    // Read destination data and verify copy
    const dstData = walloc.get_memory_view(dstHandle, size);

    // Note: bulk_copy is not exposed in WASM bindings
    // We'll test the SIMD operations indirectly through write/read
    walloc.write_memory(dstHandle, walloc.get_memory_view(srcHandle, size));

    const copiedData = walloc.get_memory_view(dstHandle, size);
    assert(
      copiedData.length === srcData.length,
      `Length mismatch for size ${size}`
    );
    for (let i = 0; i < size; i++) {
      assert(
        copiedData[i] === srcData[i],
        `SIMD copy failed at index ${i} for size ${size}`
      );
    }
  }

  log('✓ SIMD operations test passed', 'success');
}

async function test6_AssetEviction() {
  log('Test 6: Asset eviction with platform-aware compaction...');

  // Register multiple assets
  const assetCount = 5;
  for (let i = 0; i < assetCount; i++) {
    const data = new TextEncoder().encode(`Asset data ${i}`);
    const handle = walloc.allocate(data.length, TIER.MIDDLE);
    walloc.write_memory(handle, data);

    walloc.register_asset(
      `asset_${i}`,
      ASSET_TYPE.BINARY,
      data.length,
      handle,
      TIER.MIDDLE
    );
  }

  // Verify all registered
  for (let i = 0; i < assetCount; i++) {
    try {
      const data = walloc.get_asset_data(`asset_${i}`);
      assert(data !== null, `Asset ${i} should be registered`);
    } catch (e) {
      assert(false, `Asset ${i} should be registered but threw error: ${e}`);
    }
  }

  // Evict middle asset - on WASM this would compact
  assert(walloc.evict_asset('asset_2') === true, 'Should evict asset_2');

  // Verify asset_2 is gone
  try {
    walloc.get_asset_data('asset_2');
    assert(false, 'asset_2 should not exist');
  } catch (e) {
    // Expected - asset should be gone
  }

  // Others should still exist
  try {
    walloc.get_asset_data('asset_1');
    walloc.get_asset_data('asset_3');
  } catch (e) {
    assert(false, 'Other assets should still exist');
  }

  // Batch eviction
  const evictList = new Array();
  evictList.push('asset_0');
  evictList.push('asset_4');
  evictList.push('nonexistent');

  const evicted = walloc.evict_assets_batch(evictList);
  assert(evicted === 2, 'Should evict 2 assets');

  log('✓ Asset eviction test passed', 'success');
}

async function test7_HttpAssetLoading() {
  log('Test 7: HTTP asset loading...');

  try {
    // Using JSONPlaceholder API which is already set as base URL
    const handle = await walloc.load_asset('posts/1', ASSET_TYPE.JSON);
    assert(isValidHandle(handle), 'Should get valid handle from load_asset');

    // Try to read the loaded data
    const data = walloc.get_memory_view(handle, 100);
    log(
      `✓ HTTP asset loading test passed - loaded ${data.length} bytes`,
      'success'
    );

    // Try test_http_connection as well
    try {
      const result = await walloc.test_http_connection();
      log(`test_http_connection success: ${result.length} chars`, 'info');
    } catch (e) {
      log(`test_http_connection failed: ${e}`, 'error');
    }
  } catch (e) {
    log(`Network test failed: ${e}`, 'error');
  }
}

async function test8_MemoryStats() {
  log('Test 8: Memory statistics...', 'stats');

  const stats = walloc.memory_stats();

  log(`   Allocator Type: ${stats.allocatorType}`, 'stats');
  log(`   Total Memory Pages: ${stats.pages}`, 'stats');
  log(`   Total Memory Size: ${stats.rawMemorySize} bytes`, 'stats');
  log(`   Total Used: ${stats.totalUsed} bytes`, 'stats');
  log(`   Memory Utilization: ${stats.memoryUtilization.toFixed(2)}%`, 'stats');

  // Display tier stats
  for (const tier of stats.tiers) {
    log(
      `   ${tier.name.padEnd(8)} | Used: ${tier.used
        .toString()
        .padStart(8)} | Cap: ${tier.capacity
        .toString()
        .padStart(8)} | Peak: ${tier.highWaterMark
        .toString()
        .padStart(8)} | Total: ${tier.totalAllocated
        .toString()
        .padStart(8)} | Saved: ${tier.memorySaved.toString().padStart(8)}`,
      'stats'
    );
  }
}

async function test9_ConcurrentOperations() {
  log('Test 9: Concurrent operations (simulated with async)...');

  // JavaScript doesn't have true threads, but we can simulate concurrent operations
  const promises = [];

  for (let threadId = 0; threadId < 3; threadId++) {
    const promise = (async () => {
      // Each "thread" allocates and registers assets
      for (let i = 0; i < 10; i++) {
        const key = `thread_${threadId}_asset_${i}`;
        const handle = walloc.allocate(64, TIER.BOTTOM);
        if (isValidHandle(handle)) {
          walloc.register_asset(
            key,
            ASSET_TYPE.BINARY,
            64,
            handle,
            TIER.BOTTOM
          );
        }
      }
    })();
    promises.push(promise);
  }

  await Promise.all(promises);

  // Count registered assets
  let count = 0;
  for (let tid = 0; tid < 3; tid++) {
    for (let i = 0; i < 10; i++) {
      try {
        walloc.get_asset_data(`thread_${tid}_asset_${i}`);
        count++;
      } catch (e) {
        // Asset not found
      }
    }
  }

  log(
    `✓ Concurrent operations test passed (${count} assets registered)`,
    'success'
  );
}

async function test10_ErrorHandling() {
  log('Test 10: Error handling...');

  // Test huge allocation
  const hugeAlloc = walloc.allocate(1_000_000_000, TIER.TOP);
  assert(!isValidHandle(hugeAlloc), 'Should fail huge allocation');

  // Test invalid write
  try {
    walloc.write_memory(0xffffffff, new Uint8Array([1, 2, 3]));
    assert(false, 'Should have thrown on invalid handle write');
  } catch (e) {
    // Expected error
  }

  // Test invalid tier in fast_compact
  const invalidCompact = walloc.fast_compact_tier(
    TIER.TOP,
    Number.MAX_SAFE_INTEGER
  );
  assert(invalidCompact === false, 'Should fail invalid compaction');

  // Test non-existent asset
  try {
    walloc.get_asset_data('non_existent_asset');
    assert(false, 'Should throw for non-existent asset');
  } catch (e) {
    // Expected error
  }

  log('✓ Error handling test passed', 'success');
}

async function test11_ZeroCopyOperations() {
  log('Test 11: Zero-copy operations...');

  const testData = new Uint8Array(1024).fill(0x42);
  const handle = walloc.load_asset_zero_copy(testData, TIER.MIDDLE);

  assert(isValidHandle(handle), 'Zero-copy allocation should succeed');

  const readData = walloc.get_memory_view(handle, testData.length);
  assert(readData.length === testData.length, 'Zero-copy length mismatch');

  for (let i = 0; i < testData.length; i++) {
    assert(
      readData[i] === testData[i],
      `Zero-copy data mismatch at index ${i}`
    );
  }

  log('✓ Zero-copy operations test passed', 'success');
}

async function test12_TierReset() {
  log('Test 12: Tier reset functionality...');

  // Allocate some memory in each tier
  const handles = [];
  for (let i = 0; i < 5; i++) {
    handles.push(walloc.allocate(1024, TIER.MIDDLE));
  }

  const statsBefore = walloc.memory_stats();
  const tierBefore = statsBefore.tiers.find((t) => t.name === 'scene');
  assert(tierBefore.used > 0, 'Should have allocations before reset');

  // Reset the tier
  const resetResult = walloc.reset_tier(TIER.MIDDLE);
  assert(resetResult === true, 'Reset should succeed');

  const statsAfter = walloc.memory_stats();
  const tierAfter = statsAfter.tiers.find((t) => t.name === 'scene');
  assert(tierAfter.used === 0, 'Tier should be empty after reset');

  // Verify we can allocate again
  const newHandle = walloc.allocate(1024, TIER.MIDDLE);
  assert(isValidHandle(newHandle), 'Should be able to allocate after reset');

  log('✓ Tier reset test passed', 'success');
}

async function runAllTests() {
  try {
    const startTime = performance.now();
    log('Enhanced Walloc WASM Test Suite', 'info');

    // Initialize WASM module
    await init();

    // Create walloc with base URL using new constructor
    walloc = WallocWrapper.new_with_base_url(
      'https://jsonplaceholder.typicode.com/'
    );

    log(
      `Allocator created in ${(performance.now() - startTime).toFixed(2)}ms`,
      'info'
    );

    // Run all tests
    await test1_BasicAllocation();
    await test2_MemoryOperations();
    await test3_MemoryOwnerTracking();
    await test4_FastCompactTier();
    await test5_SimdOperations();
    await test6_AssetEviction();
    await test7_HttpAssetLoading();
    await test8_MemoryStats();
    await test9_ConcurrentOperations();
    await test10_ErrorHandling();
    await test11_ZeroCopyOperations();
    await test12_TierReset();

    const totalTime = performance.now() - startTime;
    log(`\nAll tests completed in ${totalTime.toFixed(2)}ms`, 'success');
  } catch (error) {
    log(`Test failed: ${error.message}`, 'error');
    console.error(error);
  }
}

// Make runAllTests available globally
window.runAllTests = runAllTests;

// Auto-run tests on load
window.addEventListener('load', runAllTests);
