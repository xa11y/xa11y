// Bounded capture (#482) through the JS binding, against the mock.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const {
  Element,
  InvalidActionDataError,
  _makeTestApp,
  _makeTestDocumentLocator,
  _makeTestTableLocator,
} = require('../../index.js');

test('a capture is pre-order with indices that agree', async () => {
  const cap = await _makeTestApp().capture();
  assert.ok(cap.nodes.length > 1);
  assert.deepEqual(cap.truncated, {
    byDepth: false,
    byNodes: false,
    byText: false,
    byTime: false,
  });
  assert.equal(cap.vanished, 0);
  assert.ok(cap.started <= cap.finished);
  const root = cap.nodes[0];
  assert.equal(root.index, 0);
  assert.equal(root.parent ?? null, null);
  assert.equal(root.element.role, 'application');
  for (const node of cap.nodes) {
    assert.equal(node.childrenCount, node.children.length);
    assert.equal(node.childrenTruncated, false);
    assert.deepEqual(node.errors, []);
    for (const child of node.children) {
      assert.equal(cap.nodes[child].parent, node.index);
      assert.ok(child > node.index);
    }
  }
  const names = cap.nodes.map((n) => n.element.name);
  assert.ok(names.indexOf('Navigation') < names.indexOf('Back'));
});

test('the node and depth limits cut and say where', async () => {
  const cut = await _makeTestApp().capture({ maxNodes: 3 });
  assert.equal(cut.nodes.length, 3);
  assert.equal(cut.truncated.byNodes, true);
  const marked = cut.nodes.filter((n) => n.childrenTruncated);
  assert.ok(marked.length > 0);
  for (const n of marked) assert.ok(n.childrenCount > n.children.length);

  const shallow = await _makeTestApp().capture({ maxDepth: 1 });
  assert.equal(shallow.truncated.byDepth, true);
  assert.equal(Math.max(...shallow.nodes.map((n) => n.depth)), 1);
});

test('a captured node is the element itself', async () => {
  const cap = await _makeTestApp().capture();
  const back = cap.nodes.find((n) => n.element.name === 'Back');
  assert.ok(back.element instanceof Element);
  assert.equal(await back.element.sameAs(back.element), true);
});

test('relations resolve to indices inside the capture', async () => {
  const cap = await _makeTestDocumentLocator('window').capture({ detail: ['relations'] });
  const field = cap.nodes.find((n) => n.element.name === 'Email');
  const labelled = field.relations.find((r) => r.kind === 'labelled_by');
  assert.equal(labelled.targets.length, 1);
  assert.equal(typeof labelled.targets[0], 'number');
  assert.equal(cap.nodes[labelled.targets[0]].element.name, 'Email address');
  // Every other kind this platform reports is listed, empty.
  const others = field.relations.filter((r) => !['labelled_by', 'described_by'].includes(r.kind));
  assert.ok(others.length > 0);
  for (const r of others) assert.deepEqual(r.targets, []);

  const alone = await field.element.capture({ detail: ['relations'] });
  const outside = alone.nodes[0].relations.find((r) => r.kind === 'labelled_by').targets[0];
  assert.ok(outside instanceof Element);
  assert.equal(outside.name, 'Email address');
});

test('text detail reads within the budget', async () => {
  const notes = _makeTestTableLocator().descendant('text_area[name="Notes"]');
  const full = (await notes.capture({ detail: ['text'] })).nodes[0].text;
  assert.deepEqual(full, {
    content: 'Hello 😀 world\nsecond line',
    length: 26,
    truncated: false,
  });
  const cut = await notes.capture({ detail: ['text'], maxTextBytes: 8 });
  assert.equal(cut.truncated.byText, true);
  assert.equal(cut.nodes[0].text.truncated, true);
  assert.equal((await notes.capture()).nodes[0].text ?? null, null);
});

test('a spent time budget resolves with a partial capture', async () => {
  const cap = await _makeTestApp().capture({ timeout: 0 });
  assert.equal(cap.truncated.byTime, true);
  assert.equal(cap.nodes.length, 1);
});

test('bad options are refused', async () => {
  await assert.rejects(_makeTestApp().capture({ maxNodes: 0 }), InvalidActionDataError);
  assert.throws(() => _makeTestApp().capture({ detail: ['formatting'] }), InvalidActionDataError);
});
