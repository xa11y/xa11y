// Regression tests for issue #189: errors raised from the native layer must
// be catchable via `instanceof` against the documented public classes
// exported from `index.js`, and must not leak a `_native`-flavoured class
// name through `.name` / `.constructor.name`.
//
// The Python binding had a parallel bug where `_native.XA11yTimeoutError`
// surfaced in tracebacks for what the docs called `xa11y.TimeoutError`. The
// Node bindings re-throw via pure JS subclasses (`toTypedError` in
// `index.js`), so the risk is "did every error path actually get wrapped?"
// — these tests trigger each easily-reachable error path against the mock
// provider and assert the public surface.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const xa11y = require('../../index.js');
const {
  _makeTestLocator,
  InvalidActionDataError,
  InvalidSelectorError,
  PlatformError,
  SelectorNotMatchedError,
  TimeoutError,
  XA11yError,
} = xa11y;

// Every documented public error class. Kept in one place so the surface
// checks stay in sync when the public API grows.
const PUBLIC_ERROR_CLASSES = [
  'XA11yError',
  'PermissionDeniedError',
  'AccessibilityNotEnabledError',
  'SelectorNotMatchedError',
  'ActionNotSupportedError',
  'TimeoutError',
  'InvalidSelectorError',
  'InvalidActionDataError',
  'PlatformError',
];

test('every documented error class is exported and named publicly', () => {
  for (const name of PUBLIC_ERROR_CLASSES) {
    const Cls = xa11y[name];
    assert.equal(typeof Cls, 'function', `${name} is missing from the public export`);
    // An instance's `.name` is what shows up in stack traces / `toString()`;
    // a leak of an internal name here would mirror the Python `_native.X`
    // traceback problem in #189.
    assert.equal(new Cls('x').name, name, `${name}.prototype.name leaks an internal tag`);
    assert.ok(new Cls('x') instanceof XA11yError);
    assert.ok(new Cls('x') instanceof Error);
  }
});

test('SelectorNotMatchedError thrown from native is instanceof public class', async () => {
  await assert.rejects(
    _makeTestLocator().descendant('button[name="Nope"]').element(),
    (err) => {
      assert.ok(err instanceof SelectorNotMatchedError, 'instanceof SelectorNotMatchedError');
      assert.ok(err instanceof XA11yError, 'instanceof XA11yError');
      assert.equal(err.constructor.name, 'SelectorNotMatchedError');
      return true;
    },
  );
});

test('InvalidSelectorError thrown from native is instanceof public class', async () => {
  await assert.rejects(
    _makeTestLocator().descendant('[[[bad').elements(),
    (err) => {
      assert.ok(err instanceof InvalidSelectorError);
      assert.ok(err instanceof XA11yError);
      return true;
    },
  );
});

test('InvalidActionDataError thrown from native (Locator.nth(0)) is instanceof public class', async () => {
  // `nth(0)` is rejected at the binding boundary — see the matching guard in
  // `xa11y-python/src/lib.rs::Locator::nth`. Mirrors the Python regression
  // test for issue #189.
  assert.throws(
    () => _makeTestLocator().nth(0),
    (err) => {
      assert.ok(err instanceof InvalidActionDataError);
      assert.ok(err instanceof XA11yError);
      return true;
    },
  );
});

test('TimeoutError from waitDetached is instanceof public class', async () => {
  // `button` is always present in the mock tree, so wait_detached can never
  // succeed. `waitDetached` takes seconds — a 0.05s budget keeps the test fast.
  await assert.rejects(
    _makeTestLocator().descendant('button').waitDetached(0.05),
    (err) => {
      assert.ok(err instanceof TimeoutError, 'instanceof TimeoutError');
      assert.ok(err instanceof XA11yError, 'instanceof XA11yError');
      assert.equal(err.constructor.name, 'TimeoutError');
      return true;
    },
  );
});

test('PlatformError from mock subscribe() is instanceof public class', async () => {
  // The mock provider intentionally returns Error::Platform from subscribe()
  // (see `xa11y-core/src/mock.rs`), which exercises the PlatformError mapping.
  const app = await _makeTestLocator().element();
  await assert.rejects(
    Promise.resolve().then(() => app.subscribe()),
    (err) => {
      assert.ok(err instanceof PlatformError, 'instanceof PlatformError');
      assert.ok(err instanceof XA11yError, 'instanceof XA11yError');
      return true;
    },
  );
});

// ── Structured diagnosis (tenet 6) ──────────────────────────────────────────
//
// Timeouts and selector misses carry structured fields (`selector`,
// `condition`, `lastObserved`, `candidates`, `scope`, `elapsedMs`) parsed
// from the native payload, so a harness never needs to wrap a call in
// try/catch just to dump the tree. The same content is rendered into the
// message.

test('TimeoutError carries a structured diagnosis when the selector never matched', async () => {
  await assert.rejects(
    _makeTestLocator().descendant('button[name="Nope"]').waitVisible(0.3),
    (err) => {
      assert.ok(err instanceof TimeoutError);
      assert.equal(err.condition, 'visible');
      assert.equal(err.selector, 'application button[name="Nope"]');
      assert.equal(err.lastObserved, 'selector never matched');
      assert.ok(err.candidates.includes('button "Back"'), `candidates: ${err.candidates}`);
      assert.ok(err.scope.includes('TestApp'), `scope: ${err.scope}`);
      assert.ok(err.elapsedMs >= 300, `elapsedMs: ${err.elapsedMs}`);
      // The message renders the same content — no separator bytes leak in.
      assert.ok(err.message.includes('waiting for: visible'), err.message);
      assert.ok(!err.message.includes('\u001f'), 'separator must be stripped');
      return true;
    },
  );
});

test('SelectorNotMatchedError carries a structured diagnosis on element() miss', async () => {
  await assert.rejects(
    _makeTestLocator().descendant('button[name="Nope"]').element(),
    (err) => {
      assert.ok(err instanceof SelectorNotMatchedError);
      assert.equal(err.selector, 'application button[name="Nope"]');
      assert.ok(err.candidates.includes('button "Back"'), `candidates: ${err.candidates}`);
      assert.ok(err.scope.includes('TestApp'), `scope: ${err.scope}`);
      assert.equal(err.elapsedMs, null);
      return true;
    },
  );
});

test('diagnosis fields default to null/[] when a path produces no context', async () => {
  await assert.rejects(
    _makeTestLocator().descendant('[[[bad').elements(),
    (err) => {
      // Not a diagnosis-carrying class; just confirm the carrying classes
      // always expose the fields even before population.
      const t = new TimeoutError('x');
      assert.equal(t.selector, null);
      assert.equal(t.condition, null);
      assert.equal(t.lastObserved, null);
      assert.deepEqual(t.candidates, []);
      assert.equal(t.scope, null);
      assert.equal(t.elapsedMs, null);
      return true;
    },
  );
});
