import init, { Walloc } from './wbg/walloc.js';

let startTime = null;
let allocator = null;
let allocations = [];

const MB = 1024 * 1024;
const GB = 1024 * MB;

const logToConsole = (str) => {
  if (startTime === null) {
    startTime = performance.now();
  }
  const elapsed = performance.now() - startTime;
  const ns = `${elapsed}ms | ${str}`;
  const consoleDiv = document.getElementById('console');
  const span = document.createElement('div');
  span.textContent = ns;
  consoleDiv.appendChild(span);
  consoleDiv.scrollTop = consoleDiv.scrollHeight;
  console.log(ns);
};

async function initWasm() {
  try {
    // Initialize the WebAssembly module
    await init();
    logToConsole('WebAssembly module initialized successfully');

    // Create an instance of the Walloc allocator
    allocator = Walloc.new();
    logToConsole('Walloc allocator created');

    testSuite();
  } catch (error) {
    logToConsole(`Error initializing WebAssembly module: ${error}`);
  }
}

function testSuite() {
  // Start with current memory size
  let stats = allocator.memory_stats();
  let currentSize = stats.totalSize;
  logToConsole(`Starting allocation test with ${currentSize / MB} MB`);

  // Approach 1 - One Chunk
  testSingleMaxAlloc();

  // Approach 2 - Incrementally size down chunk allocation
  // testRepeatedAlloc();

  // Approach 3 - Find the maximum single allocation possible using binary search
  // testMaxSingleAllocation();

  // Clean up all allocations
  logToConsole('Cleaning up allocations...');
  allocations.forEach((alloc) => {
    try {
      allocator.free(alloc.offset);
    } catch (e) {
      logToConsole(
        `Error freeing allocation at offset ${alloc.offset}: ${e.message}`
      );
    }
  });
}

function testMaxSingleAllocation() {
  const MB = 1024 * 1024;

  let min = 1 * MB; // Start with 1MB
  let max = 2048 * MB; // 2GB max
  let best = 0;
  let bestOffset = 0;

  logToConsole('Starting binary search for maximum single allocation...');

  while (min <= max) {
    const mid = Math.floor((min + max) / 2);
    logToConsole(`Trying allocation of ${mid / MB} MB...`);

    try {
      const offset = allocator.allocate(mid);
      if (offset !== 0) {
        // Success, try larger
        best = mid;
        bestOffset = offset;
        min = mid + 1;
        logToConsole(`SUCCESS: Allocated ${mid / MB} MB at offset ${offset}`);
        allocator.free(offset);
      } else {
        // Failed, try smaller
        max = mid - 1;
        logToConsole(`FAILED: Could not allocate ${mid / MB} MB`);
      }
    } catch (e) {
      // Error, try smaller
      max = mid - 1;
      logToConsole(
        `ERROR: Exception when allocating ${mid / MB} MB: ${e.message}`
      );
    }
  }

  logToConsole(
    `Maximum single allocation: ${best / MB} MB (${best / (1024 * MB)} GB)`
  );

  // Try to allocate the best size again to verify
  if (best > 0) {
    try {
      const finalOffset = allocator.allocate(best);
      if (finalOffset !== 0) {
        logToConsole(
          `Verified maximum allocation: ${
            best / MB
          } MB at offset ${finalOffset}`
        );

        allocator.free(finalOffset);
      }
    } catch (e) {
      logToConsole(`Error verifying maximum allocation: ${e.message}`);
    }
  }
}

function runRegularTests() {
  logToConsole('Running Rudimentary Test Suite...');

  // Get and display memory stats
  const stats = allocator.memory_stats();
  logToConsole(`Total memory size: ${stats.totalSize} bytes`);
  logToConsole(`Allocator type: ${stats.allocatorType}`);

  // Example 1: Allocate a simple buffer and write data to it
  const bufferSize = 100; // bytes
  const offset = allocator.allocate(bufferSize);
  logToConsole(`Allocated buffer at offset: ${offset}`);

  // Create some data to write to the buffer
  const data = new Uint8Array(bufferSize);
  for (let i = 0; i < bufferSize; i++) {
    data[i] = i % 256; // Fill with sequential values
  }

  // Copy data from JavaScript to WebAssembly memory
  allocator.copy_from_js(offset, data);
  logToConsole('Data written to WebAssembly memory');

  // Read the data back to verify
  const readBack = allocator.copy_to_js(offset, bufferSize);
  logToConsole('Data read back from WebAssembly memory');

  // Verify data integrity
  let dataMatches = true;
  for (let i = 0; i < bufferSize; i++) {
    if (data[i] !== readBack[i]) {
      dataMatches = false;
      logToConsole(`Data mismatch at index ${i}: ${data[i]} vs ${readBack[i]}`);
      break;
    }
  }
  if (dataMatches) {
    logToConsole('Data integrity check passed!');
  }

  // Free the memory when done
  allocator.free(offset);
  logToConsole('Memory freed');

  logToConsole('All tests completed');
}

