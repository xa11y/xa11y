'use strict';

// App.enableWebAccessibility reaches the core and maps its errors. The mock
// provider has no web content switch, so it answers with the core's default
// refusal; what the switch does on a real Chromium app is covered by the
// integration suites.

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { _makeTestApp, ActionNotSupportedError } = require('../../index.js');

test('the mock provider refuses with ActionNotSupportedError', async () => {
  const app = _makeTestApp(false);
  await assert.rejects(app.enableWebAccessibility({ timeout: 0 }), (err) => {
    assert.ok(err instanceof ActionNotSupportedError, `${err}`);
    assert.match(err.message, /enable_web_accessibility/);
    return true;
  });
});

test('options are optional', async () => {
  const app = _makeTestApp(false);
  await assert.rejects(app.enableWebAccessibility(), ActionNotSupportedError);
});
