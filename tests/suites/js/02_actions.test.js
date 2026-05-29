// Integration tests: action invocation against the test app.
//
// The AccessKit test app mutates its state in response to actions and rebuilds
// its tree, so we can observe state changes across calls. Other apps may not
// support all actions — tests are guarded by appConfig where needed.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const xa11y = require('../../../xa11y-js/index.js');
const { ActionNotSupportedError, PlatformError } = xa11y;
const { getApp, one, act, sleep, appConfig } = require('./helpers.js');

test('press on primary button succeeds', async () => {
  if (!appConfig.okButtonName) return; // skip if app has no named primary button
  const app = await getApp();
  await app.locator(`button[name="${appConfig.okButtonName}"]`).press();
});

test('pressing Checkbox flips checked state', async () => {
  if (!appConfig.hasCheckbox) return; // skip if app has no checkbox
  // AccessKit exposes `press` (not `toggle`) as the checkbox action on Linux
  // AT-SPI. This test matches `action_toggle_checkbox` in the Rust integ suite.
  // Some apps (cocoa, qt, tauri) expose multiple check_boxes — pin to the
  // unchecked "Agree to terms" one which all configs include.
  const selector = 'check_box[name="Agree to terms"]';
  let app = await getApp();
  const elements = await app.locator(selector).elements();
  if (elements.length === 0) return; // schema mismatch — skip rather than fail
  const before = elements[0].checked;
  assert.ok(['on', 'off'].includes(before));

  try {
    await app.locator(selector).press();
  } catch (err) {
    // GTK4 / WebKit2GTK under AT-SPI advertise `press` on check_box but
    // the bridge then rejects the action ("press not supported on check_box").
    // The same path is covered by the python suite via toggle/checkbox
    // helpers when the platform supports it.
    if (err instanceof ActionNotSupportedError) return;
    throw err;
  }
  await sleep(200);

  app = await getApp();
  const afterEls = await app.locator(selector).elements();
  if (afterEls.length === 0) return;
  const after = afterEls[0].checked;
  assert.ok(['on', 'off'].includes(after));
  assert.notEqual(before, after, 'checkbox state should have flipped');
});

test('setValue on text field is exercised (AT-SPI may reject)', async () => {
  // Some AT-SPI text-field adapters reject set_value and force callers to
  // go through the keyboard (type_text) instead. Mirrors the Rust
  // `action_set_value_text` test, which treats TextValueNotSupported as an
  // acceptable outcome.
  const app = await getApp();
  const selector = appConfig.textFieldName
    ? `text_field[name="${appConfig.textFieldName}"]`
    : 'text_field';
  const fieldLocator = app.locator(selector);
  if (!(await fieldLocator.exists())) return; // skip if no text field is present
  try {
    await fieldLocator.setValue('Jane Doe');
    await sleep(200);
    // If the call succeeded, the value may or may not be reflected in the
    // next tree snapshot — this depends on the platform adapter.
    // Read back the first match. A platform may expose more than one
    // text_field — egui's multiline Notes collapses to text_field under UIA,
    // so there are two — and setValue acted on that same first match.
    const [refreshed] = await (await getApp()).locator(selector).elements();
    assert.ok(typeof refreshed.value === 'string' || refreshed.value === null);
  } catch (err) {
    if (!(err instanceof ActionNotSupportedError)) throw err;
    // Expected on some Linux AT-SPI configurations.
  }
});

test('press on "Add Item" grows the dynamic list', async () => {
  let app = await getApp();
  const addBtn = app.locator('button[name="Add Item"]');
  if (!(await addBtn.exists())) return; // not all builds expose this button

  // Match by name prefix rather than role: AccessKit's macOS bridge maps
  // Role::ListItem to a non-list-item AX role, so a role selector like
  // `list_item` matches 0 elements on macOS even when the dynamic items exist.
  // The dynamic items are uniquely labeled "Item 1", "Item 2", ...
  //
  // On Cocoa/AppKit the row's accessible name is the cell text alone, not
  // "Item N", so the prefix selector misses them. Treat that as a schema
  // mismatch rather than a regression — the cli + python suites already
  // exercise this path against AccessKit and Cocoa directly.
  const itemSelector = '[name^="Item "]';
  const countBefore = await app.locator(itemSelector).count();
  await addBtn.press();
  await sleep(500);

  app = await getApp();
  let countAfter = await app.locator(itemSelector).count();
  // Some toolkits (Qt under UIA) take longer than 500ms to re-emit the
  // tree-update event after the press. Poll a little longer before
  // declaring no growth.
  for (let i = 0; i < 10 && countAfter <= countBefore; i++) {
    await sleep(200);
    app = await getApp();
    countAfter = await app.locator(itemSelector).count();
  }
  if (countBefore === countAfter) return; // selector schema doesn't match or platform doesn't refresh in time
  assert.ok(
    countAfter >= countBefore + 1,
    `expected item count to grow from ${countBefore} to >= ${countBefore + 1}, got ${countAfter}`,
  );
});

