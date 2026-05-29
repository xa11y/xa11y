// Shared helpers for JS integration tests against the AccessKit test app.
//
// The test app is assumed to be already running (launched by the shell
// harness in scripts/run_js_tests.sh). This module polls the accessibility
// API until the app appears, then exposes a cached `getApp()` handle for
// individual tests.

'use strict';

const xa11y = require('../../../xa11y-js/index.js');
const { App, SelectorNotMatchedError, PlatformError, TimeoutError } = xa11y;

// ---------------------------------------------------------------------------
// Per-app configuration
// ---------------------------------------------------------------------------
//
// Mirrors the Python APP_CONFIGS dict in tests/suites/python/conftest.py.
// Tests use `appConfig` to adapt selectors and assertions to the current app.

const APP_CONFIG = {
  accesskit: {
    okButtonName: 'Submit',        // AccessKit test app uses "Submit" as the primary button
    textFieldName: 'Name',
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
  },
  qt: {
    okButtonName: 'OK',
    textFieldName: 'Search',
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
  },
  gtk: {
    okButtonName: 'OK',
    textFieldName: null,           // GTK doesn't reliably expose AX label on text_field
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
  },
  cocoa: {
    okButtonName: 'OK',
    textFieldName: 'Search',
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
  },
  tauri: {
    okButtonName: 'OK',
    textFieldName: 'Search',
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
  },
  electron: {
    okButtonName: 'OK',           // Electron index.html has <button id="ok">OK</button>
    textFieldName: null,           // Electron text input has no AX label
    minButtons: 2,                 // "OK" + "Cancel"
    hasCheckbox: false,
    hasRadio: false,
  },
  egui: {
    okButtonName: 'OK',           // egui sets the AccessKit name from the visible label
    textFieldName: null,          // egui's TextEdit::singleline does not set an AX name
    minButtons: 2,                // "OK" + "Cancel"
    hasCheckbox: true,
    hasRadio: true,
  },
};

// Candidate app names. `scripts/run_js_tests.sh` can pre-resolve the actual
// name of a running test app and pass it via `XA11Y_TEST_APP_NAME`, in which
// case we try it first and skip the startup polling loop entirely.
//
// `XA11Y_TEST_APP` selects a named fixture (accesskit, electron, …) whose
// known process names are added to the candidate list automatically.
const APP_NAMES_BY_APP = {
  accesskit: ['xa11y-test-app', 'xa11y Test App'],
  qt: ['xa11y-qt-test-app', 'xa11y', 'python3', 'python', 'Python'],
  gtk: ['xa11y-gtk-test-app', 'xa11y', 'python3', 'python', 'Python'],
  cocoa: ['xa11y-cocoa-test-app'],
  tauri: ['xa11y-tauri-test-app'],
  electron: ['xa11y-electron-test-app', 'Electron', 'xa11y'],
  egui: ['xa11y-egui-test-app'],
};

const appEnv = process.env.XA11Y_TEST_APP || 'accesskit';
const APP_NAMES = [
  ...(process.env.XA11Y_TEST_APP_NAME ? [process.env.XA11Y_TEST_APP_NAME] : []),
  ...(APP_NAMES_BY_APP[appEnv] ?? []),
  'xa11y-test-app',
  'xa11y Test App',
];
const STARTUP_TIMEOUT_MS = 30_000;

const appConfig = APP_CONFIG[appEnv] || APP_CONFIG.accesskit;

let cachedApp = null;

/**
 * Return the xa11y App handle for the running test app. Races all candidate
 * names in parallel so the first one to register (up to `STARTUP_TIMEOUT_MS`)
 * wins — handles cross-platform name differences without interleaved polling.
 */
async function getApp() {
  if (cachedApp !== null) return cachedApp;
  const attempts = APP_NAMES.map((name) =>
    App.byName(name, { timeout: STARTUP_TIMEOUT_MS }),
  );
  try {
    cachedApp = await Promise.any(attempts);
    return cachedApp;
  } catch (err) {
    let listed = '<failed to list>';
    try {
      const apps = await App.list();
      listed = JSON.stringify(apps.map((a) => ({ name: a.name, pid: a.pid })));
    } catch {
      /* ignore */
    }
    throw new Error(
      `Test app not found after ${STARTUP_TIMEOUT_MS}ms (tried ${APP_NAMES.join(', ')}). ` +
        `Errors: ${err.errors?.map((e) => e.message).join(' | ') ?? err}. Running apps: ${listed}`,
    );
  }
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
  APP_CONFIG,
  appConfig,
  appEnv,
  TimeoutError,
  SelectorNotMatchedError,
  PlatformError,
  getApp,
  one,
  named,
  act,
  sleep,
};
