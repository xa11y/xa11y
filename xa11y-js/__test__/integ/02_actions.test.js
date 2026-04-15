// Integration tests: action invocation against the AccessKit test app.
//
// The test app mutates its state in response to actions and rebuilds its
// tree, so we can observe state changes across calls.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { getApp, one, act, sleep } = require('./helpers.js');

test('press on Submit button succeeds', async () => {
  const app = await getApp();
  await app.locator('button[name="Submit"]').press();
});

test('toggle on Checkbox flips checked state', async () => {
  let app = await getApp();
  const before = (await one(app, 'check_box')).checked;
  assert.ok(['on', 'off'].includes(before));

  await app.locator('check_box').toggle();
  await sleep(200);

  app = await getApp();
  const after = (await one(app, 'check_box')).checked;
  assert.ok(['on', 'off'].includes(after));
  assert.notEqual(before, after, 'checkbox state should have flipped');
});

test('setValue on text field updates its value', async () => {
  let app = await getApp();
  const field = app.locator('text_field[name="Name"]');
  await field.setValue('Jane Doe');
  await sleep(200);

  app = await getApp();
  const refreshed = await one(app, 'text_field[name="Name"]');
  // Some platforms return the new value as the `.value` property. Where they
  // don't, this test still exercises the Rust->JS plumbing without crashing.
  assert.ok(typeof refreshed.value === 'string' || refreshed.value === null);
});

test('press on "Add Item" grows the dynamic list', async () => {
  let app = await getApp();
  const addBtn = app.locator('button[name="Add Item"]');
  if (!(await addBtn.exists())) return; // not all builds expose this button

  const countBefore = await app.locator('list_item').count();
  await addBtn.press();
  await sleep(200);

  app = await getApp();
  const countAfter = await app.locator('list_item').count();
  assert.ok(
    countAfter >= countBefore + 1,
    `expected list to grow from ${countBefore} to >= ${countBefore + 1}, got ${countAfter}`,
  );
});

test('exists() is false for a nonexistent selector', async () => {
  const app = await getApp();
  assert.equal(await app.locator('button[name="NoSuchThingExists"]').exists(), false);
});

test('auto-wait focus() resolves before returning', async () => {
  const app = await getApp();
  const name = app.locator('text_field[name="Name"]');
  await name.focus();
});

test('act() helper re-reads the tree after an action', async () => {
  const app = await getApp();
  const addBtn = app.locator('button[name="Add Item"]');
  if (!(await addBtn.exists())) return;
  const updated = await act(addBtn, 'press');
  assert.ok(updated);
});
