// tree() and dump() against the mock provider.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { _makeTestLocator } = require('../../index.js');

function root() {
  return _makeTestLocator();
}

async function rootElement() {
  return await root().element();
}

// ── Element.tree() ─────────────────────────────────────────────────────────

test('tree() root has correct role and name', async () => {
  const node = await (await rootElement()).tree();
  assert.equal(node.role, 'application');
  assert.equal(node.name, 'TestApp');
  assert.equal(node.value, undefined);
});

test('tree() without max_depth includes all descendants', async () => {
  const node = await (await rootElement()).tree();
  assert.equal(node.children.length, 1);
  const win = node.children[0];
  assert.equal(win.role, 'window');
  assert.equal(win.children.length, 2);
});

test('tree(0) returns only the root with no children', async () => {
  const node = await (await rootElement()).tree(0);
  assert.equal(node.role, 'application');
  assert.deepEqual(node.children, []);
});

test('tree(1) returns root + children but no grandchildren', async () => {
  const node = await (await rootElement()).tree(1);
  assert.equal(node.children.length, 1);
  assert.deepEqual(node.children[0].children, []);
});

test('tree() exposes value on nodes that have one', async () => {
  const textField = await root().descendant('text_field').element();
  const node = await textField.tree(0);
  assert.equal(node.value, 'hello');
});

test('tree() leaf node has empty children array', async () => {
  const back = await root().descendant('button[name="Back"]').element();
  const node = await back.tree();
  assert.equal(node.role, 'button');
  assert.deepEqual(node.children, []);
});

// ── Element.dump() ─────────────────────────────────────────────────────────

test('dump() returns a string', async () => {
  const text = await (await rootElement()).dump();
  assert.equal(typeof text, 'string');
});

test('dump() contains role and name', async () => {
  const text = await (await rootElement()).dump();
  assert.ok(text.includes('application "TestApp"'));
});

test('dump() is indented', async () => {
  const text = await (await rootElement()).dump();
  assert.ok(text.includes('  window "Main Window"'));
});

test('dump(0) produces exactly one non-empty line', async () => {
  const text = await (await rootElement()).dump(0);
  const lines = text.split('\n').filter((l) => l.trim());
  assert.equal(lines.length, 1);
  assert.ok(lines[0].includes('application'));
});

test('dump() includes value for nodes that have one', async () => {
  const textField = await root().descendant('text_field').element();
  const text = await textField.dump(0);
  assert.ok(text.includes('value="hello"'));
});

