'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { createAppDiscovery } = require('../../../tests/suites/js/app-discovery');

class SelectorNotMatchedError extends Error {}

function fixture(options = {}) {
  const calls = [];
  const app = { name: 'fixture', pid: 123 };
  const App = {
    async byPid(...args) { calls.push(['pid', ...args]); return app; },
    async byName(...args) { calls.push(['name', ...args]); return app; },
    async list() { throw new Error('unexpected enumeration'); },
  };
  return { app, calls, App, options: { App, SelectorNotMatchedError, names: ['wrong', 'fixture'], ...options } };
}

test('harness PID is authoritative and concurrent callers share one native lookup', async () => {
  const f = fixture({ pid: '123', name: 'another process' });
  const discovery = createAppDiscovery(f.options);
  assert.deepEqual(await Promise.all([discovery.getApp(), discovery.getApp()]), [f.app, f.app]);
  assert.deepEqual(f.calls, [['pid', 123, { timeout: 30_000 }]]);
  discovery.reset();
  await discovery.getApp();
  assert.deepEqual(f.calls, Array(2).fill(['pid', 123, { timeout: 30_000 }]));
});

test('resolved harness name has a single startup poll without unrelated name probes', async () => {
  const f = fixture({ name: 'resolved fixture' });
  await createAppDiscovery(f.options).getApp();
  assert.deepEqual(f.calls, [['name', 'resolved fixture', { timeout: 30_000 }]]);
});

test('authoritative PID errors keep their identity and diagnosis, never fall back by name', async () => {
  const f = fixture({ pid: '123' });
  const error = Object.assign(new Error('attach failed'), { diagnosis: { app: 123 } });
  f.App.byPid = async () => { throw error; };
  const discovery = createAppDiscovery(f.options);
  await assert.rejects(discovery.getApp(), (err) => err === error);
  f.App.byPid = async () => f.app;
  assert.equal(await discovery.getApp(), f.app, 'a failed lookup must not poison the cache');
  assert.deepEqual(f.calls, []);
});

test('invalid explicit PID is rejected rather than rediscovering a namesake', () => {
  for (const pid of ['', '0', '-1', '12oops', '4294967296']) {
    assert.throws(() => createAppDiscovery(fixture({ pid }).options), /Invalid XA11Y_TEST_APP_PID/);
  }
});

test('standalone lookup leaves no losing startup task occupying native workers', async () => {
  const f = fixture({ names: ['wrong', 'wrong', 'fixture', 'never-start-this-poll'] });
  let pending = 0;
  let maxPending = 0;
  f.App.byName = async (name, options) => {
    f.calls.push(['name', name, options]);
    pending += 1;
    maxPending = Math.max(maxPending, pending);
    // A native miss with a timeout remains alive after another promise wins.
    // Model that ownership using a timer; immediate probes leave no tail.
    await new Promise((resolve) => setTimeout(resolve, name === 'fixture' ? 1 : options.timeout));
    pending -= 1;
    if (name !== 'fixture') throw new SelectorNotMatchedError(name);
    return f.app;
  };
  assert.equal(await createAppDiscovery(f.options).getApp(), f.app);
  assert.equal(pending, 0);
  assert.equal(maxPending, 1);
  assert.deepEqual(f.calls, [['name', 'wrong', { timeout: 0 }], ['name', 'fixture', { timeout: 0 }]]);
});

test('standalone registration may take almost the full original 30-second budget', async () => {
  let elapsed = 0;
  const f = fixture({ now: () => elapsed, sleep: async (ms) => { elapsed += ms; } });
  f.App.byName = async (name, options) => {
    assert.equal(options.timeout, 0);
    if (elapsed < 29_900 || name !== 'fixture') throw new SelectorNotMatchedError(name);
    return f.app;
  };
  assert.equal(await createAppDiscovery(f.options).getApp(), f.app);
  assert.equal(elapsed, 29_900);
});

test('standalone hard provider error fails immediately without hiding it behind another candidate', async () => {
  const f = fixture();
  const error = new Error('accessibility permission denied');
  f.App.byName = async (name) => { f.calls.push(name); throw error; };
  await assert.rejects(createAppDiscovery(f.options).getApp(), (err) => err === error);
  assert.deepEqual(f.calls, ['wrong']);
});

test('standalone deadline preserves last miss and bounded running-app diagnosis', async () => {
  let elapsed = 0;
  const miss = new SelectorNotMatchedError('fixture never registered');
  const f = fixture({ now: () => elapsed, sleep: async (ms) => { elapsed += ms; } });
  f.App.byName = async () => { throw miss; };
  f.App.list = async () => [{ name: 'other', pid: 456 }];
  await assert.rejects(createAppDiscovery(f.options).getApp(), (err) => {
    assert.equal(err.cause, miss);
    assert.match(err.message, /30000ms.*wrong, fixture/);
    assert.match(err.message, /fixture never registered.*other.*456/);
    return true;
  });
  assert.equal(elapsed, 30_000);
});

test('failed diagnostic enumeration is visible alongside the original miss', async () => {
  const f = fixture({ timeout: 0 });
  const miss = new SelectorNotMatchedError('missing');
  f.App.byName = async () => { throw miss; };
  await assert.rejects(createAppDiscovery(f.options).getApp(), (err) => {
    assert.equal(err.cause, miss);
    assert.match(err.message, /failed to list: unexpected enumeration/);
    return true;
  });
});
