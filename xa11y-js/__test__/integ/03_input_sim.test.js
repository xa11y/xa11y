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

const { test } = require('node:test');
const assert = require('node:assert/strict');

const xa11y = require('../../index.js');
const { InvalidActionDataError } = xa11y;
const { getApp } = require('./helpers.js');

const skip = process.env.XA11Y_SKIP_INPUT_SIM === '1';

test('inputSim() returns an InputSim', { skip }, () => {
  const sim = xa11y.inputSim();
  assert.equal(sim.constructor.name, 'InputSim');
});

test('moveTo accepts an [x, y] tuple', { skip }, async () => {
  const sim = xa11y.inputSim();
  await sim.moveTo([10, 10]);
});

test('moveTo accepts an Element', { skip }, async () => {
  const app = await getApp();
  const sim = xa11y.inputSim();
  const button = await app.locator('button[name="Submit"]').element();
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
  await assert.rejects(sim.moveTo([1]), (err) => err instanceof InvalidActionDataError);
});

test('press rejects an unknown key name', { skip }, async () => {
  const sim = xa11y.inputSim();
  await assert.rejects(
    sim.press('NotARealKey'),
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
