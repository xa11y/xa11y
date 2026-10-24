// Module-level exports and smoke test for the loaded bindings.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { EventEmitter } = require('node:events');

const xa11y = require('../../index.js');
const {
  App,
  Element,
  Event,
  InputSim,
  Locator,
  Screenshot,
  Subscription,
  XA11yError,
  PermissionDeniedError,
  SelectorNotMatchedError,
  ActionNotSupportedError,
  TimeoutError,
  InvalidSelectorError,
  PlatformError,
  inputSim,
  locator,
  screenshot,
} = xa11y;

test('exports the public API surface', () => {
  assert.equal(typeof App, 'function');
  assert.equal(typeof Element, 'function');
  assert.equal(typeof Event, 'function');
  assert.equal(typeof InputSim, 'function');
  assert.equal(typeof Locator, 'function');
  assert.equal(typeof Screenshot, 'function');
  assert.equal(typeof Subscription, 'function');
  assert.equal(typeof inputSim, 'function');
  assert.equal(typeof locator, 'function');
  assert.equal(typeof screenshot, 'function');
});

test('every export is an ESM named export', async () => {
  // Node finds a CommonJS module's named exports by scanning its source, so
  // an export the scan misses fails `import { screenshot } from
  // '@crowecawcaw/xa11y'` with a SyntaxError and is reachable only through
  // the default import.
  const esm = await import('../../index.js');
  for (const name of Object.keys(xa11y)) {
    assert.equal(esm[name], xa11y[name], `import { ${name} } from '@crowecawcaw/xa11y'`);
  }
});

test('Subscription extends EventEmitter', () => {
  assert.ok(Subscription.prototype instanceof EventEmitter);
});

test('error classes form a proper hierarchy', () => {
  assert.ok(new PermissionDeniedError('x') instanceof XA11yError);
  assert.ok(new SelectorNotMatchedError('x') instanceof XA11yError);
  assert.ok(new ActionNotSupportedError('x') instanceof XA11yError);
  assert.ok(new TimeoutError('x') instanceof XA11yError);
  assert.ok(new InvalidSelectorError('x') instanceof XA11yError);
  assert.ok(new PlatformError('x') instanceof XA11yError);
  assert.ok(new XA11yError('x') instanceof Error);
});

test('error class names are set for debugging', () => {
  assert.equal(new PermissionDeniedError('x').name, 'PermissionDeniedError');
  assert.equal(new SelectorNotMatchedError('x').name, 'SelectorNotMatchedError');
  assert.equal(new TimeoutError('x').name, 'TimeoutError');
});
