'use strict';

// node:test cancellation cannot stop a pending napi worker. A stuck native
// operation otherwise keeps the process alive until the 900s harness limit.
async function withNativeWatchdog(label, operation, timeoutMs = 60_000) {
  const watchdog = setTimeout(() => {
    process.stderr.write(`Native test stalled after ${timeoutMs}ms: ${label}\n`);
    process.exit(1);
  }, timeoutMs);
  watchdog.unref();
  try {
    return await operation();
  } finally {
    clearTimeout(watchdog);
  }
}

module.exports = { withNativeWatchdog };
