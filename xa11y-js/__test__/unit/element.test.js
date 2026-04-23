// Element snapshot accessors.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { _makeTestLocator, Element } = require('../../index.js');

async function rootElement() {
  return await _makeTestLocator().element();
}

async function findDescendant(predicate) {
  // Depth-first walk of the mock tree until predicate is satisfied.
  const queue = [await rootElement()];
  while (queue.length > 0) {
    const el = queue.shift();
    if (predicate(el)) return el;
    queue.push(...(await el.children()));
  }
  return null;
}

test('synchronous property getters return the captured snapshot', async () => {
  const app = await rootElement();
  assert.equal(app.role, 'application');
  assert.equal(app.name, 'TestApp');
  assert.equal(app.pid, 1234);
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

  // Window contains a toolbar and a group in the shared mock.
  const nested = await wins[0].children();
  const roles = nested.map((n) => n.role).sort();
  assert.deepEqual(roles, ['group', 'toolbar']);
});

test('parent() walks back up', async () => {
  const app = await rootElement();
  const [win] = await app.children();
  const parent = await win.parent();
  assert.ok(parent);
  assert.equal(parent.role, 'application');
});

test('disabled element reports enabled=false', async () => {
  // "Forward" is the disabled button in the shared mock tree.
  const forward = await findDescendant((el) => el.name === 'Forward');
  assert.ok(forward);
  assert.equal(forward.enabled, false);
});

test('checked state is exposed as a string enum', async () => {
  const checkBox = await findDescendant((el) => el.role === 'check_box');
  assert.ok(checkBox);
  assert.equal(checkBox.checked, 'on');
});

test('text field exposes editable + value', async () => {
  const textField = await findDescendant((el) => el.role === 'text_field');
  assert.ok(textField);
  assert.equal(textField.editable, true);
  assert.equal(textField.value, 'hello');
});

test('Element instances have the native prototype', async () => {
  const app = await rootElement();
  assert.ok(app instanceof Element);
});

test('raw exposes provider-supplied platform metadata', async () => {
  // The shared mock sets raw = {"ax_role": "AXApplication"} on the root so
  // the binding's `raw` getter has a concrete value to serialise.
  const app = await rootElement();
  assert.deepEqual(app.raw, { ax_role: 'AXApplication' });
});

test('raw defaults to an empty object for elements without metadata', async () => {
  const back = await findDescendant((el) => el.name === 'Back');
  assert.ok(back);
  assert.deepEqual(back.raw, {});
});
