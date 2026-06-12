// Process-wide default timeout (issue #259): `setDefaultTimeout` /
// `getDefaultTimeout`, and their effect on auto-waiting actions and the
// `wait*` defaults. The env-var path (XA11Y_DEFAULT_TIMEOUT) is read once
// per process, so it is exercised in subprocesses.

'use strict';

const { test, afterEach } = require('node:test');
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');

const {
  _makeTestLocator,
  TimeoutError,
  getDefaultTimeout,
  setDefaultTimeout,
} = require('../../index.js');

const BUILTIN_DEFAULT = 5;

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

test('built-in default is 5 seconds', () => {
  assert.equal(getDefaultTimeout(), BUILTIN_DEFAULT);
});

test('set/get round trip', () => {
  setDefaultTimeout(12.5);
  assert.equal(getDefaultTimeout(), 12.5);
});

test('rejects negative and non-finite values, leaving the default unchanged', () => {
  assert.throws(() => setDefaultTimeout(-1));
  assert.throws(() => setDefaultTimeout(NaN));
  assert.throws(() => setDefaultTimeout(Infinity));
  assert.equal(getDefaultTimeout(), BUILTIN_DEFAULT);
});

test('auto-waiting actions use the global default', async () => {
  setDefaultTimeout(0.3);
  const start = Date.now();
  await assert.rejects(missing().press(), TimeoutError);
  assert.ok(
    Date.now() - start < FAST_CEILING_MS,
    'auto-wait must use the 0.3s global default, not the built-in 5s',
  );
});

test('wait* with no argument uses the global default', async () => {
  setDefaultTimeout(0.3);
  const start = Date.now();
  await assert.rejects(missing().waitVisible(), TimeoutError);
  assert.ok(Date.now() - start < FAST_CEILING_MS);
});

test('an explicit per-call timeout beats the global default', async () => {
  setDefaultTimeout(60);
  const start = Date.now();
  await assert.rejects(missing().waitVisible(0.2), TimeoutError);
  assert.ok(
    Date.now() - start < FAST_CEILING_MS,
    'explicit waitVisible(0.2) must win over the 60s global default',
  );
});

test('zero default keeps single-attempt semantics', async () => {
  setDefaultTimeout(0);
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
  await assert.rejects(missing().waitVisible(-1));
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
    'console.log(require(process.argv[1]).getDefaultTimeout())',
    '12.5',
  );
  assert.equal(out, '12.5');
});

test('setDefaultTimeout overrides the env var (subprocess)', () => {
  const out = runNode(
    'const x = require(process.argv[1]); x.setDefaultTimeout(2); console.log(x.getDefaultTimeout())',
    '12.5',
  );
  assert.equal(out, '2');
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
