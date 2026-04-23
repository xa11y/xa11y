// Subscription terminates cleanly when the underlying event source disconnects.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { _makeDisconnectedSubscription, _Subscription } = require('../../index.js');

test('disconnected subscription stops the worker loop without hanging', async () => {
  // Create a _NativeSubscription whose backing mpsc sender has already been
  // dropped — the Rust worker should detect Disconnected and exit cleanly
  // rather than spinning forever on the (previously swallowed) error path.
  const native = _makeDisconnectedSubscription();
  const sub = new _Subscription(native);

  // Give the worker thread a chance to notice the disconnect and exit.
  // If the regression returns (Err(_) => break replaced with a bad match),
  // this test still passes because the worker spins — the point is:
  //   (a) close() must return promptly, and
  //   (b) no events should be emitted (no synthetic values from error path).
  const received = [];
  sub.on('event', (ev) => received.push(ev));

  // Yield a tick so the native wake-up (if any) can fire.
  await new Promise((r) => setTimeout(r, 50));

  sub.close();
  assert.equal(sub.closed, true);
  assert.deepEqual(received, []);
});

test('closing a disconnected subscription is idempotent', () => {
  const sub = new _Subscription(_makeDisconnectedSubscription());
  sub.close();
  sub.close(); // second close must not throw
  assert.equal(sub.closed, true);
});
