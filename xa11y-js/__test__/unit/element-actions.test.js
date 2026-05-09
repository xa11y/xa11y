// Element action invocation — verifies snapshot Element methods delegate to
// the stored provider and ElementData without going back through Locator.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { _makeTestActionProbe } = require('../../index.js');

async function elementFor(probe, selector) {
  return await probe.locator().descendant(selector).element();
}

async function assertElementAction({ selector, invoke, expected }) {
  const probe = _makeTestActionProbe();
  const el = await elementFor(probe, selector);

  await invoke(el);

  assert.deepEqual(probe.actions(), [expected]);
}

test('Element.press() delegates to provider press', async () => {
  await assertElementAction({
    selector: 'button[name="Back"]',
    invoke: (el) => el.press(),
    expected: [3, 'press', null],
  });
});

test('Element.focus() delegates to provider focus', async () => {
  await assertElementAction({
    selector: 'button[name="Back"]',
    invoke: (el) => el.focus(),
    expected: [3, 'focus', null],
  });
});

test('Element.toggle() delegates to provider toggle', async () => {
  await assertElementAction({
    selector: 'check_box[name="Agree"]',
    invoke: (el) => el.toggle(),
    expected: [7, 'toggle', null],
  });
});

test('Element.expand() delegates to provider expand', async () => {
  await assertElementAction({
    selector: 'list[name="Items"]',
    invoke: (el) => el.expand(),
    expected: [10, 'expand', null],
  });
});

test('Element.collapse() delegates to provider collapse', async () => {
  await assertElementAction({
    selector: 'list[name="Items"]',
    invoke: (el) => el.collapse(),
    expected: [10, 'collapse', null],
  });
});

test('Element.select() delegates to provider select', async () => {
  await assertElementAction({
    selector: 'list_item[name="Item 1"]',
    invoke: (el) => el.select(),
    expected: [11, 'select', null],
  });
});

test('Element.showMenu() delegates to provider show_menu', async () => {
  await assertElementAction({
    selector: 'button[name="Back"]',
    invoke: (el) => el.showMenu(),
    expected: [3, 'show_menu', null],
  });
});

test('Element.scrollIntoView() delegates to provider scroll_into_view', async () => {
  await assertElementAction({
    selector: 'button[name="Back"]',
    invoke: (el) => el.scrollIntoView(),
    expected: [3, 'scroll_into_view', null],
  });
});

test('Element.increment() delegates to provider increment', async () => {
  await assertElementAction({
    selector: 'slider[name="Volume"]',
    invoke: (el) => el.increment(),
    expected: [8, 'increment', null],
  });
});

test('Element.decrement() delegates to provider decrement', async () => {
  await assertElementAction({
    selector: 'slider[name="Volume"]',
    invoke: (el) => el.decrement(),
    expected: [8, 'decrement', null],
  });
});

test('Element.setValue() delegates value payload', async () => {
  await assertElementAction({
    selector: 'text_field[name="Search"]',
    invoke: (el) => el.setValue('world'),
    expected: [6, 'set_value', 'world'],
  });
});

test('Element.setNumericValue() delegates numeric payload', async () => {
  await assertElementAction({
    selector: 'slider[name="Volume"]',
    invoke: (el) => el.setNumericValue(42),
    expected: [8, 'set_numeric_value', '42'],
  });
});

test('Element.typeText() delegates text payload', async () => {
  await assertElementAction({
    selector: 'text_field[name="Search"]',
    invoke: (el) => el.typeText('abc'),
    expected: [6, 'type_text', 'abc'],
  });
});

test('Element.selectText() delegates text range payload', async () => {
  await assertElementAction({
    selector: 'text_field[name="Search"]',
    invoke: (el) => el.selectText(1, 3),
    expected: [6, 'set_text_selection', '1..3'],
  });
});

test('Element.performAction() delegates arbitrary action names', async () => {
  await assertElementAction({
    selector: 'button[name="Back"]',
    invoke: (el) => el.performAction('raise'),
    expected: [3, 'raise', null],
  });
});
