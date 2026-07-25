// Integration tests: input simulation via `inputSim()`.
//
// The AccessKit test app has no event-log that captures synthesised pointer
// or keyboard events, so these are smoke tests rather than end-to-end
// assertions about WebView-delivered events (that kind of assertion lives in
// tests/tauri/test_input_sim.py). We verify the binding surface is callable,
// that target-resolution works for both tuple and Element forms, and that
// key parsing rejects garbage.
//
// When the host can't synthesise input (no Accessibility/Input Monitoring
// grant on macOS, no WM under Xvfb on Linux), the harness sets
// XA11Y_SKIP_INPUT_SIM=1; we skip at that signal rather than fail.

'use strict';

// Only runs against Tauri and Electron — input_sim tests one-per-platform strategy
const XA11Y_TEST_APP = process.env.XA11Y_TEST_APP || 'accesskit';
if (!['tauri', 'electron'].includes(XA11Y_TEST_APP)) {
  console.log(`Skipping input_sim tests for app=${XA11Y_TEST_APP}`);
  process.exit(0);
}

const { test } = require('node:test');
const assert = require('node:assert/strict');

const xa11y = require('../../../xa11y-js/index.js');
const { InvalidActionDataError } = xa11y;
const { getApp, appConfig } = require('./helpers.js');

const skip = process.env.XA11Y_SKIP_INPUT_SIM === '1';

test('inputSim() returns an InputSim', { skip }, () => {
  const sim = xa11y.inputSim();
  assert.equal(sim.constructor.name, 'InputSim');
});

test('moveTo accepts an [x, y] tuple', { skip }, async () => {
  const sim = xa11y.inputSim();
  await sim.moveTo([10, 10]);
});

test('moveTo accepts an Element', { skip }, async (t) => {
  const app = await getApp();
  const sim = xa11y.inputSim();
  // Use the app's primary button name (Submit on AccessKit, OK on Tauri/
  // Electron). The test only cares that an Element-shaped target works.
  const primary = appConfig.okButtonName || 'Submit';
  const buttons = await app.locator(`button[name="${primary}"]`).elements();
  if (buttons.length === 0) {
    return t.skip(`primary button ${JSON.stringify(primary)} not found in this app`);
  }
  const button = buttons[0];
  // If the app is headless/off-screen the element may have null bounds;
  // in that case moveTo should reject with an XA11yError (NoElementBounds).
  if (button.bounds === null) {
    await assert.rejects(sim.moveTo(button), (err) => err instanceof xa11y.XA11yError);
    return;
  }
  await sim.moveTo(button);
});

test('moveTo rejects a malformed tuple', { skip }, async () => {
  const sim = xa11y.inputSim();
  // Argument validation runs synchronously in the napi entry point, so the
  // call throws before a Promise is constructed — wrap in a thunk so
  // `assert.rejects` can catch either shape.
  await assert.rejects(
    async () => sim.moveTo([1]),
    (err) => err instanceof InvalidActionDataError,
  );
});

test('press rejects an unknown key name', { skip }, async () => {
  const sim = xa11y.inputSim();
  await assert.rejects(
    async () => sim.press('NotARealKey'),
    (err) => err instanceof InvalidActionDataError,
  );
});

test('press accepts a named key', { skip }, async () => {
  const sim = xa11y.inputSim();
  // Escape is a benign key that should never have a side effect on the
  // AccessKit test app's focused widget.
  await sim.press('Escape');
});

test('chord holds modifiers', { skip }, async () => {
  const sim = xa11y.inputSim();
  // Shift+A is similarly benign on the AccessKit window; we only care that
  // the down/up sequence doesn't throw.
  await sim.chord('a', ['Shift']);
});

test('typeText no-op on empty string', { skip }, async () => {
  const sim = xa11y.inputSim();
  await sim.typeText('');
});

// ── Half-press primitives ─────────────────────────────────────────────────
//
// `keyDown`/`keyUp` and `mouseDown`/`mouseUp` are the primitives that neither
// `press` nor `click` can express: holding a key or button across other
// actions. End-to-end assertions about what the webview received live in
// tests/suites/python/test_input_sim.py; here we verify the JS surface is
// callable and validates its arguments.

test('keyDown/keyUp round-trip', { skip }, async () => {
  const sim = xa11y.inputSim();
  await sim.keyDown('Shift');
  await sim.keyUp('Shift');
});

test('keyDown rejects an unknown key name', { skip }, async () => {
  const sim = xa11y.inputSim();
  await assert.rejects(
    async () => sim.keyDown('NotARealKey'),
    (err) => err instanceof InvalidActionDataError,
  );
});

// No `mouseDown`/`mouseUp` round-trip here: a real button press lands
// wherever the pointer happens to be and could disturb the app under test,
// which is why this suite has never posted one. The end-to-end press/release
// assertions live in the Python suite, aimed at the Tauri hit target.

test('mouseDown rejects an unknown button name', { skip }, async () => {
  const sim = xa11y.inputSim();
  await assert.rejects(
    async () => sim.mouseDown('scroll'),
    (err) => err instanceof InvalidActionDataError,
  );
});

// ── Options objects (core's ClickOptions / DragOptions) ───────────────────
//
// Validation only, for the same reason: every option here is parsed before
// any OS event is posted, so these assert the surface without clicking
// anything. The Python suite exercises the options end-to-end.

test('click rejects an unknown button in its options', { skip }, async () => {
  const sim = xa11y.inputSim();
  await assert.rejects(
    async () => sim.click([10, 10], { button: 'scroll' }),
    (err) => err instanceof InvalidActionDataError,
  );
});

test('drag rejects an unknown held key in its options', { skip }, async () => {
  const sim = xa11y.inputSim();
  await assert.rejects(
    async () => sim.drag([10, 10], [20, 20], { held: ['NotARealKey'] }),
    (err) => err instanceof InvalidActionDataError,
  );
});

test('click rejects an unknown anchor', { skip }, async () => {
  const sim = xa11y.inputSim();
  await assert.rejects(
    async () => sim.click([10, 10], { anchor: 'middle_left' }),
    (err) => err instanceof InvalidActionDataError,
  );
});

test('click rejects a malformed anchor offset', { skip }, async () => {
  const sim = xa11y.inputSim();
  await assert.rejects(
    async () => sim.click([10, 10], { anchor: [1] }),
    (err) => err instanceof InvalidActionDataError,
  );
});
