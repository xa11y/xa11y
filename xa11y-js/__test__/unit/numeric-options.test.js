// Numeric options arrive as raw JS numbers and are checked in the binding.
// napi's `u32` conversion applies ToUint32, so before this `-1` became
// 4294967295 (an unbounded limit, a ~50-day timeout), `NaN` became 0 and
// `1.5` became 1. Each must now reject with InvalidActionDataError before any
// platform call. Also covers the queue between the event worker and JS,
// which is bounded at the subscription's buffer.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const {
  App,
  InvalidActionDataError,
  ShellSurface,
  _makeTestApp,
  _makeTestFloodSubscription,
} = require('../../index.js');

const BAD = [-1, Number.NaN, 1.5, Number.POSITIVE_INFINITY];

function eventsApp() {
  return Object.setPrototypeOf(_makeTestApp(false, true), App.prototype);
}

for (const option of ['maxDepth', 'maxNodes', 'maxTextBytes', 'maxRuns', 'timeout']) {
  test(`capture refuses a bad ${option}`, async () => {
    for (const value of BAD) {
      await assert.rejects(
        async () => _makeTestApp().capture({ [option]: value }),
        (err) => err instanceof InvalidActionDataError && err.message.includes(option),
        `${option}: ${value}`,
      );
    }
  });
}

test('capture still takes whole-number options', async () => {
  const cap = await _makeTestApp().capture({ maxDepth: 0, maxNodes: 5, timeout: 5000 });
  assert.equal(cap.nodes.length, 1);
});

test('subscribe refuses a bad buffer', async () => {
  for (const value of BAD) {
    await assert.rejects(
      eventsApp().subscribe({ buffer: value }),
      (err) => err instanceof InvalidActionDataError && err.message.includes('buffer'),
      `buffer: ${value}`,
    );
    await assert.rejects(
      eventsApp().asElement().subscribe(null, value),
      InvalidActionDataError,
      `Element.subscribe buffer: ${value}`,
    );
  }
});

test('App lookups refuse a bad timeout', async () => {
  for (const value of BAD) {
    await assert.rejects(App.byName('NoSuchApp', { timeout: value }), InvalidActionDataError);
    await assert.rejects(App.byPid(1, { timeout: value }), InvalidActionDataError);
    await assert.rejects(App.foreground({ timeout: value }), InvalidActionDataError);
    await assert.rejects(ShellSurface.byKind('dock', { timeout: value }), InvalidActionDataError);
  }
});

test('the queue to JS holds `buffer` events and counts what it drops', async () => {
  const native = _makeTestFloodSubscription(10, 3);
  // A wake-up that does not drain: JS has fallen behind.
  native.start(() => {});
  const deadline = Date.now() + 5000;
  while (native.dropped < 7 && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  const events = native.drain();
  native.close();
  // The oldest went first; the sequence gap is what was dropped.
  assert.deepEqual(
    events.map((e) => e.sequence),
    [7, 8, 9],
  );
  assert.equal(native.dropped, 7);
});

test('a queue that keeps up drops nothing', async () => {
  const native = _makeTestFloodSubscription(10, 100);
  native.start(() => {});
  await new Promise((resolve) => setTimeout(resolve, 300));
  const events = native.drain();
  native.close();
  assert.equal(events.length, 10);
  assert.equal(native.dropped, 0);
});
