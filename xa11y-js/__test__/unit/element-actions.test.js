// Element action invocation — verifies that each action method on the JS
// Element binding dispatches into the core Element and lands the right
// entry in the mock provider's action log.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { _makeTestActionProbe, InvalidActionDataError } = require('../../index.js');

async function findDescendant(root, predicate) {
  // Depth-first walk of the mock tree until predicate is satisfied.
  const queue = [root];
  while (queue.length > 0) {
    const el = queue.shift();
    if (predicate(el)) return el;
    queue.push(...(await el.children()));
  }
  return null;
}

async function probeWith(predicate) {
  const probe = _makeTestActionProbe();
  const app = await probe.locator().element();
  const el = await findDescendant(app, predicate);
  assert.ok(el, 'fixture element not found in mock tree');
  return { probe, el };
}

function lastAction(probe) {
  const log = probe.actions();
  return log[log.length - 1];
}

// ── Nullary actions ────────────────────────────────────────────────────

test('press() records on the captured element', async () => {
  const { probe, el } = await probeWith((e) => e.name === 'Back');
  await el.press();
  const [, action, data] = lastAction(probe);
  assert.equal(action, 'press');
  assert.equal(data, null);
});

test('focus() records', async () => {
  const { probe, el } = await probeWith((e) => e.name === 'Back');
  await el.focus();
  assert.equal(lastAction(probe)[1], 'focus');
});

test('blur() records', async () => {
  const { probe, el } = await probeWith((e) => e.name === 'Back');
  await el.blur();
  assert.equal(lastAction(probe)[1], 'blur');
});

test('toggle() records on a checkbox', async () => {
  const { probe, el } = await probeWith((e) => e.role === 'check_box');
  await el.toggle();
  assert.equal(lastAction(probe)[1], 'toggle');
});

test('expand() records on the list', async () => {
  const { probe, el } = await probeWith((e) => e.role === 'list');
  await el.expand();
  assert.equal(lastAction(probe)[1], 'expand');
});

test('collapse() records on the list', async () => {
  const { probe, el } = await probeWith((e) => e.role === 'list');
  await el.collapse();
  assert.equal(lastAction(probe)[1], 'collapse');
});

test('select() records on a list item', async () => {
  const { probe, el } = await probeWith((e) => e.name === 'Item 2');
  await el.select();
  assert.equal(lastAction(probe)[1], 'select');
});

test('showMenu() records', async () => {
  const { probe, el } = await probeWith((e) => e.name === 'Back');
  await el.showMenu();
  assert.equal(lastAction(probe)[1], 'show_menu');
});

test('scrollIntoView() records', async () => {
  const { probe, el } = await probeWith((e) => e.name === 'Back');
  await el.scrollIntoView();
  assert.equal(lastAction(probe)[1], 'scroll_into_view');
});

test('increment() records on the slider', async () => {
  const { probe, el } = await probeWith((e) => e.role === 'slider');
  await el.increment();
  assert.equal(lastAction(probe)[1], 'increment');
});

test('decrement() records on the slider', async () => {
  const { probe, el } = await probeWith((e) => e.role === 'slider');
  await el.decrement();
  assert.equal(lastAction(probe)[1], 'decrement');
});

// ── Actions with payloads ──────────────────────────────────────────────

test('setValue() forwards the value arg', async () => {
  const { probe, el } = await probeWith((e) => e.role === 'text_field');
  await el.setValue('world');
  const [, action, data] = lastAction(probe);
  assert.equal(action, 'set_value');
  assert.equal(data, 'world');
});

test('setNumericValue() forwards a finite value', async () => {
  const { probe, el } = await probeWith((e) => e.role === 'slider');
  await el.setNumericValue(42);
  const [, action, data] = lastAction(probe);
  assert.equal(action, 'set_numeric_value');
  assert.equal(data, '42');
});

test('typeText() forwards the text arg', async () => {
  const { probe, el } = await probeWith((e) => e.role === 'text_field');
  await el.typeText('abc');
  const [, action, data] = lastAction(probe);
  assert.equal(action, 'type_text');
  assert.equal(data, 'abc');
});

test('selectText() forwards the range', async () => {
  const { probe, el } = await probeWith((e) => e.role === 'text_field');
  await el.selectText(1, 4);
  const [, action, data] = lastAction(probe);
  assert.equal(action, 'set_text_selection');
  assert.equal(data, '1..4');
});

test('performAction() forwards an arbitrary action name', async () => {
  const { probe, el } = await probeWith((e) => e.name === 'Back');
  await el.performAction('press');
  // perform_action records under the action name itself, not "perform_action".
  assert.equal(lastAction(probe)[1], 'press');
});

// ── Validation paths ───────────────────────────────────────────────────

test('setNumericValue(NaN) rejects with InvalidActionDataError', async () => {
  const { probe, el } = await probeWith((e) => e.role === 'slider');
  await assert.rejects(() => el.setNumericValue(NaN), InvalidActionDataError);
  // Ensure no provider call landed despite the rejection.
  assert.equal(probe.actions().length, 0);
});

test('setNumericValue(Infinity) rejects with InvalidActionDataError', async () => {
  const { probe, el } = await probeWith((e) => e.role === 'slider');
  await assert.rejects(
    () => el.setNumericValue(Number.POSITIVE_INFINITY),
    InvalidActionDataError,
  );
  assert.equal(probe.actions().length, 0);
});

test('selectText() with start > end rejects with InvalidActionDataError', async () => {
  const { probe, el } = await probeWith((e) => e.role === 'text_field');
  await assert.rejects(() => el.selectText(5, 2), InvalidActionDataError);
  assert.equal(probe.actions().length, 0);
});

// ── Snapshot semantics ─────────────────────────────────────────────────

test('actions act on the captured snapshot — not a fresh resolve', async () => {
  // Distinct elements with the same role should each route to their own
  // handle in the action log, proving the binding uses the snapshot's
  // identity rather than re-querying the tree.
  const probe = _makeTestActionProbe();
  const root = await probe.locator().element();
  const item1 = await findDescendant(root, (e) => e.name === 'Item 1');
  const item2 = await findDescendant(root, (e) => e.name === 'Item 2');
  assert.ok(item1 && item2);

  await item1.select();
  await item2.select();

  const log = probe.actions();
  assert.equal(log.length, 2);
  assert.equal(log[0][1], 'select');
  assert.equal(log[1][1], 'select');
  assert.notEqual(log[0][0], log[1][0], 'each item should record under its own handle');
});
