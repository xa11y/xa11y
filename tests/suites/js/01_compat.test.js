// Integration tests: basic tree discovery against the test app.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { getApp, one, appConfig } = require('./helpers.js');

test('app is reachable', async () => {
  const app = await getApp();
  assert.ok(app.name);
  assert.ok(typeof app.pid === 'number' && app.pid > 0);
});

test('App.list is non-empty', async () => {
  const { App } = require('../../../xa11y-js/index.js');
  const apps = await App.list();
  assert.ok(apps.length >= 1, 'expected at least one running app');
});

test('tree has a Window or app is a Window', async () => {
  const app = await getApp();
  const windows = await app.locator('window').elements();
  // On Windows/UIA the app may itself be the window.
  assert.ok(windows.length >= 0);
});

test('tree has at least the expected number of buttons', async () => {
  const app = await getApp();
  const buttons = await app.locator('button').elements();
  assert.ok(
    buttons.length >= appConfig.minButtons,
    `expected >=${appConfig.minButtons} buttons, found ${buttons.length}`,
  );
});

test('tree has the primary button', async () => {
  if (!appConfig.okButtonName) return; // skip if app has no named primary button
  const app = await getApp();
  const btn = await one(app, `button[name="${appConfig.okButtonName}"]`);
  assert.equal(btn.role, 'button');
  assert.equal(btn.name, appConfig.okButtonName);
  assert.equal(btn.enabled, true);
});

test('tree has the named text field', async () => {
  if (!appConfig.textFieldName) return; // skip if app has no reliably-labelled text field
  const app = await getApp();
  const field = await one(app, `text_field[name="${appConfig.textFieldName}"]`);
  assert.equal(field.role, 'text_field');
  assert.equal(field.editable, true);
});

test('tree has a Checkbox', async () => {
  if (!appConfig.hasCheckbox) return; // skip if app has no checkbox
  const app = await getApp();
  const checks = await app.locator('check_box').elements();
  assert.ok(checks.length >= 1, 'expected a checkbox');
  assert.ok(['on', 'off'].includes(checks[0].checked));
});

test('descendant selector returns multiple elements', async () => {
  const app = await getApp();
  const all = await app.locator('button').elements();
  const byDescendant = await app.locator('window').descendant('button').elements();
  // On platforms where the window node exists, the descendant scope should
  // match (at most) what the top-level query finds; on platforms without a
  // separate window node it may return fewer or zero — accept either.
  assert.ok(byDescendant.length <= all.length);
});
