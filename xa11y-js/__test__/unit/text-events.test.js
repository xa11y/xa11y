// Text events: `event.textChange` and the `textSelectionChanged` type.
//
// Driven by a scripted native subscription that yields real core events
// (`xa11y::mock::text_edit_events`), so the conversion under test is the one a
// live subscription uses.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { _makeTestTextEventSubscription, _Subscription } = require('../../index.js');

function collect(count) {
  const sub = new _Subscription(_makeTestTextEventSubscription());
  const received = [];
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      sub.close();
      reject(new Error(`only ${received.length} of ${count} events arrived`));
    }, 5000);
    sub.on('event', (ev) => {
      received.push(ev);
      if (received.length === count) {
        clearTimeout(timer);
        sub.close();
        resolve(received);
      }
    });
  });
}

test('text events cross with their payloads', async () => {
  const [inserted, deleted, bare, caret] = await collect(4);

  assert.equal(inserted.type, 'textChanged');
  assert.deepEqual(inserted.textChange, {
    kind: 'inserted',
    range: { start: 3, end: 7 },
    text: 'hé😀',
  });
  // UTF-16 units are JS string indices: the range covers the text exactly.
  assert.equal(inserted.textChange.range.end - inserted.textChange.range.start, 'hé😀'.length);

  assert.deepEqual(deleted.textChange, { kind: 'deleted', range: null, text: 'x' });

  // A textChanged whose platform supplies no delta.
  assert.equal(bare.type, 'textChanged');
  assert.equal(bare.textChange, null);

  assert.equal(caret.type, 'textSelectionChanged');
  assert.equal(caret.textChange, null);
});
