// Process-wide default timeout (issue #259): `setDefaultTimeout` /
// `getDefaultTimeout`, and their effect on auto-waiting actions and the
// `wait*` defaults. JS timeouts are `{ timeout: ms }`; a bare number (the old
// seconds argument) must throw rather than be read as milliseconds. The env-var path (XA11Y_DEFAULT_TIMEOUT) is read once
// per process, so it is exercised in subprocesses.

'use strict';

const { test, afterEach } = require('node:test');
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');

const {
  _makeTestLocator,
  InvalidActionDataError,
  TimeoutError,
  getDefaultTimeout,
  setDefaultTimeout,
} = require('../../index.js');

const BUILTIN_DEFAULT = { timeout: 5000 };

// Ceiling for "must not have waited out a long timeout" assertions —
// generous for slow CI, far below the long timeouts the tests configure.
const FAST_CEILING_MS = 3000;

afterEach(() => {
  // The default is process-wide state; reset it after every test.
  setDefaultTimeout(BUILTIN_DEFAULT);
});

function missing() {
  return _makeTestLocator().descendant('button[name="DoesNotExist"]');
}

test('built-in default is 5000 ms', () => {
  assert.deepEqual(getDefaultTimeout(), BUILTIN_DEFAULT);
});

test('set/get round trip', () => {
  setDefaultTimeout({ timeout: 12500 });
  assert.deepEqual(getDefaultTimeout(), { timeout: 12500 });
  // The getter's shape is the setter's, so a saved default restores as-is.
  const saved = getDefaultTimeout();
  setDefaultTimeout({ timeout: 1 });
  setDefaultTimeout(saved);
  assert.deepEqual(getDefaultTimeout(), { timeout: 12500 });
});

test('a bare number throws instead of being read as milliseconds', () => {
  assert.throws(() => setDefaultTimeout(5), (err) => {
    assert.ok(err instanceof InvalidActionDataError, `got ${err}`);
    assert.match(err.message, /pass \{ timeout: ms \}/);
    return true;
  });
  assert.deepEqual(getDefaultTimeout(), BUILTIN_DEFAULT);
});

test('a missing timeout throws', () => {
  assert.throws(() => setDefaultTimeout({}), InvalidActionDataError);
  assert.deepEqual(getDefaultTimeout(), BUILTIN_DEFAULT);
});

test('rejects negative and non-finite values, leaving the default unchanged', () => {
  for (const timeout of [-1, NaN, Infinity, 0.5]) {
    assert.throws(() => setDefaultTimeout({ timeout }), InvalidActionDataError, `${timeout}`);
  }
  assert.deepEqual(getDefaultTimeout(), BUILTIN_DEFAULT);
});

test('auto-waiting actions use the global default', async () => {
  setDefaultTimeout({ timeout: 300 });
  const start = Date.now();
  await assert.rejects(missing().press(), TimeoutError);
  assert.ok(
    Date.now() - start < FAST_CEILING_MS,
    'auto-wait must use the 300 ms global default, not the built-in 5000 ms',
  );
});

test('wait* with no argument uses the global default', async () => {
  setDefaultTimeout({ timeout: 300 });
  const start = Date.now();
  await assert.rejects(missing().waitVisible(), TimeoutError);
  assert.ok(Date.now() - start < FAST_CEILING_MS);
});

test('an explicit per-call timeout beats the global default', async () => {
  setDefaultTimeout({ timeout: 60000 });
  const start = Date.now();
  await assert.rejects(missing().waitVisible({ timeout: 200 }), TimeoutError);
  assert.ok(
    Date.now() - start < FAST_CEILING_MS,
    'explicit waitVisible({ timeout: 200 }) must win over the 60 s global default',
  );
});

test('zero default keeps single-attempt semantics', async () => {
  setDefaultTimeout({ timeout: 0 });
  // One attempt still happens: an actionable element succeeds...
  await _makeTestLocator().descendant('button[name="Back"]').press();
  // ...and a miss fails immediately instead of polling.
  const start = Date.now();
  await assert.rejects(missing().press(), TimeoutError);
  assert.ok(Date.now() - start < FAST_CEILING_MS);
});

test('a negative explicit wait timeout rejects instead of crashing', async () => {
  // Regression: a negative timeout used to reach Duration::from_secs_f64 in
  // the wait task and panic; it must reject the promise.
  await assert.rejects(missing().waitVisible({ timeout: -1 }), InvalidActionDataError);
});

test('every wait* rejects a bare number before polling', async () => {
  setDefaultTimeout({ timeout: 60000 });
  const loc = _makeTestLocator().descendant('button[name="Back"]');
  for (const method of [
    'waitVisible',
    'waitAttached',
    'waitDetached',
    'waitEnabled',
    'waitHidden',
    'waitDisabled',
    'waitFocused',
    'waitUnfocused',
    'waitStable',
  ]) {
    const start = Date.now();
    await assert.rejects(loc[method](5), (err) => {
      assert.ok(err instanceof InvalidActionDataError, `${method}: got ${err}`);
      assert.match(err.message, new RegExp(`${method}: pass \\{ timeout: ms \\}`));
      return true;
    });
    assert.ok(Date.now() - start < FAST_CEILING_MS, `${method} must not poll`);
  }
});

test('a fractional millisecond timeout is refused', async () => {
  await assert.rejects(missing().waitVisible({ timeout: 0.5 }), InvalidActionDataError);
});

// ── Environment variable (read once per process → subprocess tests) ────────

const INDEX_JS = require.resolve('../../index.js');

function runNode(code, envValue) {
  const env = { ...process.env };
  delete env.XA11Y_DEFAULT_TIMEOUT;
  if (envValue !== undefined) env.XA11Y_DEFAULT_TIMEOUT = envValue;
  return execFileSync(process.execPath, ['-e', code, INDEX_JS], {
    env,
    stdio: 'pipe',
  })
    .toString()
    .trim();
}

test('XA11Y_DEFAULT_TIMEOUT sets the default (subprocess)', () => {
  const out = runNode(
    'console.log(require(process.argv[1]).getDefaultTimeout().timeout)',
    '12.5',
  );
  // The environment variable stays in seconds, shared with every binding.
  assert.equal(out, '12500');
});

test('setDefaultTimeout overrides the env var (subprocess)', () => {
  const out = runNode(
    'const x = require(process.argv[1]); x.setDefaultTimeout({ timeout: 2000 }); console.log(x.getDefaultTimeout().timeout)',
    '12.5',
  );
  assert.equal(out, '2000');
});

test('an invalid XA11Y_DEFAULT_TIMEOUT surfaces as an error (subprocess)', () => {
  assert.throws(
    () =>
      runNode(
        'require(process.argv[1]).getDefaultTimeout()',
        'not-a-number',
      ),
    /XA11Y_DEFAULT_TIMEOUT/,
  );
});
