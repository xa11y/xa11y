'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { performance } = require('node:perf_hooks');
const {
  _makeTestInput,
  _makeTestApp,
  InvalidActionDataError,
  SelectorNotMatchedError,
} = require('../../index.js');

async function back() {
  return _makeTestApp().locator('button[name="Back"]').element();
}

test('literal points expose plain structured checked geometry', async () => {
  await assert.rejects(_makeTestInput().click([500, 500]), (error) => {
    assert.ok(error instanceof InvalidActionDataError);
    assert.equal(error.delivery, 'not_delivered');
    assert.deepEqual(error.pointerTarget.point, { x: 500, y: 500 });
    assert.deepEqual(error.pointerTarget.checkedPoint, error.pointerTarget.point);
    assert.equal(error.pointerTarget.elementBounds, null);
    assert.equal(error.pointerTarget.anchor, null);
    assert.equal(error.pointerTarget.coordinateSpace, 'mock desktop pixels');
    assert.deepEqual(error.pointerTarget.displays[0], { x: 0, y: 0, width: 100, height: 100 });
    assert.equal(error.pointerTarget.omittedDisplays, 0);
    return true;
  });
});

test('element anchor survives lowering into an async task', async () => {
  const element = await back();
  await assert.rejects(_makeTestInput().click(element, { anchor: [3, 4], held: ['Shift'] }), (error) => {
    assert.deepEqual(error.pointerTarget.elementBounds, element.bounds);
    assert.deepEqual(error.pointerTarget.point, { x: element.bounds.x + 3, y: element.bounds.y + 4 });
    assert.deepEqual(error.pointerTarget.anchor, { offset: { dx: 3, dy: 4 } });
    assert.equal(error.delivery, 'not_delivered');
    return true;
  });
});

test('drag attributes an invalid end to that element', async () => {
  const element = await back();
  await assert.rejects(_makeTestInput().drag([5, 5], element, { held: ['Shift'] }), (error) => {
    assert.deepEqual(error.pointerTarget.elementBounds, element.bounds);
    assert.equal(error.pointerTarget.anchor, 'center');
    assert.equal(error.pointerTarget.point.x, element.bounds.x + Math.floor(element.bounds.width / 2));
    return true;
  });
});

test('all other point methods reject before dispatch', async () => {
  for (const method of ['doubleClick', 'rightClick', 'moveTo', 'scroll']) {
    const input = _makeTestInput();
    const outcome = method === 'scroll' ? input.scroll([500, 500], 0, 0) : input[method]([500, 500]);
    await assert.rejects(outcome, InvalidActionDataError);
  }
});

test('captured moves and releases bypass geometry while clicks stay strict', async () => {
  const input = _makeTestInput();
  await input.mouseDown();
  try {
    await input.moveTo([500, 500]);
    await assert.rejects(input.click([500, 500]), InvalidActionDataError);
  } finally {
    await input.mouseUp();
  }
  await assert.rejects(input.moveTo([500, 500]), InvalidActionDataError);
});

test('zero count is a noop and negative monitor coordinates remain valid', async () => {
  const input = _makeTestInput();
  await input.click([500, 500], { count: 0, held: ['Shift'] });
  await input.click([-50, -50]);
});

test('native geometry query runs after returning control to Node', async () => {
  const element = await back();
  const input = _makeTestInput(1000);
  const started = performance.now();
  const outcome = input.click(element).catch((error) => error);
  const synchronousMs = performance.now() - started;
  const error = await outcome;
  assert.ok(error instanceof InvalidActionDataError);
  // The native test query sleeps 1s. Parsing an Element must only copy its
  // geometry; a query on the JS thread would consume that whole second.
  assert.ok(synchronousMs < 500, `synchronous geometry query took ${synchronousMs}ms`);
});

test('off-display is invalid action data, not a selector miss', async () => {
  await assert.rejects(_makeTestInput().click([500, 500]), (error) => {
    assert.ok(error instanceof InvalidActionDataError);
    assert.ok(!(error instanceof SelectorNotMatchedError));
    assert.match(error.message, /pointer target \(500, 500\) is not on any display/);
    assert.equal(error.pointerTarget.displaySource, null);
    return true;
  });
});

test('other invalid action data carries no pointer target', () => {
  assert.throws(
    () => _makeTestApp().locator('button').nth(0),
    (error) => {
      assert.ok(error instanceof InvalidActionDataError);
      assert.equal(error.pointerTarget, null);
      return true;
    },
  );
});
