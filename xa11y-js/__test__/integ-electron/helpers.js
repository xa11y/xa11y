// Shared helpers for the Electron integration tests.
//
// Each test launches a fresh Electron process via a uniquely-named symlink to
// the Electron binary so the process's AT-SPI application name is
// deterministic. The launcher polls `xa11y.App.list()` until the app appears
// (and, optionally, until a content selector is reachable inside it) before
// handing back the App handle to the test.

'use strict';

const { spawn } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const xa11y = require('../../index.js');
const {
  App,
  AccessibilityNotEnabledError,
  SelectorNotMatchedError,
  PlatformError,
  TimeoutError,
} = xa11y;

const APP_DIR = path.resolve(__dirname, '..', '..', '..', 'test-apps', 'electron');
const ELECTRON_BIN = path.join(APP_DIR, 'node_modules', 'electron', 'dist', 'electron');

const STARTUP_TIMEOUT_MS = 30_000;

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

/** Resolve the Electron binary path, throwing if it isn't installed. */
function electronBin() {
  if (!fs.existsSync(ELECTRON_BIN)) {
    throw new Error(
      `Electron not installed at ${ELECTRON_BIN}. Run \`npm install\` in test-apps/electron.`,
    );
  }
  return ELECTRON_BIN;
}

/**
 * Create a symlink whose basename is `appName` and which points at the real
 * Electron binary. Chromium uses `argv[0]`'s basename as the AT-SPI
 * application name, so this lets each test instance be located unambiguously
 * via `App.byName(appName)` even when several Electron processes are alive.
 */
function makeElectronSymlink(appName) {
  const linkDir = path.join(os.tmpdir(), 'xa11y-electron-links');
  fs.mkdirSync(linkDir, { recursive: true });
  const link = path.join(linkDir, appName);
  try {
    fs.unlinkSync(link);
  } catch (e) {
    if (e.code !== 'ENOENT') throw e;
  }
  fs.symlinkSync(electronBin(), link);
  return link;
}

/**
 * Launch an Electron instance. Returns `{ app, dispose }`.
 *
 * @param {object} opts
 * @param {string} opts.appName              Unique name (also the symlink basename).
 * @param {boolean} [opts.forceA11y]         Pass `--force-renderer-accessibility`.
 * @param {string} [opts.contentReadySelector]
 *   If set, additionally poll until this selector matches inside the App's
 *   tree before resolving — useful when waiting for the renderer to paint.
 */
async function launchElectron(opts) {
  const { appName, forceA11y = false, contentReadySelector = null } = opts;
  const bin = makeElectronSymlink(appName);
  const args = ['--no-sandbox'];
  if (forceA11y) args.push('--force-renderer-accessibility');
  args.push(APP_DIR);

  const child = spawn(bin, args, {
    env: process.env,
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  const dispose = async () => {
    if (!child.killed) {
      try {
        child.kill('SIGTERM');
      } catch {
        /* ignore */
      }
    }
  };

  let exited = false;
  child.once('exit', () => {
    exited = true;
  });

  try {
    const app = await waitForApp(appName, child, () => exited);
    if (contentReadySelector) {
      await waitForContent(app, contentReadySelector);
    }
    return { app, dispose };
  } catch (err) {
    await dispose();
    throw err;
  }
}

async function waitForApp(appName, child, isExited) {
  const deadline = Date.now() + STARTUP_TIMEOUT_MS;
  let lastErr = null;
  while (Date.now() < deadline) {
    if (isExited()) {
      throw new Error(`Electron process exited before registering as "${appName}".`);
    }
    try {
      return await App.byName(appName);
    } catch (e) {
      if (!(e instanceof SelectorNotMatchedError || e instanceof PlatformError)) {
        throw e;
      }
      lastErr = e;
    }
    await sleep(250);
  }
  let listed = '<failed to list>';
  try {
    const apps = await App.list();
    listed = apps.map((a) => a.name).filter(Boolean).join(', ');
  } catch {
    /* ignore */
  }
  throw new Error(
    `Electron app "${appName}" not visible after ${STARTUP_TIMEOUT_MS}ms. ` +
      `Last error: ${lastErr}. Visible apps: [${listed}].`,
  );
}

async function waitForContent(app, selector) {
  const deadline = Date.now() + STARTUP_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      await app.locator(selector).element();
      return;
    } catch (e) {
      if (
        !(
          e instanceof SelectorNotMatchedError ||
          e instanceof PlatformError ||
          e instanceof TimeoutError
        )
      ) {
        throw e;
      }
    }
    await sleep(250);
  }
  throw new Error(
    `Content selector ${JSON.stringify(selector)} never matched after ${STARTUP_TIMEOUT_MS}ms.`,
  );
}

module.exports = {
  launchElectron,
  AccessibilityNotEnabledError,
  SelectorNotMatchedError,
  PlatformError,
  TimeoutError,
};
