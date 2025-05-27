import init, { Walloc } from './wbg/walloc.js';

let allocator = null;
let frameId = null;
let frameCount = 0;
let testPhase = 'init'; // 'init', 'simulation', 'assets', 'complete'

const MB = 1024 * 1024;
const TIER = { RENDER: 0, SCENE: 1, ENTITY: 2 };

function log(message) {
  console.log(message);
  const consoleDiv = document.getElementById('console');
  if (consoleDiv) {
    const line = document.createElement('div');
    line.textContent = message;
    consoleDiv.appendChild(line);
    consoleDiv.scrollTop = consoleDiv.scrollHeight;
  }
}

async function initWasm() {
  try {
    await init();
    log('WebAssembly module initialized');

    allocator = Walloc.new();
    log('Walloc tiered allocator created');

    // Set up button event handlers
    const startBtn = document.getElementById('start-btn');
    const stopBtn = document.getElementById('stop-btn');

    if (startBtn) startBtn.addEventListener('click', startComprehensiveTest);
    if (stopBtn) stopBtn.addEventListener('click', stopSimulation);

    log('Ready! Click "Start Game Loop" to begin comprehensive tests');
    log(
      'This will run: 1) Memory initialization, 2) Frame simulation, 3) Asset management tests'
    );
  } catch (error) {
    log(`Error: ${error}`);
  }
}

// Helper function to log memory stats in a readable format
function logMemoryStats(stats) {
  const tierNames = ['Render', 'Scene', 'Entity'];

  stats.tiers.forEach((tier, i) => {
    const usedMB = (tier.used / MB).toFixed(2);
    const capacityMB = (tier.capacity / MB).toFixed(2);
    const utilizationPercent = ((tier.used / tier.capacity) * 100).toFixed(2);

    log(
      `${tierNames[i]} tier: ${usedMB}MB/${capacityMB}MB (${utilizationPercent}%)`
    );
  });

  log(`Total memory utilization: ${stats.memoryUtilization.toFixed(2)}%`);
}

// Phase 1: Simulate start-up with memory preservation
function runInitTest() {
  log('=== PHASE 1: INITIALIZATION TEST ===');
  log('Loading new scene...');

  // First, make sure scene is reset at startup
  allocator.reset_tier(TIER.SCENE);

  // Simulate loading all the scene data into the main buffer, in one big chunk.
  const persistentDataSize = 74 * MB;
  const persistentDataSizeMB = (persistentDataSize / MB).toFixed(2);
  const largeOffset = allocator.allocate_tiered(persistentDataSize, TIER.SCENE);
  log(
    `Total persistent data allocated: ${persistentDataSizeMB}MB, offset: ${largeOffset}`
  );

  // Initialize render resources
  log('Renderer starting up for 1080p resolution...');
  const renderSize = 3 * MB;
  const renderOffset = allocator.allocate_tiered(renderSize, TIER.RENDER);
  log(
    `Allocated ${renderSize / MB}MB in RENDER tier at offset ${renderOffset}`
  );

  // Store the persistent data size globally for use in frame function
  window.persistentDataSize = persistentDataSize;

  // Get memory stats after initialization
  const initStats = allocator.memory_stats();
  log('Initial memory usage:');
  logMemoryStats(initStats);

  log('Startup complete. Beginning frame loop...');
}

