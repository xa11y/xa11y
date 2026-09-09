// Window-management surface against the shared mock provider: Element /
// Locator window verbs, window state getters, and App.windows().
//
// The mock models window state mutations in place (minimize marks the window
// minimized and off-screen; restore reverses both).

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { _makeTestActionProbe, _makeTestApp, InvalidActionDataError } = require('../../index.js');

async function findDescendant(root, predicate) {
  const queue = [root];
  while (queue.length > 0) {
    const el = queue.shift();
    if (predicate(el)) return el;
    queue.push(...(await el.children()));
  }
  return null;
}

async function probeWindow(predicate, probe = _makeTestActionProbe()) {
  const app = await probe.locator().element();
  const el = await findDescendant(app, predicate);
  assert.ok(el, 'fixture element not found in mock tree');
  return { probe, el };
}

function lastAction(probe) {
  const log = probe.actions();
  assert.ok(log.length > 0, 'expected at least one recorded action');
  return log[log.length - 1];
}

// ── Element window verbs ────────────────────────────────────────────────

test('window verbs record their action names', async () => {
  const { probe, el } = await probeWindow((e) => e.role === 'window');
  for (const verb of ['raise', 'minimize', 'maximize', 'restore', 'close']) {
    probe.clear();
    await el[verb]();
    assert.equal(lastAction(probe)[1], verb);
  }
});

test('moveTo() records the coordinate payload', async () => {
  const { probe, el } = await probeWindow((e) => e.role === 'window');
  await el.moveTo(10, 20);
  let [, name, data] = lastAction(probe);
  assert.equal(name, 'move_to');
  assert.equal(data, '10,20');
  // Negative origins are valid geometry — a monitor left of or above the
  // primary display — so they must reach the provider, not be rejected.
  await el.moveTo(-10, -20);
  [, name, data] = lastAction(probe);
  assert.equal(name, 'move_to');
  assert.equal(data, '-10,-20');
});

test('resizeTo() records the dimension payload', async () => {
  const { probe, el } = await probeWindow((e) => e.role === 'window');
  await el.resizeTo(640, 480);
  const [, name, data] = lastAction(probe);
  assert.equal(name, 'resize_to');
  assert.equal(data, '640x480');
});

test('resizeTo() rejects invalid dimensions before reaching the provider', async () => {
  const { probe, el } = await probeWindow((e) => e.role === 'window');
  // Zero, negative, fractional, non-finite, and beyond-u32 values must be
  // rejected on the raw JS number: napi's ToUint32 would otherwise turn -1
  // into 4294967295 and a fractional 100.5 into 100, and 0 was the only case
  // core validated before.
  assert.throws(() => el.resizeTo(0, 100), InvalidActionDataError);
  assert.throws(() => el.resizeTo(-1, 100), InvalidActionDataError);
  assert.throws(() => el.resizeTo(100.5, 100), InvalidActionDataError);
  assert.throws(() => el.resizeTo(4294967296, 100), InvalidActionDataError);
  assert.throws(() => el.resizeTo(100, 0), InvalidActionDataError);
  assert.throws(() => el.resizeTo(100, -1), InvalidActionDataError);
  assert.throws(() => el.resizeTo(100, 100.5), InvalidActionDataError);
  assert.throws(() => el.resizeTo(100, NaN), InvalidActionDataError);
  assert.deepEqual(probe.actions(), []);
});

test('moveTo() rejects invalid coordinates before reaching the provider', async () => {
  const { probe, el } = await probeWindow((e) => e.role === 'window');
  // ToInt32 would silently wrap 2147483648 to -2147483648; non-finite,
  // fractional, and out-of-range values must be rejected on the raw JS
  // number instead. Negative coordinates remain valid (a monitor left of
  // or above the primary display) — only out-of-range negatives are bad.
  for (const [x, y] of [[NaN, 10], [10.5, 20], [2147483648, 0], [-2147483649, 0], [Infinity, 0], [10, 2147483648]]) {
    assert.throws(() => el.moveTo(x, y), InvalidActionDataError);
  }
  assert.deepEqual(probe.actions(), []);
});

test('window state getters default to null and round-trip minimize/restore', async () => {
  const { probe, el } = await probeWindow((e) => e.role === 'window');
  assert.equal(el.minimized, null);
  assert.equal(el.maximized, null);
  assert.equal(el.fullscreen, null);

  await el.minimize();
  const minimized = await probeWindow((e) => e.role === 'window', probe);
  assert.equal(minimized.el.minimized, true);
  // The mock decided the window is not maximized: the real-provider tri-state
  // (UIA WindowVisualState_Minimized → minimized=Some(true), maximized=Some(false))
  // reports decided-false, not unknown-null.
  assert.equal(minimized.el.maximized, false);
  assert.equal(minimized.el.visible, false);

  await minimized.el.restore();
  const restored = await probeWindow((e) => e.role === 'window', probe);
  assert.equal(restored.el.minimized, false);
  assert.equal(restored.el.visible, true);
});

// ── Locator window verbs (enabled-only auto-wait gate) ──────────────────

test('locator window verbs act on a minimized (invisible) window', async () => {
  const probe = _makeTestActionProbe();
  await probe.locator().descendant('window').minimize();
  // The window is now visible=false; the enabled-only gate must let restore
  // through — a visible-gated wait would time out after the default 5s.
  await probe.locator().descendant('window').restore();
  const log = probe.actions();
  assert.ok(log.some(([, action]) => action === 'restore'));
});

test('locator window verbs reject invalid geometry before auto-wait', async () => {
  const probe = _makeTestActionProbe();
  const loc = probe.locator().descendant('window[name="never-matches"]');
  assert.throws(() => loc.resizeTo(0, 100), InvalidActionDataError);
  assert.throws(() => loc.resizeTo(-1, 100), InvalidActionDataError);
  assert.throws(() => loc.moveTo(NaN, 0), InvalidActionDataError);
});

// ── App.windows() ───────────────────────────────────────────────────────

test('App.windows() lists the app\'s window children', async () => {
  const app = _makeTestApp();
  const windows = await app.windows();
  assert.equal(windows.length, 1);
  assert.equal(windows[0].role, 'window');
  assert.equal(windows[0].name, 'Main Window');
  assert.equal(windows[0].minimized, null);
});
