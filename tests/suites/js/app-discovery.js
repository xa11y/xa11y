'use strict';

const { performance } = require('node:perf_hooks');

/** Discovery shared by integration files; native lookups must never race.
 * Promise.any does not cancel a losing native task, which keeps its worker
 * (and Node) alive until the full startup timeout even after a match wins.
 */
function createAppDiscovery({
  App, SelectorNotMatchedError, names, pid, name,
  timeout = 30_000,
  now = () => performance.now(),
  sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
}) {
  if (pid !== undefined && (!/^[1-9]\d*$/.test(pid) || Number(pid) > 0xffffffff)) {
    throw new Error(`Invalid XA11Y_TEST_APP_PID: ${JSON.stringify(pid)}`);
  }
  const candidates = [...new Set(names)];
  let pending = null;

  async function discover() {
    // The harness already proves the fixture identity. Do not rediscover a
    // different process by name if its authoritative PID cannot be attached.
    if (pid !== undefined) return App.byPid(Number(pid), { timeout });
    if (name) return App.byName(name, { timeout });

    // Standalone use still tolerates delayed platform registration and name
    // differences, with one shared budget. Only a genuine miss is retryable.
    const deadline = now() + timeout;
    let lastError;
    do {
      for (const candidate of candidates) {
        try {
          return await App.byName(candidate, { timeout: 0 });
        } catch (err) {
          if (!(err instanceof SelectorNotMatchedError)) throw err;
          lastError = err;
        }
        if (now() >= deadline) break;
      }
      const remaining = deadline - now();
      if (remaining <= 0) break;
      await sleep(Math.min(100, remaining));
    } while (now() < deadline);

    let listed;
    try {
      const apps = await App.list();
      listed = JSON.stringify(apps.slice(0, 20).map((app) => ({ name: app.name, pid: app.pid })));
      if (apps.length > 20) listed += ` (${apps.length - 20} more omitted)`;
    } catch (err) {
      listed = `<failed to list: ${err.message}>`;
    }
    throw new Error(
      `Test app not found after ${timeout}ms (tried ${candidates.join(', ')}). ` +
        `Last error: ${lastError?.message}. Running apps: ${listed}`,
      { cause: lastError },
    );
  }

  return {
    getApp() {
      // Concurrent callers share one lookup rather than filling the native
      // worker pool with identical startup polls. Failed lookups are not cached.
      if (pending === null) {
        pending = discover().catch((err) => {
          pending = null;
          throw err;
        });
      }
      return pending;
    },
    reset() { pending = null; },
  };
}

module.exports = { createAppDiscovery };
