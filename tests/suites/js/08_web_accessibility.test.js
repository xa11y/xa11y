// App.enableWebAccessibility against the real Electron app (issue #511).
//
// The deep check (a second Electron started with its web tree off, a button
// unreachable before the call and reachable after it) lives in
// tests/suites/python/test_web_accessibility.py; this file covers the JS
// boundary: the async task, its options object, and the typed refusal.
//
// The suite's Electron is launched with its web tree already on, so on macOS
// the request is taken and the content is there. Linux and Windows have no
// such request and refuse with guidance.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { getApp, appEnv, ActionNotSupportedError } = require('./helpers.js');

const electronOnly = appEnv !== 'electron'
  ? 'enableWebAccessibility is exercised against the Electron app'
  : false;

test('enableWebAccessibility resolves on macOS', {
  skip: electronOnly || (process.platform !== 'darwin' && 'the request exists only on macOS'),
}, async () => {
  const app = await getApp();
  await app.enableWebAccessibility({ timeout: 10000 });
  // The options object is optional.
  await app.enableWebAccessibility();
  const ok = await app.locator('button[name="OK"]').element();
  assert.equal(ok.name, 'OK');
});

test('enableWebAccessibility is refused with guidance off macOS', {
  skip: electronOnly || (process.platform === 'darwin' && 'macOS takes the request'),
}, async () => {
  const app = await getApp();
  await assert.rejects(app.enableWebAccessibility({ timeout: 0 }), (err) => {
    assert.ok(err instanceof ActionNotSupportedError, `got ${err && err.constructor.name}: ${err}`);
    if (process.platform === 'linux') {
      assert.match(err.message, /--force-renderer-accessibility/);
      assert.match(err.message, /ACCESSIBILITY_ENABLED=1/);
    } else {
      assert.match(err.message, /UI Automation/);
    }
    return true;
  });
});
