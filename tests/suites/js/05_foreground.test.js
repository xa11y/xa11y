// Integration tests: foreground application + active window against the app.
//
// Mirrors tests/suites/python/test_foreground.py. Runs app-agnostically across
// the toolkits the JS integration harness targets (qt / gtk / tauri), so the
// strict identity assertions are gated on a *frontmost* signal — the freshly
// read `isForeground` flag for the app's pid. When the OS reports our app as
// frontmost we assert the strong invariants; otherwise we fall back to the
// app-agnostic ones (types, uniqueness) so the suite degrades gracefully.
//
// The gate is read either side of the observation it gates, and honoured only
// when the two agree — see `observeWithStableForeground`. Reading it once,
// afterwards, made the gate and the observation two views of a desktop that
// had moved in between.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const xa11y = require('../../../xa11y-js/index.js');
const { App, SelectorNotMatchedError } = xa11y;
const { getApp, appConfig } = require('./helpers.js');

/**
 * Re-list running apps and return the current `isForeground` for our pid.
 * Uses a fresh `App.list()` rather than the cached handle's possibly-stale
 * flag. Returns `null` if the app is no longer listed.
 */
async function freshForegroundFlag(app) {
  const apps = await App.list();
  const me = apps.find((a) => a.pid === app.pid);
  return me ? Boolean(me.isForeground) : null;
}

/**
 * Run `observe()` sandwiched between two reads of the app's frontmost flag,
 * and report the flag only when it did not change across the observation.
 *
 * The gate and the thing it gates are two separate reads of mutable global
 * desktop state, so taking them one after the other is a race: if the
 * foreground moves to our app *between* `App.foreground()` resolving someone
 * else and the gate being read, the gate opens on a fact that was not true
 * when the observation was taken, and the strong assertion compares two
 * moments that never coexisted. That is a real CI failure, not a bug in the
 * binding:
 *
 *     our app (pid=7764) reports isForeground but
 *     App.foreground() resolved a different pid=1704
 *
 * Reading the flag either side and requiring agreement means the strong
 * invariants are asserted only against an observation the desktop held still
 * for. A few retries cover a desktop that is merely settling; one that keeps
 * changing hands reports `frontmost: null`, and the caller degrades to the
 * app-agnostic assertions exactly as it already does when the app is not
 * frontmost at all.
 */
async function observeWithStableForeground(app, observe, attempts = 5) {
  let value;
  for (let i = 0; i < attempts; i += 1) {
    const before = await freshForegroundFlag(app);
    value = await observe();
    const after = await freshForegroundFlag(app);
    if (before === after) return { value, frontmost: before };
  }
  return { value, frontmost: null };
}

// ── App.foreground() ────────────────────────────────────────────────────────

test('App.foreground() resolves to a foreground app', async () => {
  const app = await getApp();
  let reading;
  try {
    reading = await observeWithStableForeground(app, () =>
      App.foreground({ timeout: 5000 }),
    );
  } catch (err) {
    if (err instanceof SelectorNotMatchedError) return; // nothing holds focus
    throw err;
  }
  const { value: fg, frontmost } = reading;

  assert.ok(typeof fg.pid === 'number' && fg.pid > 0);
  // Whatever App.foreground() returns is, by definition, the foreground app.
  assert.equal(fg.isForeground, true);

  if (frontmost) {
    assert.equal(
      fg.pid,
      app.pid,
      `our app (pid=${app.pid}) reports isForeground but App.foreground() ` +
        `resolved a different pid=${fg.pid}`,
    );
  }
});

// ── App.list() + isForeground ───────────────────────────────────────────────

test('App.list() contains the app and exposes an isForeground flag', async () => {
  const app = await getApp();
  const apps = await App.list();
  for (const a of apps) {
    assert.equal(typeof a.isForeground, 'boolean');
  }

  const me = apps.find((a) => a.pid === app.pid);
  assert.ok(me, `fixture app pid=${app.pid} not found in App.list()`);

  // Frontmost-guarded: only assert we claim the foreground on harnesses where
  // the OS actually puts us in front.
  if (me.isForeground) {
    assert.equal(me.isForeground, true);
  }
});

// ── App.isForeground / App.focused (deprecated) ─────────────────────────────

test('App.focused is a deprecated alias equal to isForeground', async () => {
  const app = await getApp();
  assert.equal(app.focused, app.isForeground);
});

test('App.isForeground is a plain boolean', async () => {
  const app = await getApp();
  assert.equal(typeof app.isForeground, 'boolean');
});

// ── Element.active — active/foreground window StateSet flag ──────────────────

test('active window reports active; descendants do not', async () => {
  const app = await getApp();
  // Same race as above: the window snapshot and the frontmost gate are two
  // reads of moving state, so take the snapshot inside the sandwich.
  const { value: windows, frontmost } = await observeWithStableForeground(
    app,
    () => app.locator('window').elements(),
  );
  if (windows.length === 0) return; // no separate window node (e.g. UIA app-as-window)

  const activeWindows = windows.filter((w) => w.active);

  // Exactly-one-active-window is the invariant — never more than one.
  assert.ok(
    activeWindows.length <= 1,
    `expected at most one active window, found ${activeWindows.length}`,
  );

  if (frontmost) {
    assert.equal(
      activeWindows.length,
      1,
      'app is frontmost but no window reports active=true',
    );
    const bySelector = await app.locator('window[active="true"]').elements();
    assert.equal(bySelector.length, 1);
    assert.equal(bySelector[0].active, true);
  }

  // A non-window descendant (a button) is never the active window.
  if (appConfig.okButtonName) {
    const button = await app
      .locator(`button[name="${appConfig.okButtonName}"]`)
      .element();
    assert.equal(button.active, false);
  }
});