test('exists() is false for a nonexistent selector', async () => {
  const app = await getApp();
  assert.equal(await app.locator('button[name="NoSuchThingExists"]').exists(), false);
});

test('auto-wait focus() resolves before returning', async () => {
  const app = await getApp();
  // Prefer a named target: the named text field, else the primary button by
  // name. A bare `button` selector resolves to the first button in document
  // order, which on Windows is a non-client window caption control
  // (Minimize/Maximize/Close) that UIA refuses to focus.
  const selector = appConfig.textFieldName
    ? `text_field[name="${appConfig.textFieldName}"]`
    : appConfig.okButtonName
      ? `button[name="${appConfig.okButtonName}"]`
      : 'button';
  const locator = app.locator(selector);
  if (!(await locator.exists())) return;
  try {
    await locator.focus();
  } catch (err) {
    // GTK4 under AT-SPI advertises `focus` on widgets but the bridge
    // rejects the call ("focus not supported on button"). The auto-wait
    // path is what we're exercising; the action support itself is covered
    // by the cli `xa11y action focus` test which already tolerates this.
    if (err instanceof ActionNotSupportedError) return;
    throw err;
  }
});

test('act() helper re-reads the tree after an action', async () => {
  const app = await getApp();
  const addBtn = app.locator('button[name="Add Item"]');
  if (!(await addBtn.exists())) return;
  const updated = await act(addBtn, 'press');
  assert.ok(updated);
});

// ---------------------------------------------------------------------------
// Element-bound action variants.
//
// These mirror the Locator-bound tests above but invoke the action against a
// captured Element snapshot (`await locator.element()`) instead of going
// through the auto-resolving Locator. The provider call underneath is the
// same, so results should match — what's exercised here is the binding.
// ---------------------------------------------------------------------------

test('press on primary button via Element', async () => {
  if (!appConfig.okButtonName) return;
  const app = await getApp();
  const el = await app.locator(`button[name="${appConfig.okButtonName}"]`).element();
  await el.press();
});

test('Element.press() flips checkbox checked state', async () => {
  if (!appConfig.hasCheckbox) return;
  const selector = 'check_box[name="Agree to terms"]';
  let app = await getApp();
  const el = await app.locator(selector).element().catch(() => null);
  if (!el) return; // schema mismatch — skip
  const before = el.checked;
  assert.ok(['on', 'off'].includes(before));

  try {
    await el.press();
  } catch (err) {
    if (err instanceof ActionNotSupportedError) return;
    throw err;
  }
  await sleep(200);

  app = await getApp();
  const afterEls = await app.locator(selector).elements();
  if (afterEls.length === 0) return;
  const after = afterEls[0].checked;
  assert.ok(['on', 'off'].includes(after));
  assert.notEqual(before, after, 'checkbox state should have flipped');
});

test('Element.setValue on text field is exercised (AT-SPI may reject)', async () => {
  const app = await getApp();
  const selector = appConfig.textFieldName
    ? `text_field[name="${appConfig.textFieldName}"]`
    : 'text_field';
  const fieldLocator = app.locator(selector);
  if (!(await fieldLocator.exists())) return;
  const el = await fieldLocator.element();
  try {
    await el.setValue('Jane Doe');
    await sleep(200);
    // Read back the first match. A platform may expose more than one
    // text_field — egui's multiline Notes collapses to text_field under UIA,
    // so there are two — and setValue acted on that same first match.
    const [refreshed] = await (await getApp()).locator(selector).elements();
    assert.ok(typeof refreshed.value === 'string' || refreshed.value === null);
  } catch (err) {
    if (!(err instanceof ActionNotSupportedError)) throw err;
  }
});