function testSingleMaxAlloc() {
  // First approach: Try to allocate one massive chunk
  logToConsole('Approach 1: Trying to allocate one massive chunk...');
  try {
    // Try to allocate 4GB in one go
    const sizeGB = 3.988;
    const GBOffset = allocator.allocate(sizeGB * GB);
    if (GBOffset !== 0) {
      allocations.push({ offset: GBOffset, size: sizeGB * GB });
      logToConsole(
        `SUCCESS: Allocated ${sizeGB}GB chunk at offset ${GBOffset}`
      );
    } else {
      logToConsole(`FAILED: Could not allocate ${sizeGB}GB in one chunk`);
    }
  } catch (e) {
    logToConsole(`ERROR during massive allocation: ${e.message}`);
  }
}

function testRepeatedAlloc() {
  try {
    // Second approach: Incrementally grow with large chunks
    logToConsole('Approach 2: Incrementally growing with large chunks...');
    const chunkSizes = [
      512 * MB,
      256 * MB,
      128 * MB,
      64 * MB,
      32 * MB,
      16 * MB,
    ];

    for (const chunkSize of chunkSizes) {
      logToConsole(`Trying to allocate chunks of ${chunkSize / MB} MB...`);
      let successCount = 0;
      let failureCount = 0;

      // Try allocating several chunks of this size
      for (let i = 0; i < 20; i++) {
        try {
          const offset = allocator.allocate(chunkSize);
          if (offset !== 0) {
            allocations.push({ offset, size: chunkSize });
            successCount++;
          } else {
            failureCount++;
            break; // Stop if we can't allocate more
          }
        } catch (e) {
          logToConsole(`Error during chunk allocation: ${e.message}`);
          break;
        }

        // Check memory stats after each successful allocation
        if (i % 5 === 0) {
          const currentStats = allocator.memory_stats();
          logToConsole(`Current memory: ${currentStats.totalSize / MB} MB`);
        }
      }

      logToConsole(
        `Allocated ${successCount} chunks of ${
          chunkSize / MB
        } MB (${failureCount} failures)`
      );

      // If we've allocated more than 3.5GB, we're probably near the limit
      const totalAllocated = allocations.reduce(
        (sum, alloc) => sum + alloc.size,
        0
      );
      if (totalAllocated > 3.5 * GB) {
        logToConsole(`Reached ${totalAllocated / GB} GB, approaching limit`);
        break;
      }
    }

    // Calculate total allocated memory
    const totalAllocated = allocations.reduce(
      (sum, alloc) => sum + alloc.size,
      0
    );
    logToConsole(
      `Total memory allocated: ${totalAllocated / MB} MB (${
        totalAllocated / GB
      } GB)`
    );

    // Get final memory stats
    const finalStats = allocator.memory_stats();
    logToConsole(
      `Final memory size: ${finalStats.totalSize / MB} MB (${
        finalStats.totalSize / GB
      } GB)`
    );
  } catch (error) {
    logToConsole(`Error during repeated memory allocation testing: ${error}`);
  }
}

function setupUI() {
  const memoryStatsDisplay = document.getElementById('memory-stats');

  function updateMemoryStats() {
    if (allocator) {
      const stats = allocator.memory_stats();
      const MB = 1024 * 1024;
      const totalSizeMB = stats.totalSize / MB;

      if (memoryStatsDisplay) {
        memoryStatsDisplay.textContent = `Total Memory: ${totalSizeMB.toFixed(
          2
        )} MB (${stats.totalSize / (1024 * MB)} GB)\n\nAllocated Pages: ${
          stats.pages
        } pages (Memory Grown To ~${((stats.pages * 64) / 1048576).toFixed(
          6
        )} GB)\n\nProgram Size: MB\n\nMemory Utilization: ${(
          100 -
          (4 - stats.totalSize / (1024 * MB)) * 100
        ).toFixed(3)} %`;
      }
    }
  }

  // Update stats couple seconds
  setInterval(updateMemoryStats, 3000);
}

initWasm();

if (typeof document !== 'undefined') {
  document.addEventListener('DOMContentLoaded', setupUI);
}
