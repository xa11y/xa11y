// Element identity and staleness (#481), against the mock provider. An
// element is the reference to one node: an action reaches that node or fails
// with ElementStaleError, never re-matching a similarly named one.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const {
  ElementStaleError,
  SelectorNotMatchedError,
  _makeTestStaleElements,
} = require('../../index.js');

test('two finds of one node are the same node', async () => {
  const [back, backAgain] = _makeTestStaleElements('Back', 'none');
  const [forward] = _makeTestStaleElements('Forward', 'none');
  assert.equal(await back.sameAs(backAgain), true);
  assert.equal(await back.sameAs(forward), false);
  await back.checkAlive();
});

test('an action on a removed node rejects stale and does nothing', async () => {
  const [back] = _makeTestStaleElements('Back', 'removed');
  await assert.rejects(back.press(), (err) => {
    assert.ok(err instanceof ElementStaleError, err);
    assert.ok(err instanceof SelectorNotMatchedError, 'existing handlers keep catching it');
    assert.equal(err.reason, 'removed');
    assert.match(err.element, /Back/);
    // The parent class's `selector` names the same thing, as in Python.
    assert.equal(err.selector, err.element);
    assert.equal(err.delivery, 'not_delivered');
    assert.match(err.message, /^Element stale \(removed\)/);
    return true;
  });
  await assert.rejects(back.checkAlive(), ElementStaleError);
});

test('a reused id with another role is identity_changed', async () => {
  const [back] = _makeTestStaleElements('Back', 'replaced');
  for (const act of [() => back.press(), () => back.focus(), () => back.checkAlive()]) {
    await assert.rejects(
      act(),
      (err) => err instanceof ElementStaleError && err.reason === 'identity_changed',
    );
  }
});
