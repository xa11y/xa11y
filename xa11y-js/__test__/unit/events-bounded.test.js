// Event announcements, sequence numbers, kinds and the bounded buffer (#480),
// against the mock subscription.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { App, InvalidActionDataError, Subscription, _makeTestApp } = require('../../index.js');

/** The mock app with the public `App` wrapper's methods, as the factories give it. */
function eventsApp() {
  return Object.setPrototypeOf(_makeTestApp(false, true), App.prototype);
}

/** Collect every event the scripted stream yields until it disconnects. */
async function collect(opts) {
  const sub = await eventsApp().subscribe(opts);
  const events = [];
  sub.on('event', (ev) => events.push(ev));
  await new Promise((resolve) => setTimeout(resolve, 300));
  const dropped = sub.dropped;
  sub.close();
  return { events, dropped };
}

test('events are numbered and announcements carry their payload', async () => {
  const { events, dropped } = await collect();
  assert.deepEqual(
    events.map((e) => e.type),
    ['focusChanged', 'announcement', 'announcement', 'announcement'],
  );
  assert.deepEqual(
    events.map((e) => e.sequence),
    [0, 1, 2, 3],
  );
  assert.equal(dropped, 0);
  const times = events.map((e) => e.time);
  assert.deepEqual(times, [...times].sort((a, b) => a - b));
  for (const t of times) assert.ok(Math.abs(t - Date.now()) < 60_000);
  assert.equal(events[0].announcement ?? null, null);
  assert.deepEqual(events[1].announcement, {
    origin: 'request',
    text: 'Saved',
    politeness: 'polite',
    platformPriority: 'MostRecent',
    activityId: 'save',
  });
  assert.equal(events[2].announcement.origin, 'live_region');
  assert.equal(events[2].announcement.text ?? null, null);
  assert.equal(events[3].announcement.politeness, 'assertive');
});

test('kinds keep only what was asked for, spelled as Event.type', async () => {
  const { events } = await collect({ kinds: ['announcement'] });
  assert.deepEqual(
    events.map((e) => e.sequence),
    [1, 2, 3],
  );
  const focus = await collect({ kinds: ['focusChanged'] });
  assert.deepEqual(
    focus.events.map((e) => e.type),
    ['focusChanged'],
  );
});

test('a small buffer drops the oldest and counts them', async () => {
  const { events, dropped } = await collect({ buffer: 2 });
  assert.deepEqual(
    events.map((e) => e.sequence),
    [2, 3],
  );
  assert.equal(dropped, 2);
});

test('bad subscribe options are refused', async () => {
  await assert.rejects(eventsApp().subscribe({ kinds: ['focus'] }), InvalidActionDataError);
  // The error lists the JS spellings, the ones `kinds` takes, not Python's.
  await assert.rejects(eventsApp().subscribe({ kinds: ['focus'] }), (err) => {
    assert.match(err.message, /expected one of focusChanged, valueChanged/);
    assert.doesNotMatch(err.message, /focus_changed/);
    return true;
  });
  // The core's snake_case spelling is not an `EventTypeName`, so it is refused
  // rather than silently accepted.
  await assert.rejects(
    eventsApp().subscribe({ kinds: ['focus_changed'] }),
    InvalidActionDataError,
  );
  await assert.rejects(eventsApp().subscribe({ buffer: 0 }), InvalidActionDataError);
});

// ── Element.subscribe: the same shape as App.subscribe ─────────────────────

/** The mock app's root as an `Element`, with events enabled. */
function eventsElement() {
  return _makeTestApp(false, true).asElement();
}

test('Element.subscribe resolves to the EventEmitter Subscription', async () => {
  const sub = await eventsElement().subscribe({ kinds: ['announcement'], buffer: 2 });
  try {
    assert.ok(sub instanceof Subscription, 'must be the public Subscription');
    const events = [];
    sub.on('announcement', (ev) => events.push(ev));
    await new Promise((resolve) => setTimeout(resolve, 300));
    // Three announcements into a buffer of two: the oldest is dropped.
    assert.deepEqual(
      events.map((e) => e.sequence),
      [2, 3],
    );
    assert.equal(sub.dropped, 1);
  } finally {
    sub.close();
  }
  assert.equal(sub.closed, true);
});

test('Element.subscribe with no options takes every event', async () => {
  const sub = await eventsElement().subscribe();
  const events = [];
  sub.on('event', (ev) => events.push(ev));
  await new Promise((resolve) => setTimeout(resolve, 300));
  sub.close();
  assert.deepEqual(
    events.map((e) => e.type),
    ['focusChanged', 'announcement', 'announcement', 'announcement'],
  );
});

test('Element.subscribe closes on an aborted signal', async () => {
  const ctrl = new AbortController();
  const sub = await eventsElement().subscribe({ signal: ctrl.signal });
  assert.equal(sub.closed, false);
  ctrl.abort();
  assert.equal(sub.closed, true);

  const pre = new AbortController();
  pre.abort();
  const already = await eventsElement().subscribe({ signal: pre.signal });
  assert.equal(already.closed, true);
});

test('subscribe refuses the old positional shape on Element and App', async () => {
  for (const target of [eventsElement(), eventsApp()]) {
    await assert.rejects(target.subscribe(['focusChanged']), (err) => {
      assert.ok(err instanceof InvalidActionDataError, `got ${err}`);
      assert.match(err.message, /pass an options object \{ kinds, buffer, signal \}/);
      return true;
    });
    await assert.rejects(target.subscribe(4096), InvalidActionDataError);
    await assert.rejects(target.subscribe(null, 16), InvalidActionDataError);
  }
  await assert.rejects(eventsElement().subscribe({ kinds: ['focus'] }), InvalidActionDataError);
  await assert.rejects(eventsElement().subscribe({ buffer: 0 }), InvalidActionDataError);
});