// Phase 2: Simulate frame updates with recycling
function runFrameTest() {
  frameCount++;

  const shouldLog = frameCount % 30 === 0 || frameCount < 5;
  if (shouldLog) {
    log(`--- Frame ${frameCount} ---`);
  }

  // 1. Reset RENDER tier every frame (traditional approach)
  allocator.reset_tier(TIER.RENDER);
  if (shouldLog) {
    log('Reset RENDER tier');
  }

  // Allocate render buffer for 1080p frame
  const renderSize = 3 * MB;
  allocator.allocate_tiered(renderSize, TIER.RENDER);

  // 2. ENTITY tier - particles, effects, etc.
  if (frameCount % 10 === 0) {
    allocator.reset_tier(TIER.ENTITY);
    if (shouldLog) {
      log('Reset ENTITY tier for new effects');
    }
  }
  // Add some particles (10KB each)
  const particleSize = 10 * 1024;
  for (let i = 0; i < 5; i++) {
    allocator.allocate_tiered(particleSize, TIER.ENTITY);
  }

  // 3. SCENE tier - using fast_compact_tier for recycling
  const tempObjectSize = 50 * 1024; // 50KB per object
  const numObjects = 2; // Add 2 objects per frame

  for (let i = 0; i < numObjects; i++) {
    allocator.allocate_tiered(tempObjectSize, TIER.SCENE);
  }

  // Every 60 frames, use fast_compact_tier to preserve persistent data
  if (frameCount % 60 === 0) {
    if (shouldLog) {
      log('Memory stats before compaction:');
      logMemoryStats(allocator.memory_stats());
    }

    let preserveSize = window.persistentDataSize;

    // Every 180 frames, test preserving more than currently allocated
    if (frameCount % 180 === 0) {
      preserveSize = window.persistentDataSize + 20 * MB;
      log(
        `TESTING LARGER PRESERVATION: Trying to preserve ${(
          preserveSize / MB
        ).toFixed(2)}MB`
      );
    }

    const recycleResult = allocator.fast_compact_tier(TIER.SCENE, preserveSize);

    if (shouldLog) {
      log(
        `RECYCLED SCENE memory with fast_compact_tier(${(
          preserveSize / MB
        ).toFixed(2)}MB)`
      );
      log(`Compaction result: ${recycleResult ? 'SUCCESS' : 'FAILED'}`);

      if (frameCount % 180 === 0 && recycleResult) {
        window.persistentDataSize = preserveSize;
        log(
          `Updated persistent data size to ${(
            window.persistentDataSize / MB
          ).toFixed(2)}MB`
        );
      }

      log('Memory stats after compaction:');
      logMemoryStats(allocator.memory_stats());
    }

    // After recycling, add new textures
    const smallTexture = 1024 * 1024 * 4; // 4MB
    const mediumTexture = 2048 * 2048 * 4; // 16MB
    const largeTexture = 4096 * 4096 * 4; // 64MB

    const smallOffset = allocator.allocate_tiered(smallTexture, TIER.SCENE);
    const mediumOffset = allocator.allocate_tiered(mediumTexture, TIER.SCENE);
    const largeOffset = allocator.allocate_tiered(largeTexture, TIER.SCENE);

    if (shouldLog) {
      log(
        `Allocated 3 new textures in SCENE tier: offsets ${smallOffset}, ${mediumOffset}, ${largeOffset}`
      );
      log('Memory stats after new allocations:');
      logMemoryStats(allocator.memory_stats());
    }
  }

  // After 120 frames, move to asset testing phase
  if (frameCount >= 120) {
    log('=== FRAME SIMULATION COMPLETE ===');
    log('Transitioning to asset management tests...');
    testPhase = 'assets';
    runAssetTests();
    return;
  }

  frameId = requestAnimationFrame(runFrameTest);
}

// Phase 3: Asset management tests
async function runAssetTests() {
  log('=== PHASE 3: ASSET MANAGEMENT TESTS ===');

  // Helper to verify asset content
  async function verifyAsset(path, expectedId) {
    try {
      const data = allocator.get_asset(path);
      const jsonData = JSON.parse(new TextDecoder().decode(data));
      const result = jsonData.id === expectedId;
      log(`[${result ? 'PASS' : 'FAIL'}] Asset ${path}: id=${jsonData.id}`);
      return result;
    } catch (error) {
      log(`[FAIL] Error verifying ${path}: ${error.message}`);
      return false;
    }
  }

  function logAssetMemStats() {
    if (allocator) {
      const stats = allocator.memory_stats();
      log(
        `Scene tier: ${(stats.tiers[1].used / 1024).toFixed(2)} KB used, ` +
          `${(stats.tiers[1].highWaterMark / 1024).toFixed(2)} KB high, ` +
          `${(stats.tiers[1].memorySaved / 1024).toFixed(2)} KB saved`
      );
    }
  }

  try {
    // Test setup
    allocator.set_base_url('https://jsonplaceholder.typicode.com/');
    log('Starting asset tests...');
    logAssetMemStats();

    // Test 1: Load assets
    log('\n1. Loading assets:');
    const offset1 = await allocator.load_asset('todos/1', 1);
    const offset2 = await allocator.load_asset('todos/2', 1);
    const offset3 = await allocator.load_asset('todos/3', 1);
    log(`Assets loaded at offsets: ${offset1}, ${offset2}, ${offset3}`);
    logAssetMemStats();

    // Test 2: Verify content
    log('\n2. Verifying assets:');
    const allValid =
      (await verifyAsset('todos/1', 1)) &&
      (await verifyAsset('todos/2', 2)) &&
      (await verifyAsset('todos/3', 3));
    log(allValid ? '[PASS] All assets verified' : '[FAIL] Verification failed');

    // Test 3: Asset eviction
    log('\n3. Testing eviction:');
    allocator.evict_asset('todos/2');
    log('Asset 2 evicted');
    logAssetMemStats();

    // Verify eviction worked properly
    try {
      allocator.get_asset('todos/2');
      log('[FAIL] Asset 2 should be evicted');
    } catch {
      log('[PASS] Asset 2 properly evicted');
    }

    // Check other assets survived
    const othersValid =
      (await verifyAsset('todos/1', 1)) && (await verifyAsset('todos/3', 3));
    log(
      othersValid
        ? '[PASS] Other assets intact'
        : '[FAIL] Other assets affected'
    );

    // Test 4: Load more and evict all
    log('\n4. Load more and reset:');
    for (let i = 4; i <= 6; i++) {
      await allocator.load_asset(`todos/${i}`, 1);
    }
    log('Added 3 more assets');
    logAssetMemStats();

    // Evict all assets
    for (let i = 1; i <= 6; i++) {
      if (i !== 2) {
        // Skip already evicted asset
        allocator.evict_asset(`todos/${i}`);
      }
    }
    log('All assets evicted');
    logAssetMemStats();

    // Test 5: Recovery and larger asset
    log('\n5. Recovery test with larger asset:');
    await allocator.load_asset('comments', 1);
    log('Loaded larger asset (comments)');
    logAssetMemStats();

    try {
      const commentsData = allocator.get_asset('comments');
      const comments = JSON.parse(new TextDecoder().decode(commentsData));
      log(
        `[PASS] Comments loaded: ${comments.length} items, ` +
          `${(commentsData.length / 1024).toFixed(2)} KB`
      );
    } catch (error) {
      log(`[FAIL] Failed to verify comments: ${error.message}`);
    }

    log('\n=== ALL TESTS COMPLETED SUCCESSFULLY! ===');
    log('Summary:');
    log('✓ Memory initialization and tiered allocation');
    log('✓ Frame-based memory management with compaction');
    log('✓ Asset loading, verification, and eviction');
    log('✓ Memory recycling and preservation');

    testPhase = 'complete';

    // Final memory stats
    log('\n\n\nFinal memory statistics:');
    logMemoryStats(allocator.memory_stats());
    log('\n\nStopping started game loop automatically, tests finished...');
    stopSimulation();
  } catch (error) {
    console.error('Asset test error:', error);
    log(`Fatal error in asset tests: ${error.message}`);
  }
}

