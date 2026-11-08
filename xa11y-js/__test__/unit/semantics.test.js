// Document semantics (#476) against the mock document fixture.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const {
  ActionNotSupportedError,
  InvalidActionDataError,
  InvalidSelectorError,
  _makeTestDocumentLocator,
} = require('../../index.js');

const one = (selector) => _makeTestDocumentLocator(selector).element();

test('landmarks, headings, links and live regions are getters', async () => {
  const main = await one('landmark[landmark_type="main"]');
  assert.equal(main.role, 'landmark');
  assert.equal(main.landmarkType, 'main');
  assert.equal(main.webRole, 'main');
  assert.equal((await one('heading[heading_level="2"]')).headingLevel, 2);
  const link = await one('link[url*="example.com"]');
  assert.equal(link.linkUrl, 'https://example.com/docs');
  assert.equal(link.liveRegion, null);
  const status = await one('[live="polite"]');
  assert.deepEqual(status.liveRegion, {
    politeness: 'polite',
    atomic: true,
    relevant: 'additions text',
  });
});

test('navigation is refused with its replacement', async () => {
  await assert.rejects(one('navigation'), (e) => {
    assert.ok(e instanceof InvalidSelectorError, e);
    assert.match(e.message, /landmark_type/);
    return true;
  });
});

test('relations come back by kind, in request order', async () => {
  const field = await one('text_field[name="Email"]');
  const rel = await field.relations(['described_by', 'labelled_by', 'controls']);
  assert.deepEqual(
    rel.map((r) => r.kind),
    ['described_by', 'labelled_by', 'controls'],
  );
  assert.equal(rel[1].elements[0].name, 'Email address');
  assert.equal(rel[0].elements[0].name, 'We never share it');
  assert.deepEqual(rel[2].elements, []);
  await assert.rejects(field.relations(['linked']), ActionNotSupportedError);
  assert.throws(() => field.relations(['labeled_by']), InvalidActionDataError);
});
