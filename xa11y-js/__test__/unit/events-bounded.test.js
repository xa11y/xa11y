// Event announcements, sequence numbers, kinds and the bounded buffer (#480),
// against the mock subscription.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { App, InvalidActionDataError, _makeTestApp } = require('../../index.js');

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
  await assert.rejects(eventsApp().subscribe({ buffer: 0 }), InvalidActionDataError);
});
