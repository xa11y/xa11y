// tree() and dump() against the mock provider.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { _makeTestApp, _makeTestLocator, SelectorNotMatchedError } = require('../../index.js');

function root() {
  return _makeTestLocator();
}

async function rootElement() {
  return await root().element();
}

function mockApp() {
  return _makeTestApp();
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

// ── App.tree() / App.dump() ────────────────────────────────────────────────

test('App.tree() returns the application root', async () => {
  const node = await mockApp().tree();
  assert.equal(node.role, 'application');
  assert.equal(node.name, 'TestApp');
  assert.ok(node.children.length >= 1);
});

test('App.tree(0) returns only the application node', async () => {
  const node = await mockApp().tree(0);
  assert.equal(node.role, 'application');
  assert.deepEqual(node.children, []);
});

test('App.tree(1) stops at direct children', async () => {
  const node = await mockApp().tree(1);
  assert.ok(node.children.length >= 1);
  for (const child of node.children) {
    assert.deepEqual(child.children, []);
  }
});

test('App.dump() returns a string containing the application root', async () => {
  const text = await mockApp().dump();
  assert.equal(typeof text, 'string');
  assert.ok(text.includes('application "TestApp"'));
});

test('App.dump(0) produces exactly one non-empty line', async () => {
  const text = await mockApp().dump(0);
  const lines = text.split('\n').filter((l) => l.trim());
  assert.equal(lines.length, 1);
});

test('App.dump() matches Element.dump() on the app root', async () => {
  const fromApp = await mockApp().dump();
  const fromElement = await (await rootElement()).dump();
  assert.equal(fromApp, fromElement);
});

// ── App.isForeground / App.focused ─────────────────────────────────────────

test('App.isForeground is true for the mock foreground app', () => {
  // The mock reports its root as the foreground app and `_makeTestApp`
  // resolves it via the predicate finder, so the flag is tagged.
  const app = mockApp();
  assert.equal(typeof app.isForeground, 'boolean');
  assert.equal(app.isForeground, true);
});

test('App.focused is a deprecated alias equal to isForeground', () => {
  const app = mockApp();
  assert.equal(app.focused, app.isForeground);
  assert.equal(app.focused, true);
});

// ── Locator.tree() / Locator.dump() ────────────────────────────────────────

test('Locator.tree() returns the matched subtree', async () => {
  const node = await root().tree();
  assert.equal(node.role, 'application');
  assert.equal(node.name, 'TestApp');
});

test('Locator.tree() scopes to the selector', async () => {
  const node = await root().descendant('toolbar').tree();
  assert.equal(node.role, 'toolbar');
  assert.equal(node.children.length, 2);
});

test('Locator.tree(0) drops children', async () => {
  const node = await root().descendant('toolbar').tree(0);
  assert.equal(node.role, 'toolbar');
  assert.deepEqual(node.children, []);
});

test('Locator.dump() returns a string of the matched subtree', async () => {
  const text = await root().descendant('toolbar').dump();
  assert.equal(typeof text, 'string');
  assert.ok(text.includes('toolbar'));
});

test('Locator.dump(0) is one line', async () => {
  const text = await root().descendant('toolbar').dump(0);
  const lines = text.split('\n').filter((l) => l.trim());
  assert.equal(lines.length, 1);
});

test('Locator.tree() rejects with SelectorNotMatchedError on miss', async () => {
  await assert.rejects(
    root().descendant('button[name="DoesNotExist"]').tree(),
    SelectorNotMatchedError,
  );
});

test('Locator.dump() fails fast on miss (no auto-wait)', async () => {
  const start = process.hrtime.bigint();
  await assert.rejects(
    root().descendant('button[name="DoesNotExist"]').dump(),
    SelectorNotMatchedError,
  );
  const elapsedMs = Number(process.hrtime.bigint() - start) / 1e6;
  assert.ok(elapsedMs < 500, `dump should fail fast, took ${elapsedMs}ms`);
});

