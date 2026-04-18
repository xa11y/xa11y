// App factory method dispatch — regression tests for GitHub issue #113.
//
// The static factories `App.list()`, `App.byName()`, and `App.byPid()`
// delegate to napi-generated `native.App.*` methods, which allocate
// `native.App` instances. The wrapper class must rewire the returned
// instances' prototype to `App.prototype` so that `.subscribe()` and
// `.waitForEvent()` dispatch to the EventEmitter-based wrapper rather
// than the raw `_NativeSubscription`.
//
// The napi-generated `native.App` methods are sealed (non-configurable,
// non-writable), so we can't monkey-patch them. Instead, we substitute
// a stub for `./native.js` in `require.cache` BEFORE requiring
// `./index.js`, so the wrapper captures our stub as its `native`
// reference. Node's `--test` runner runs each test file in an isolated
// worker, so this substitution doesn't affect other suites.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { EventEmitter } = require('node:events');

// ── Stub native bindings ───────────────────────────────────────────────────

class NativeAppStub {
  static __listReturn;
  static __byNameReturn;
  static __byPidReturn;
  static __byNameLastArg;
  static __byPidLastArg;

  static async list() {
    return NativeAppStub.__listReturn ?? [];
  }
  static async byName(name) {
    NativeAppStub.__byNameLastArg = name;
    return NativeAppStub.__byNameReturn ?? new NativeAppStub();
  }
  static async byPid(pid) {
    NativeAppStub.__byPidLastArg = pid;
    return NativeAppStub.__byPidReturn ?? new NativeAppStub();
  }

  async subscribe() {
    return new NativeSubscriptionStub();
  }
}

class NativeSubscriptionStub {
  start() {}
  drain() {
    return [];
  }
  close() {}
}

// Install the stub into `require.cache` so that `index.js`'s internal
// `require('./native.js')` resolves to our module object instead of
// the real napi binding.
const path = require('node:path');
const nativePath = require.resolve('../../native.js');
require.cache[nativePath] = {
  id: nativePath,
  filename: nativePath,
  loaded: true,
  path: path.dirname(nativePath),
  exports: {
    App: NativeAppStub,
    Element: class {},
    Event: class {},
    Locator: class {},
    _NativeSubscription: NativeSubscriptionStub,
    NativeSubscription: NativeSubscriptionStub,
    _makeTestLocator: () => {},
    locator: () => {},
  },
};

// Now pull the wrapper — it captures `NativeAppStub` as its parent.
const { App, Subscription } = require('../../index.js');

// ── Tests ──────────────────────────────────────────────────────────────────

test('App.list() returns instances with the wrapper prototype (#113)', async () => {
  NativeAppStub.__listReturn = [new NativeAppStub()];
  const apps = await App.list();
  assert.equal(apps.length, 1);
  assert.strictEqual(
    Object.getPrototypeOf(apps[0]),
    App.prototype,
    'list() return must have the wrapper prototype, not native.App.prototype',
  );
  assert.strictEqual(
    apps[0].subscribe,
    App.prototype.subscribe,
    '.subscribe must dispatch to the wrapper (EventEmitter Subscription)',
  );
  assert.strictEqual(
    apps[0].waitForEvent,
    App.prototype.waitForEvent,
    '.waitForEvent must dispatch to the wrapper',
  );
});

test('App.byName() returns an instance with the wrapper prototype (#113)', async () => {
  NativeAppStub.__byNameReturn = new NativeAppStub();
  const app = await App.byName('Calculator');
  assert.equal(NativeAppStub.__byNameLastArg, 'Calculator', 'name arg propagates to native');
  assert.strictEqual(
    Object.getPrototypeOf(app),
    App.prototype,
    'byName() return must have the wrapper prototype',
  );
  assert.strictEqual(app.subscribe, App.prototype.subscribe);
  assert.strictEqual(app.waitForEvent, App.prototype.waitForEvent);
});

test('App.byPid() returns an instance with the wrapper prototype (#113)', async () => {
  NativeAppStub.__byPidReturn = new NativeAppStub();
  const app = await App.byPid(9876);
  assert.equal(NativeAppStub.__byPidLastArg, 9876, 'pid arg propagates to native');
  assert.strictEqual(
    Object.getPrototypeOf(app),
    App.prototype,
    'byPid() return must have the wrapper prototype',
  );
  assert.strictEqual(app.subscribe, App.prototype.subscribe);
  assert.strictEqual(app.waitForEvent, App.prototype.waitForEvent);
});

test('App.list() rewires every returned instance (not just the first)', async () => {
  NativeAppStub.__listReturn = [new NativeAppStub(), new NativeAppStub(), new NativeAppStub()];
  const apps = await App.list();
  assert.equal(apps.length, 3);
  for (let i = 0; i < apps.length; i++) {
    assert.strictEqual(
      Object.getPrototypeOf(apps[i]),
      App.prototype,
      `every element must be rewired (index ${i})`,
    );
  }
});

test('subscribed apps from factories produce an EventEmitter Subscription', async () => {
  // End-to-end assertion of the consumer-observable bug: the value
  // returned by `.subscribe()` is an `EventEmitter`-based `Subscription`,
  // not a `_NativeSubscription`.
  NativeAppStub.__byNameReturn = new NativeAppStub();
  const app = await App.byName('anything');
  const sub = await app.subscribe();
  assert.ok(sub instanceof Subscription, 'subscribe() must return a wrapper Subscription');
  assert.ok(sub instanceof EventEmitter, 'Subscription must be an EventEmitter');
  assert.equal(typeof sub.on, 'function', '.on must be callable');
  assert.equal(typeof sub.waitForEvent, 'function', '.waitForEvent must be callable');
  sub.close();
});
