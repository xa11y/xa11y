// Module-level exports and smoke test for the loaded bindings.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const xa11y = require('../../index.js');
const {
  App,
  Element,
  Event,
  EventType,
  Locator,
  Subscription,
  XA11yError,
  PermissionDeniedError,
  SelectorNotMatchedError,
  ActionNotSupportedError,
  TimeoutError,
  InvalidSelectorError,
  PlatformError,
  locator,
} = xa11y;

test('exports the public API surface', () => {
  assert.equal(typeof App, 'function');
  assert.equal(typeof Element, 'function');
  assert.equal(typeof Event, 'function');
  assert.equal(typeof Locator, 'function');
  assert.equal(typeof Subscription, 'function');
  assert.equal(typeof locator, 'function');
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

test('EventType lists all normalised event categories', () => {
  assert.equal(EventType.FocusChanged, 'focusChanged');
  assert.equal(EventType.ValueChanged, 'valueChanged');
  assert.equal(EventType.WindowOpened, 'windowOpened');
  assert.equal(EventType.TextChanged, 'textChanged');
  assert.ok(Object.isFrozen(EventType));
});
