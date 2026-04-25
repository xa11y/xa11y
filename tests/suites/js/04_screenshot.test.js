// Integration tests: screenshot capture via `screenshot()`.
//
// The capture pipeline needs a real display server and (on some platforms)
// an OS-level grant (Screen Recording on macOS, a working Wayland portal
// or X11 DISPLAY on Linux). Where the session can't capture at all, the
// backend surfaces `Error::Unsupported` as `ActionNotSupportedError` and we
// skip the assertions — the construction path is still exercised.

'use strict';

// Only runs against Tauri and Electron — input_sim tests one-per-platform strategy
const XA11Y_TEST_APP = process.env.XA11Y_TEST_APP || 'accesskit';
if (!['tauri', 'electron'].includes(XA11Y_TEST_APP)) {
  console.log(`Skipping screenshot tests for app=${XA11Y_TEST_APP}`);
  process.exit(0);
}

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const xa11y = require('../../../xa11y-js/index.js');
const {
  ActionNotSupportedError,
  InvalidActionDataError,
  PermissionDeniedError,
  PlatformError,
} = xa11y;
const { getApp, appConfig } = require('./helpers.js');

// Known platform-error substrings that indicate the host can't capture the
// screen at all (no Screen Recording grant on macOS GH runners, no working
// X11 GetImage on a fresh Xvfb, etc.). These are environmental — the binding
// surface is what we're testing, not screen-capture itself, so treat them
// as a skip.
const CAPTURE_UNAVAILABLE = [
  'screenshotmanager returned no image',  // macOS without Screen Recording
  'getimage',                              // X11 GetImage Match error on Xvfb
  'no portal',                             // Wayland xdg-desktop-portal missing
];

async function tryCapture(fn) {
  try {
    return await fn();
  } catch (err) {
    if (err instanceof ActionNotSupportedError || err instanceof PermissionDeniedError) {
      return null;
    }
    if (err instanceof PlatformError) {
      const msg = String(err.message || err).toLowerCase();
      if (CAPTURE_UNAVAILABLE.some((needle) => msg.includes(needle))) {
        return null;
      }
    }
    throw err;
  }
}

test('screenshot() returns RGBA pixels and a valid PNG', async (t) => {
  const shot = await tryCapture(() => xa11y.screenshot());
  if (shot === null) return t.skip('capture unsupported in this session');

  assert.ok(shot.width > 0);
  assert.ok(shot.height > 0);
  assert.ok(shot.scale > 0);
  assert.equal(shot.pixels.length, shot.width * shot.height * 4);

  const png = shot.toPng();
  // PNG magic bytes.
  assert.deepEqual(
    Array.from(png.slice(0, 8)),
    [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
  );
  assert.ok(png.length > 100);
});

test('screenshot({ region }) respects scale', async (t) => {
  const region = { x: 0, y: 0, width: 50, height: 40 };
  const shot = await tryCapture(() => xa11y.screenshot({ region }));
  if (shot === null) return t.skip('capture unsupported in this session');

  const expectedW = Math.round(region.width * shot.scale);
  const expectedH = Math.round(region.height * shot.scale);
  // Allow 1px slack for rounding on fractional scale factors.
  assert.ok(Math.abs(shot.width - expectedW) <= 1);
  assert.ok(Math.abs(shot.height - expectedH) <= 1);
  assert.equal(shot.pixels.length, shot.width * shot.height * 4);
});

test('screenshot({ element }) uses the element bounds', async (t) => {
  const app = await getApp();
  // Use whatever button this app considers primary (Submit on AccessKit,
  // OK on Tauri/Cocoa/Qt/GTK/Electron) so we don't fail on schema mismatch.
  const primary = appConfig.okButtonName || 'Submit';
  const buttons = await app.locator(`button[name="${primary}"]`).elements();
  if (buttons.length === 0) {
    return t.skip(`primary button ${JSON.stringify(primary)} not found in this app`);
  }
  const button = buttons[0];
  if (!button.bounds || button.bounds.width === 0 || button.bounds.height === 0) {
    return t.skip('target element has no on-screen bounds (likely headless)');
  }

  const shot = await tryCapture(() => xa11y.screenshot({ element: button }));
  if (shot === null) return t.skip('capture unsupported in this session');

  const expectedW = Math.round(button.bounds.width * shot.scale);
  const expectedH = Math.round(button.bounds.height * shot.scale);
  assert.ok(Math.abs(shot.width - expectedW) <= 1);
  assert.ok(Math.abs(shot.height - expectedH) <= 1);
});

test('screenshot({ element, region }) rejects both-set', async () => {
  const app = await getApp();
  const button = await app.locator('button').first().element();
  assert.throws(
    () => xa11y.screenshot({ element: button, region: { x: 0, y: 0, width: 10, height: 10 } }),
    (err) => err instanceof InvalidActionDataError,
  );
});

test('savePng writes a valid PNG file', async (t) => {
  const shot = await tryCapture(() =>
    xa11y.screenshot({ region: { x: 0, y: 0, width: 20, height: 20 } }),
  );
  if (shot === null) return t.skip('capture unsupported in this session');

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'xa11y-js-shot-'));
  const file = path.join(dir, 'shot.png');
  try {
    shot.savePng(file);
    const bytes = fs.readFileSync(file);
    assert.deepEqual(
      Array.from(bytes.slice(0, 8)),
      [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
    );
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
