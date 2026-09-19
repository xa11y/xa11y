'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { _makeTestApp } = require('../../index.js');

test('every App inspection method includes all process registrations', async () => {
  const app = _makeTestApp(true);
  const expected = ['Main Window', 'Second Window'];
  assert.deepEqual((await app.windows()).map(window => window.name), expected);
  assert.deepEqual((await app.children()).map(child => child.name), expected);
  assert.deepEqual((await app.locator('window').elements()).map(element => element.name), expected);
  assert.deepEqual((await app.tree(1)).children.map(child => child.name), expected);
  assert.match(await app.dump(1), /window "Second Window"/);
  assert.equal((await app.locator("window:nth(2)").element()).name, "Second Window");
  assert.equal(await app.locator("window:nth(1)").count(), 1);
  assert.deepEqual((await app.tree(0)).children, []);
  assert.equal((await app.dump(0)).trim().split('\n').length, 1);
  assert.equal((await app.asElement().children()).length, 1);
});
