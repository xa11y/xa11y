// Electron / Chromium accessibility-not-enabled detection tests.
//
// On Linux, Chromium-based apps register with AT-SPI but expose only an
// `application -> frame` skeleton until launched with
// `--force-renderer-accessibility`. xa11y must surface
// `AccessibilityNotEnabledError` rather than silently returning zero results.

'use strict';

const { test, before, after } = require('node:test');
const assert = require('node:assert/strict');

const { launchElectron, AccessibilityNotEnabledError } = require('./helpers.js');

let noFlag = null;
let withFlag = null;

before(async () => {
  // Launch both fixtures up-front so the (slow) Electron startup happens
  // once per file rather than per test.
  noFlag = await launchElectron({ appName: 'xa11y-electron-noflag' });
  withFlag = await launchElectron({
    appName: 'xa11y-electron-withflag',
    forceA11y: true,
    // Wait for the rendered HTML button — the menubar shows up before the
    // renderer paints, so we'd otherwise race the page load.
    contentReadySelector: 'button[name="OK"]',
  });
});

after(async () => {
  if (noFlag) await noFlag.dispose();
  if (withFlag) await withFlag.dispose();
});

test('without flag: window.children() raises AccessibilityNotEnabledError', async () => {
  const windows = await noFlag.app.children();
  assert.equal(windows.length, 1, `expected exactly one window, got ${windows.length}`);
  const window = windows[0];
  await assert.rejects(
    () => window.children(),
    (err) => {
      assert.ok(
        err instanceof AccessibilityNotEnabledError,
        `expected AccessibilityNotEnabledError, got ${err && err.constructor && err.constructor.name}: ${err && err.message}`,
      );
      assert.match(String(err.message).toLowerCase(), /force-renderer-accessibility/);
      return true;
    },
  );
});

test('without flag: locator query raises AccessibilityNotEnabledError', async () => {
  await assert.rejects(
    () => noFlag.app.locator('button').elements(),
    (err) => {
      assert.ok(
        err instanceof AccessibilityNotEnabledError,
        `expected AccessibilityNotEnabledError, got ${err && err.constructor && err.constructor.name}: ${err && err.message}`,
      );
      assert.match(String(err.message).toLowerCase(), /force-renderer-accessibility/);
      return true;
    },
  );
});

test('with flag: window subtree is populated', async () => {
  const windows = await withFlag.app.children();
  assert.equal(windows.length, 1);
  const kids = await windows[0].children();
  assert.ok(kids.length > 0, 'window should have descendants when flag is set');
});

test('with flag: OK and Cancel buttons reachable via locator', async () => {
  const buttons = await withFlag.app.locator('button').elements();
  const names = new Set(buttons.map((b) => b.name).filter(Boolean));
  assert.ok(names.has('OK'), `OK button not found; names=${[...names].join(', ')}`);
  assert.ok(names.has('Cancel'), `Cancel button not found; names=${[...names].join(', ')}`);
});