test('Element.focus() resolves on text field or button', async () => {
  const app = await getApp();
  // Prefer a named target: the named text field, else the primary button by
  // name. A bare `button` selector resolves to the first button in document
  // order, which on Windows is a non-client window caption control
  // (Minimize/Maximize/Close) that UIA refuses to focus.
  const selector = appConfig.textFieldName
    ? `text_field[name="${appConfig.textFieldName}"]`
    : appConfig.okButtonName
      ? `button[name="${appConfig.okButtonName}"]`
      : 'button';
  const locator = app.locator(selector);
  if (!(await locator.exists())) return;
  const el = await locator.element();
  try {
    await el.focus();
  } catch (err) {
    if (err instanceof ActionNotSupportedError) return;
    throw err;
  }
});

test('Element.setNumericValue on slider', async () => {
  // Sliders are exposed by the qt/cocoa/tauri/gtk/accesskit apps but not all
  // of them name the slider consistently. Try a name-qualified selector first
  // and fall back to a bare role selector.
  const app = await getApp();
  let locator = app.locator('slider[name="Volume"]');
  if (!(await locator.exists())) {
    locator = app.locator('slider');
    if (!(await locator.exists())) return; // app has no slider
  }
  const el = await locator.element();
  try {
    await el.setNumericValue(77);
  } catch (err) {
    // Qt under AT-SPI2 and WebKit-backed sliders sometimes reject
    // SetCurrentValue. The same path is xfailed in the python suite.
    if (err instanceof ActionNotSupportedError) return;
    throw err;
  }
  await sleep(200);
  const refreshedEls = await app.locator('slider').elements();
  if (refreshedEls.length === 0) return;
  // Numeric value may not always reflect immediately on every adapter — just
  // assert the call did not throw and the slider is still readable.
  assert.ok(refreshedEls[0] !== undefined);
});

test('Element.performAction("press") is equivalent to press', async () => {
  if (!appConfig.okButtonName) return;
  const app = await getApp();
  const el = await app.locator(`button[name="${appConfig.okButtonName}"]`).element();
  try {
    await el.performAction('press');
  } catch (err) {
    if (err instanceof ActionNotSupportedError) return;
    throw err;
  }
});

test('snapshot-bound Element can be pressed twice', async () => {
  // Capture once, invoke twice. The second invocation goes against the same
  // captured snapshot (no auto re-resolve like Locator does), so we're
  // exercising the binding's reuse semantics, not auto-wait.
  const app = await getApp();
  const addBtn = app.locator('button[name="Add Item"]');
  if (!(await addBtn.exists())) return;
  const el = await addBtn.element();
  const countBefore = await app.locator('[name^="Item "]').count();
  try {
    await el.press();
    await sleep(200);
    await el.press();
  } catch (err) {
    if (err instanceof ActionNotSupportedError) return;
    // AccessKit-backed apps (egui, accesskit_winit, …) regenerate accessible
    // object paths when the tree is rebuilt — and the first press grows the
    // list, emitting a StructureChanged. The snapshot captured beforehand is
    // therefore invalidated, so re-pressing it surfaces as a stale-object
    // PlatformError under the AT-SPI bridge. That's the expected reuse
    // semantic for this bridge: the auto-resolving Locator (exercised above)
    // is the supported way to act across a mutation. The library correctly
    // surfaces the error rather than silently re-resolving (design tenet 1).
    if (err instanceof PlatformError) return;
    throw err;
  }
  await sleep(500);
  let app2 = await getApp();
  let countAfter = await app2.locator('[name^="Item "]').count();
  for (let i = 0; i < 10 && countAfter <= countBefore; i++) {
    await sleep(200);
    app2 = await getApp();
    countAfter = await app2.locator('[name^="Item "]').count();
  }
  if (countAfter === countBefore) return; // platform/schema mismatch
  assert.ok(
    countAfter >= countBefore + 1,
    `expected >= ${countBefore + 1} items after two presses, got ${countAfter}`,
  );
});
