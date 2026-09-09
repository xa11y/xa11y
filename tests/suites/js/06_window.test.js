// Window-management coverage for the JS binding, against a real app.
//
// Mirrors tests/suites/python/test_window.py and, at a distance, the Rust
// integ suite (xa11y/tests/integ/windows.rs). The Rust suite runs the deep
// per-platform success/failure matrix; this file's job is the *surface* — the
// async binding, the napi task payloads, and the provider boundary — so that
// `App.windows()` and the state getters are verified end to end.
//
// Only the read-only surface and `raise` (focus-only, least invasive) run
// here: the suite shares the app instance with the python and cli suites,
// and the mutating window verbs (minimize / restore / resize) churn the
// UIA/AX cache in a way that made a following suite's action tests flaky.
// minimize / restore / maximize and moveTo / resizeTo run in
// 07_window_mutating.test.js, which the harness orders after the action
// suites (see tests/harness/launch.py); the Rust integ suite is the ground
// truth for per-platform support. `close` on a real secondary dialog is
// exercised by 07_window_mutating.test.js (Element.close and Locator.close)
// and by the Rust integ suite; closing the shared app's main window would
// still kill it for the suites that follow, so only secondary-window close
// is covered in the binding suites.
//
// Not every matrix app exposes a window-like element (the GTK app's AT-SPI
// tree starts at a plain group, with no frame node), so an empty listing
// skips rather than fails.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { getApp } = require('./helpers.js');

test('App.windows() reaches the provider with the promised state getters', async () => {
  const app = await getApp();
  const windows = await app.windows();
  assert.ok(Array.isArray(windows), 'windows() is an array');
  if (windows.length === 0) {
    return; // the app exposes no window-like element; skip is the honest answer
  }
  for (const w of windows) {
    assert.ok(w.role, 'a listed window always carries its role');
    // `null` is the documented "unknown / not a window" on this surface.
    assert.ok(typeof w.minimized === 'boolean' || w.minimized === null, 'minimized');
    assert.ok(typeof w.maximized === 'boolean' || w.maximized === null, 'maximized');
    assert.ok(typeof w.fullscreen === 'boolean' || w.fullscreen === null, 'fullscreen');
  }
});

test('a window that advertises raise gets it dispatched to the platform', async () => {
  const app = await getApp();
  const windows = await app.windows();
  const win = windows.find((w) => w.actions.includes('raise'));
  if (!win) {
    // The per-platform matrix is the Rust suite's ground truth; here the
    // point is the boundary, and a window that advertises nothing cannot
    // verify it.
    return;
  }
  await win.raise();
});
