// Shared helpers for JS integration tests against the AccessKit test app.
//
// The test app is assumed to be already running (launched by the shell
// harness in scripts/run_js_tests.sh). This module polls the accessibility
// API until the app appears, then exposes a cached `getApp()` handle for
// individual tests.

'use strict';

const xa11y = require('../../index.js');
const { App, SelectorNotMatchedError, PlatformError, TimeoutError } = xa11y;

// Candidate app names. `scripts/run_js_tests.sh` can pre-resolve the actual
// name of a running test app and pass it via `XA11Y_TEST_APP_NAME`, in which
// case we try it first and skip the startup polling loop entirely.
const APP_NAMES = [
  ...(process.env.XA11Y_TEST_APP_NAME ? [process.env.XA11Y_TEST_APP_NAME] : []),
  'xa11y-test-app',
  'xa11y Test App',
];
const STARTUP_TIMEOUT_MS = 30_000;

let cachedApp = null;

/**
 * Return the xa11y App handle for the running test app. Polls up to
 * `STARTUP_TIMEOUT_MS` on first call so the test app has time to register
 * with AT-SPI2 / UIA / AX.
 */
async function getApp() {
  if (cachedApp !== null) return cachedApp;
  const deadline = Date.now() + STARTUP_TIMEOUT_MS;
  let lastErr = null;
  while (Date.now() < deadline) {
    for (const name of APP_NAMES) {
      try {
        cachedApp = await App.byName(name);
        return cachedApp;
      } catch (e) {
        if (!(e instanceof SelectorNotMatchedError || e instanceof PlatformError)) {
          throw e;
        }
        lastErr = e;
      }
    }
    await new Promise((r) => setTimeout(r, 250));
  }
  // One last attempt to list apps for a useful error message.
  let listed = '<failed to list>';
  try {
    const apps = await App.list();
    listed = JSON.stringify(apps.map((a) => ({ name: a.name, pid: a.pid })));
  } catch {
    /* ignore */
  }
  throw new Error(
    `Test app not found after ${STARTUP_TIMEOUT_MS}ms (tried ${APP_NAMES.join(', ')}). ` +
      `Last error: ${lastErr}. Running apps: ${listed}`,
  );
}

/**
 * Find exactly one element by selector scoped to the test app. Throws with a
 * helpful message if the count is wrong.
 */
async function one(app, selector) {
  const results = await app.locator(selector).elements();
  if (results.length !== 1) {
    throw new Error(
      `Selector ${JSON.stringify(selector)} matched ${results.length} elements (expected 1)`,
    );
  }
  return results[0];
}

/** Find the first element whose `name` contains `substring` (case-insensitive). */
async function named(app, substring) {
  const selector = `[name*="${substring}"]`;
  const results = await app.locator(selector).elements();
  if (results.length === 0) {
    throw new Error(`No element with name containing ${JSON.stringify(substring)}`);
  }
  return results[0];
}

/** Sleep for `ms` milliseconds. */
function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

/** Perform an action, wait briefly, then re-resolve the app handle fresh. */
async function act(locator, action, ...args) {
  await locator[action](...args);
  await sleep(150);
  cachedApp = null; // force a fresh App lookup so we re-read the tree
  return getApp();
}

module.exports = {
  APP_NAMES,
  TimeoutError,
  SelectorNotMatchedError,
  PlatformError,
  getApp,
  one,
  named,
  act,
  sleep,
};