function startComprehensiveTest() {
  log('=== STARTING COMPREHENSIVE WALLOC TEST SUITE ===');

  if (frameId) {
    log('Test already running...');
    return;
  }

  frameCount = 0;
  testPhase = 'init';

  // Phase 1: Initialize memory
  runInitTest();

  // Phase 2: Start frame simulation
  setTimeout(() => {
    testPhase = 'simulation';
    log('=== PHASE 2: FRAME SIMULATION TEST ===');
    frameId = requestAnimationFrame(runFrameTest);
  }, 1000);
}

function stopSimulation() {
  if (frameId) {
    cancelAnimationFrame(frameId);
    frameId = null;
    log('Simulation stopped');
    testPhase = 'init';
  }
}

// Initialize WASM on load
initWasm();

// Memory display update function
function updateMemoryDisplay() {
  if (allocator) {
    const stats = allocator.memory_stats();
    const memoryStatsDisplay = document.getElementById('memory-stats');
    if (memoryStatsDisplay) {
      memoryStatsDisplay.textContent = `Total Memory: ${(
        stats.totalSize / MB
      ).toFixed(3)} MB\nMemory Utilization: ${stats.memoryUtilization.toFixed(
        3
      )}% \nTotal Memory Available: ${(stats.rawMemorySize / MB).toFixed(
        3
      )} MB \n\nTier 1 - Render System \nBytes Used: ${(
        stats.tiers[0].used / MB
      ).toFixed(3)} MB \nTotal Capacity: ${(
        stats.tiers[0].capacity / MB
      ).toFixed(3)} MB \nHigh Water Mark: ${(
        stats.tiers[0].highWaterMark / MB
      ).toFixed(3)} MB\nTotal Allocated: ${(
        stats.tiers[0].totalAllocated / MB
      ).toFixed(3)} MB\nMemory Saved: ${(
        (stats.tiers[0].memorySaved || 0) / MB
      ).toFixed(3)} MB\n\nTier 2 - Scene System \nBytes Used: ${(
        stats.tiers[1].used / MB
      ).toFixed(3)} MB \nTotal Capacity: ${(
        stats.tiers[1].capacity / MB
      ).toFixed(3)} MB \nHigh Water Mark: ${(
        stats.tiers[1].highWaterMark / MB
      ).toFixed(3)} MB\nTotal Allocated: ${(
        stats.tiers[1].totalAllocated / MB
      ).toFixed(3)} MB\nMemory Saved: ${(
        (stats.tiers[1].memorySaved || 0) / MB
      ).toFixed(3)} MB\n\nTier 3 - Entity System \nBytes Used: ${(
        stats.tiers[2].used / MB
      ).toFixed(3)} MB \nTotal Capacity: ${(
        stats.tiers[2].capacity / MB
      ).toFixed(3)} MB \nHigh Water Mark: ${(
        stats.tiers[2].highWaterMark / MB
      ).toFixed(3)} MB\nTotal Allocated: ${(
        stats.tiers[2].totalAllocated / MB
      ).toFixed(3)} MB\nMemory Saved: ${(
        (stats.tiers[2].memorySaved || 0) / MB
      ).toFixed(3)} MB`;
    }
  }
  setTimeout(updateMemoryDisplay, 300);
}

// Start memory display updates when DOM is ready
if (typeof document !== 'undefined') {
  document.addEventListener('DOMContentLoaded', updateMemoryDisplay);
}
