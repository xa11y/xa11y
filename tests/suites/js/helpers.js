// Shared helpers for JS integration tests against the AccessKit test app.
//
// The test app is assumed to be already running (launched by the shell
// harness in scripts/run_js_tests.sh). This module polls the accessibility
// API until the app appears, then exposes a cached `getApp()` handle for
// individual tests.

'use strict';

const xa11y = require('../../../xa11y-js/index.js');
const { App, SelectorNotMatchedError, PlatformError, TimeoutError, ActionNotSupportedError } = xa11y;

// ---------------------------------------------------------------------------
// Per-app configuration
// ---------------------------------------------------------------------------
//
// Mirrors the Python APP_CONFIGS dict in tests/suites/python/conftest.py.
// Tests use `appConfig` to adapt selectors and assertions to the current app.

const APP_CONFIG = {
  accesskit: {
    // "Start slide" / "Slide target" (#530): required, so a missing widget
    // fails the waitStable tests instead of skipping them.
    hasSlideWidget: true,
    okButtonName: 'Submit',        // AccessKit test app uses "Submit" as the primary button
    textFieldName: 'Name',
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
    passwordFieldName: 'Password', // password entry (#522)
  },
  qt: {
    okButtonName: 'OK',
    textFieldName: 'Search',
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
    // Secondary dialog window (mirrors the Python APP_CONFIGS keys).
    dialogButtonName: 'Open Dialog',
    dialogName: 'Sample Dialog',
  },
  gtk: {
    okButtonName: 'OK',
    textFieldName: null,           // GTK doesn't reliably expose AX label on text_field
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
    dialogButtonName: 'Open Dialog',
    dialogName: 'Sample Dialog',
  },
  cocoa: {
    okButtonName: 'OK',
    textFieldName: 'Search',
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
  },
  tauri: {
    // "Start slide" / "Slide target" (#530): required, so a missing widget
    // fails the waitStable tests instead of skipping them.
    hasSlideWidget: true,
    okButtonName: 'OK',
    textFieldName: 'Search',
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
    passwordFieldName: 'Password', // password entry (#522)
  },
  electron: {
    okButtonName: 'OK',           // Electron index.html has <button id="ok">OK</button>
    textFieldName: null,           // Electron text input has no AX label
    minButtons: 2,                 // "OK" + "Cancel"
    hasCheckbox: false,
    hasRadio: false,
    passwordFieldName: 'Password', // password entry (#522)
  },
  egui: {
    okButtonName: 'OK',           // egui sets the AccessKit name from the visible label
    textFieldName: null,          // egui's TextEdit::singleline does not set an AX name
    minButtons: 2,                // "OK" + "Cancel"
    hasCheckbox: true,
    hasRadio: true,
  },
  winforms: {
    okButtonName: 'OK',
    textFieldName: 'Search',      // TextBox.AccessibleName = "Search"
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
    passwordFieldName: 'Password', // password entry (#522)
  },
  wpf: {
    okButtonName: 'OK',
    textFieldName: 'Search',      // AutomationProperties.Name = "Search"
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
    passwordFieldName: 'Password', // password entry (#522)
    dialogButtonName: 'Open Dialog',
    dialogName: 'Sample Dialog',
    siblingButtonName: 'Open Sibling',
    siblingName: 'Sample Sibling',
    duplicateButtonName: 'Open Duplicate Windows',
    duplicateWindowName: 'Duplicate Window',
  },
  uwp: {
    okButtonName: 'OK',
    textFieldName: 'Search',
    // Raw View deliberately retains the non-control Edit inside the XAML
    // TextBox (#606). The native class distinguishes the outer field.
    textFieldSelector: 'text_field[name="Search"][class_name="TextBox"]',
    minButtons: 2,
    hasCheckbox: true,
    hasRadio: true,
  },
};

// Names used only when no authoritative harness PID or resolved name is supplied.
const APP_NAMES_BY_APP = {
  accesskit: ['xa11y-test-app', 'xa11y Test App'],
  qt: ['xa11y-qt-test-app', 'xa11y', 'python3', 'python', 'Python'],
  gtk: ['xa11y-gtk-test-app', 'xa11y', 'python3', 'python', 'Python'],
  cocoa: ['xa11y-cocoa-test-app'],
  tauri: ['xa11y-tauri-test-app'],
  electron: ['xa11y-electron-test-app', 'Electron', 'xa11y'],
  egui: ['xa11y-egui-test-app'],
  winforms: ['xa11y-winforms-test-app'],
  wpf: ['xa11y-wpf-test-app'],
  uwp: ['xa11y UWP Test', 'xa11y-uwp-test-app'],
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
const textFieldSelector = appConfig.textFieldSelector || (
  appConfig.textFieldName ? `text_field[name="${appConfig.textFieldName}"]` : null
);

const { createAppDiscovery } = require('./app-discovery');
const discovery = createAppDiscovery({
  App,
  SelectorNotMatchedError,
  names: APP_NAMES,
  pid: process.env.XA11Y_TEST_APP_PID,
  name: process.env.XA11Y_TEST_APP_NAME,
  timeout: STARTUP_TIMEOUT_MS,
});
const getApp = () => discovery.getApp();
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
  discovery.reset(); // force a fresh App lookup so we re-read the tree
  return getApp();
}

module.exports = {
  APP_NAMES,
  APP_CONFIG,
  appConfig,
  appEnv,
  textFieldSelector,
  TimeoutError,
  SelectorNotMatchedError,
  PlatformError,
  ActionNotSupportedError,
  getApp,
  one,
  named,
  act,
  sleep,
};
