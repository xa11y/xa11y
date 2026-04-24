// Integration tests: screenshot capture via `screenshotter()`.
//
// The capture pipeline needs a real display server and (on some platforms)
// an OS-level grant (Screen Recording on macOS, a working Wayland portal
// or X11 DISPLAY on Linux). Where the session can't capture at all, the
// backend surfaces `Error::Unsupported` as `ActionNotSupportedError` and we
// skip the assertions — the construction path is still exercised.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const xa11y = require('../../index.js');
const { ActionNotSupportedError, PermissionDeniedError } = xa11y;
const { getApp } = require('./helpers.js');

async function tryCapture(fn) {
  try {
    return await fn();
  } catch (err) {
    if (err instanceof ActionNotSupportedError || err instanceof PermissionDeniedError) {
      return null;
    }
    throw err;
  }
}

test('screenshotter() returns a Screenshotter', () => {
  const s = xa11y.screenshotter();
  assert.equal(s.constructor.name, 'Screenshotter');
});

test('capture() returns RGBA pixels and a valid PNG', async (t) => {
  const shooter = xa11y.screenshotter();
  const shot = await tryCapture(() => shooter.capture());
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

test('captureRegion respects scale', async (t) => {
  const shooter = xa11y.screenshotter();
  const rect = { x: 0, y: 0, width: 50, height: 40 };
  const shot = await tryCapture(() => shooter.captureRegion(rect));
  if (shot === null) return t.skip('capture unsupported in this session');

  const expectedW = Math.round(rect.width * shot.scale);
  const expectedH = Math.round(rect.height * shot.scale);
  // Allow 1px slack for rounding on fractional scale factors.
  assert.ok(Math.abs(shot.width - expectedW) <= 1);
  assert.ok(Math.abs(shot.height - expectedH) <= 1);
  assert.equal(shot.pixels.length, shot.width * shot.height * 4);
});

test('captureElement uses the element bounds', async (t) => {
  const app = await getApp();
  const button = await app.locator('button[name="Submit"]').element();
  if (!button.bounds || button.bounds.width === 0 || button.bounds.height === 0) {
    return t.skip('target element has no on-screen bounds (likely headless)');
  }

  const shooter = xa11y.screenshotter();
  const shot = await tryCapture(() => shooter.captureElement(button));
  if (shot === null) return t.skip('capture unsupported in this session');

  const expectedW = Math.round(button.bounds.width * shot.scale);
  const expectedH = Math.round(button.bounds.height * shot.scale);
  assert.ok(Math.abs(shot.width - expectedW) <= 1);
  assert.ok(Math.abs(shot.height - expectedH) <= 1);
});

test('savePng writes a valid PNG file', async (t) => {
  const shooter = xa11y.screenshotter();
  const shot = await tryCapture(() =>
    shooter.captureRegion({ x: 0, y: 0, width: 20, height: 20 }),
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
