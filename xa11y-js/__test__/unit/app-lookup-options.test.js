// Tests for the `App.byName` / `App.byPid` options forwarding.
//
// The actual polling behavior (default 5s, `timeout: 0` as single-attempt,
// retry semantics) is covered by the Rust unit tests in
// `xa11y/tests/unit_test.rs`. These tests cover the JS-binding-specific
// concern: that `options` reaches the native binding unchanged, including
// the case where the caller omits it entirely (so the native-side default
// kicks in).
//
// Uses the same `require.cache` stub trick as `app-factories.test.js` —
// substitute `./native.js` before requiring `./index.js`, so the wrapper
// captures our stub and we can observe what it forwards.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

// ── Stub native bindings ───────────────────────────────────────────────────

class NativeAppStub {
  static __byNameLastArgs;
  static __byPidLastArgs;

  static async byName(name, options) {
    NativeAppStub.__byNameLastArgs = { name, options };
    return new NativeAppStub();
  }
  static async byPid(pid, options) {
    NativeAppStub.__byPidLastArgs = { pid, options };
    return new NativeAppStub();
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
    _makeDisconnectedSubscription: () => new NativeSubscriptionStub(),
    locator: () => {},
  },
};

const { App } = require('../../index.js');

// ── Tests ──────────────────────────────────────────────────────────────────

test('App.byName forwards options unchanged to native', async () => {
  await App.byName('Calc', { timeout: 1500 });
  assert.deepEqual(NativeAppStub.__byNameLastArgs, {
    name: 'Calc',
    options: { timeout: 1500 },
  });
});

test('App.byName forwards timeout: 0 (no-wait sentinel) unchanged', async () => {
  await App.byName('Calc', { timeout: 0 });
  assert.deepEqual(NativeAppStub.__byNameLastArgs, {
    name: 'Calc',
    options: { timeout: 0 },
  });
});

test('App.byName forwards undefined options when caller omits them', async () => {
  // The default 5s lives in the native binding (see DEFAULT_LOOKUP_TIMEOUT
  // in xa11y-js/src/app.rs) — the wrapper must NOT supply a default,
  // otherwise the native default becomes unreachable.
  await App.byName('Calc');
  assert.equal(NativeAppStub.__byNameLastArgs.name, 'Calc');
  assert.equal(NativeAppStub.__byNameLastArgs.options, undefined);
});

test('App.byPid forwards options unchanged to native', async () => {
  await App.byPid(4242, { timeout: 2500 });
  assert.deepEqual(NativeAppStub.__byPidLastArgs, {
    pid: 4242,
    options: { timeout: 2500 },
  });
});

test('App.byPid forwards undefined options when caller omits them', async () => {
  await App.byPid(7777);
  assert.equal(NativeAppStub.__byPidLastArgs.pid, 7777);
  assert.equal(NativeAppStub.__byPidLastArgs.options, undefined);
});
