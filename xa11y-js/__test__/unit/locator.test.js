// Locator behaviour against the Rust mock provider.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const xa11y = require('../../index.js');
const { _makeTestLocator, Locator, SelectorNotMatchedError } = xa11y;

function root() {
  return _makeTestLocator();
}

test('count matches the mock tree', async () => {
  assert.equal(await root().count(), 1);
  const buttons = root().descendant('button');
  assert.equal(await buttons.count(), 2);
});

test('exists() does not throw on miss', async () => {
  assert.equal(await root().descendant('button[name="Back"]').exists(), true);
  assert.equal(await root().descendant('button[name="NoSuch"]').exists(), false);
});

test('element() resolves to a live Element snapshot', async () => {
  const el = await root().descendant('button[name="Back"]').element();
  assert.equal(el.role, 'button');
  assert.equal(el.name, 'Back');
  assert.equal(el.enabled, true);
  assert.deepEqual(el.actions.sort(), ['focus', 'press']);
});

test('element() throws SelectorNotMatchedError on miss', async () => {
  await assert.rejects(
    root().descendant('button[name="Nope"]').element(),
    (err) => err instanceof SelectorNotMatchedError,
  );
});

test('elements() returns all matches', async () => {
  const buttons = await root().descendant('button').elements();
  assert.equal(buttons.length, 2);
  assert.deepEqual(
    buttons.map((b) => b.name).sort(),
    ['Back', 'Forward'],
  );
});

test('child() narrows the selector', async () => {
  const win = root().child('window');
  assert.equal(await win.count(), 1);
  const button = win.descendant('button[name="Back"]');
  assert.equal(await button.exists(), true);
});

test('nth() is 1-based', async () => {
  const first = await root().descendant('button').first().element();
  const second = await root().descendant('button').nth(2).element();
  assert.notEqual(first.name, second.name);
});

test('locator properties are preserved across chains', () => {
  const base = root();
  const derived = base.child('window').descendant('button[name="Back"]');
  assert.ok(derived instanceof Locator);
  assert.ok(typeof derived.selector === 'string');
  assert.ok(derived.selector.includes('button'));
});
