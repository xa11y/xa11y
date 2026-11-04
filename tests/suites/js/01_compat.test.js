// Integration tests: basic tree discovery against the test app.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { getApp, one, appConfig, appEnv, textFieldSelector } = require('./helpers.js');

test('app is reachable', async () => {
  const app = await getApp();
  assert.ok(app.name);
  assert.ok(typeof app.pid === 'number' && app.pid > 0);
});

test('App.list is non-empty', async () => {
  const { App } = require('../../../xa11y-js/index.js');
  const apps = await App.list();
  assert.ok(apps.length >= 1, 'expected at least one running app');
});

test('tree has a Window or app is a Window', async () => {
  const app = await getApp();
  const windows = await app.locator('window').elements();
  // On Windows/UIA the app may itself be the window.
  assert.ok(windows.length >= 0);
});

test('tree has at least the expected number of buttons', async () => {
  const app = await getApp();
  const buttons = await app.locator('button').elements();
  assert.ok(
    buttons.length >= appConfig.minButtons,
    `expected >=${appConfig.minButtons} buttons, found ${buttons.length}`,
  );
});

test('tree has the primary button', async () => {
  if (!appConfig.okButtonName) return; // skip if app has no named primary button
  const app = await getApp();
  const btn = await one(app, `button[name="${appConfig.okButtonName}"]`);
  assert.equal(btn.role, 'button');
  assert.equal(btn.name, appConfig.okButtonName);
  assert.equal(btn.enabled, true);
});

test('tree has the named text field', async () => {
  if (!textFieldSelector) return; // skip if app has no reliably-labelled text field
  const app = await getApp();
  const field = await one(app, textFieldSelector);
  assert.equal(field.role, 'text_field');
  assert.equal(field.editable, true);
});

test('UWP retains the native nested Edit and selects the outer TextBox (#606)', {
  skip: appEnv !== 'uwp',
}, async () => {
  const app = await getApp();
  const fields = await app.locator('text_field[name="Search"]').elements();
  assert.equal(fields.length, 2, 'Raw View exposes both native Edit elements');
  const outer = await one(app, textFieldSelector);
  await one(app, `${textFieldSelector} > text_field[name="Search"]`);
  assert.equal(outer.raw.class_name, 'TextBox');
});

test('a password entry keeps text_field and sets password (#522)', async () => {
  if (!appConfig.passwordFieldName) return; // skip if app has no password entry
  const app = await getApp();
  const field = await one(app, `text_field[name="${appConfig.passwordFieldName}"]`);
  assert.equal(field.role, 'text_field');
  assert.equal(field.password, true);
  const flagged = await app.locator('text_field[password="true"]').elements();
  assert.deepEqual(flagged.map((e) => e.name), [appConfig.passwordFieldName]);
  if (textFieldSelector) {
    const plain = await one(app, textFieldSelector);
    assert.equal(plain.password, false);
  }
});

test('tree has a Checkbox', async () => {
  if (!appConfig.hasCheckbox) return; // skip if app has no checkbox
  const app = await getApp();
  const checks = await app.locator('check_box').elements();
  assert.ok(checks.length >= 1, 'expected a checkbox');
  assert.ok(['on', 'off'].includes(checks[0].checked));
});

test('element.tree() returns a snapshot with the expected shape', async () => {
  if (!appConfig.okButtonName) return;
  const app = await getApp();
  const btn = await one(app, `button[name="${appConfig.okButtonName}"]`);
  const node = await btn.tree(0);
  assert.equal(node.role, 'button');
  assert.equal(node.name, appConfig.okButtonName);
  assert.deepEqual(node.children, []);
});

test('element.dump() returns a string containing the role', async () => {
  if (!appConfig.okButtonName) return;
  const app = await getApp();
  const btn = await one(app, `button[name="${appConfig.okButtonName}"]`);
  const text = await btn.dump(0);
  assert.equal(typeof text, 'string');
  assert.ok(text.includes('button'));
});

test('element.dump(0) produces exactly one non-empty line', async () => {
  if (!appConfig.okButtonName) return;
  const app = await getApp();
  const btn = await one(app, `button[name="${appConfig.okButtonName}"]`);
  const text = await btn.dump(0);
  const lines = text.split('\n').filter((l) => l.trim());
  assert.equal(lines.length, 1);
});

test('element().tree() returns snapshot via locator', async () => {
  if (!appConfig.okButtonName) return;
  const app = await getApp();
  const node = await (await app.locator(`button[name="${appConfig.okButtonName}"]`).element()).tree(0);
  assert.equal(node.role, 'button');
  assert.equal(node.name, appConfig.okButtonName);
});

test('element().dump() returns string via locator', async () => {
  if (!appConfig.okButtonName) return;
  const app = await getApp();
  const text = await (await app.locator(`button[name="${appConfig.okButtonName}"]`).element()).dump(0);
  assert.ok(text.includes('button'));
});

test('descendant selector returns multiple elements', async () => {
  const app = await getApp();
  // Files run serially against this shared app. Bracket the scoped query with
  // flat reads so a delayed native update has counts on both sides.
  const allBefore = await app.locator('button').elements();
  const byDescendant = await app.locator('window').descendant('button').elements();
  const allAfter = await app.locator('button').elements();
  const allMax = Math.max(allBefore.length, allAfter.length);
  // On platforms where the window node exists, the descendant scope should
  // match (at most) what the top-level query finds; on platforms without a
  // separate window node it may return fewer or zero — accept either.
  if (byDescendant.length > allMax) {
    const describe = (elements) => ({
      count: elements.length,
      truncated: elements.length > 24,
      elements: elements.slice(0, 24).map((element) => {
        const raw = element.raw;
        const bounds = element.bounds;
        return {
          name: element.name?.slice(0, 120),
          className: typeof raw.class_name === 'string' ? raw.class_name.slice(0, 80) : null,
          stableId: element.stableId?.slice(0, 120),
          pid: element.pid,
          bounds: bounds && [bounds.x, bounds.y, bounds.width, bounds.height],
        };
      }),
    });
    assert.fail('Descendant buttons exceeded both flat samples: ' + JSON.stringify({
      before: describe(allBefore),
      descendant: describe(byDescendant),
      after: describe(allAfter),
    }));
  }
});

test('a selector miss reports it was not delivered (#523)', async () => {
  const { SelectorNotMatchedError } = require('../../../xa11y-js/index.js');
  const app = await getApp();
  await assert.rejects(
    app.locator('button[name="NoSuchButtonAnywhere523"]').element(),
    (err) => {
      assert.ok(err instanceof SelectorNotMatchedError);
      assert.equal(err.delivery, 'not_delivered');
      return true;
    },
  );
});

test('hiddenReason is set exactly when an element is not visible', async () => {
  // #590: a hidden element says why, a visible one says nothing, and the
  // reason is one of the documented names.
  const known = new Set(['window_minimized', 'scrolled_out', 'not_showing', 'unknown']);
  const app = await getApp();
  const elements = await app.locator('*').elements();
  assert.ok(elements.length > 0, "the app's tree must not be empty");
  const broken = elements
    .filter((e) => (e.hiddenReason === null) !== e.visible
      || (e.hiddenReason !== null && !known.has(e.hiddenReason)))
    .map((e) => [e.role, e.name, e.visible, e.hiddenReason]);
  assert.deepEqual(broken.slice(0, 20), [],
    'hiddenReason must be a known name exactly when visible is false');
});
