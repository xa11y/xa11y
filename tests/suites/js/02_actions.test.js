// Integration tests: action invocation against the test app.
//
// The AccessKit test app mutates its state in response to actions and rebuilds
// its tree, so we can observe state changes across calls. Other apps may not
// support all actions — tests are guarded by appConfig where needed.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const xa11y = require('../../../xa11y-js/index.js');
const { ActionNotSupportedError } = xa11y;
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
  let app = await getApp();
  const before = (await one(app, 'check_box')).checked;
  assert.ok(['on', 'off'].includes(before));

  await app.locator('check_box').press();
  await sleep(200);

  app = await getApp();
  const after = (await one(app, 'check_box')).checked;
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
    const refreshed = await one(await getApp(), selector);
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
  const itemSelector = '[name^="Item "]';
  const countBefore = await app.locator(itemSelector).count();
  await addBtn.press();
  await sleep(500);

  app = await getApp();
  const countAfter = await app.locator(itemSelector).count();
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
  // Use the named text field if available; fall back to any button.
  const selector = appConfig.textFieldName
    ? `text_field[name="${appConfig.textFieldName}"]`
    : 'button';
  const locator = app.locator(selector);
  if (!(await locator.exists())) return;
  await locator.focus();
});

test('act() helper re-reads the tree after an action', async () => {
  const app = await getApp();
  const addBtn = app.locator('button[name="Add Item"]');
  if (!(await addBtn.exists())) return;
  const updated = await act(addBtn, 'press');
  assert.ok(updated);
});
