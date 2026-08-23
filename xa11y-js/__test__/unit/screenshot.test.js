// `screenshot()`'s argument handling, and the shape of the annotation legend.
//
// None of this reaches a display: `screenshot()` needs a real capture backend,
// which no CI runner here has. What is testable is everything that happens
// before the first native call — the `element` / `region` exclusion, and the
// `Locator | string` union `annotate` accepts — plus the promise that an
// unannotated capture still answers `legend` / `omitted` / `truncated`.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const xa11y = require('../../index.js');
const { InvalidActionDataError, Screenshot, _makeTestLocator } = xa11y;

test('element and region together are rejected', () => {
  assert.throws(
    () => xa11y.screenshot({ element: {}, region: { x: 0, y: 0, width: 1, height: 1 } }),
    InvalidActionDataError,
  );
});

test('annotate must be an array', () => {
  for (const annotate of ['button', _makeTestLocator(), 7, {}]) {
    assert.throws(() => xa11y.screenshot({ annotate }), {
      name: 'InvalidActionDataError',
      message: /`annotate` must be an array/,
    });
  }
});

test('annotate rejects an entry that is neither a Locator nor a string', () => {
  for (const entry of [1, null, {}, [], true]) {
    assert.throws(() => xa11y.screenshot({ annotate: [entry] }), {
      name: 'InvalidActionDataError',
      message: /annotate\[0\] must be a Locator or a selector string/,
    });
  }
});

test('the rejection names the position of the bad entry', () => {
  assert.throws(() => xa11y.screenshot({ annotate: [_makeTestLocator(), 42] }), {
    message: /annotate\[1\] .* got number/,
  });
});

test('an omitted annotate option leaves the plain capture path alone', () => {
  // `undefined` and `null` both mean "not annotating"; neither may be read as
  // an empty group list, which would take the annotated path instead.
  for (const options of [undefined, {}, { annotate: undefined }, { annotate: null }]) {
    // The capture itself fails on a runner with no display; what matters is
    // that it fails *there* rather than in argument parsing.
    assert.doesNotThrow(() => {
      const result = xa11y.screenshot(options);
      assert.ok(result instanceof Promise);
      result.catch(() => {});
    });
  }
});

test('Screenshot declares the legend accessors', () => {
  // A consumer reads `shot.legend` without a version check, so the accessors
  // must exist on the prototype even when nothing was annotated.
  for (const name of ['legend', 'omitted', 'truncated']) {
    const descriptor = Object.getOwnPropertyDescriptor(Screenshot.prototype, name);
    assert.ok(descriptor, `Screenshot.prototype.${name} is missing`);
    assert.equal(typeof descriptor.get, 'function');
  }
});
