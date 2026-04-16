// Element snapshot accessors.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { _makeTestLocator, Element } = require('../../index.js');

async function rootElement() {
  return await _makeTestLocator().element();
}

test('synchronous property getters return the captured snapshot', async () => {
  const app = await rootElement();
  assert.equal(app.role, 'application');
  assert.equal(app.name, 'MockApp');
  assert.equal(app.pid, 4242);
  assert.equal(app.enabled, true);
  assert.equal(app.checked, null);
  assert.equal(app.selected, false);
});

test('children() re-queries the provider', async () => {
  const app = await rootElement();
  const wins = await app.children();
  assert.equal(wins.length, 1);
  assert.equal(wins[0].role, 'window');
  assert.equal(wins[0].focused, true);

  const nested = await wins[0].children();
  const names = nested.map((n) => n.name).sort();
  assert.deepEqual(names, ['Agree', 'Cancel', 'OK', 'Search']);
});

test('parent() walks back up', async () => {
  const app = await rootElement();
  const [win] = await app.children();
  const parent = await win.parent();
  assert.ok(parent);
  assert.equal(parent.role, 'application');
});

test('disabled element reports enabled=false', async () => {
  const [win] = await (await rootElement()).children();
  const buttons = await win.children();
  const cancel = buttons.find((b) => b.name === 'Cancel');
  assert.ok(cancel);
  assert.equal(cancel.enabled, false);
});

test('checked state is exposed as a string enum', async () => {
  const [win] = await (await rootElement()).children();
  const kids = await win.children();
  const checkBox = kids.find((b) => b.role === 'check_box');
  assert.ok(checkBox);
  assert.equal(checkBox.checked, 'on');
});

test('text field exposes editable + value', async () => {
  const [win] = await (await rootElement()).children();
  const kids = await win.children();
  const textField = kids.find((b) => b.role === 'text_field');
  assert.ok(textField);
  assert.equal(textField.editable, true);
  assert.equal(textField.value, 'hello');
});

test('Element instances have the native prototype', async () => {
  const app = await rootElement();
  assert.ok(app instanceof Element);
});
