'use strict';

// `typeText` refuses while `keyDown` holds a modifier, through the real binding.

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { _makeTestInput, InvalidActionDataError } = require('../../index.js');

test('typeText rejects while a modifier is held, and runs once it is released', async () => {
  const input = _makeTestInput();
  await input.keyDown('Shift');
  try {
    await assert.rejects(input.typeText('hello'), (error) => {
      assert.ok(error instanceof InvalidActionDataError);
      assert.match(error.message, /Shift held with key_down/);
      return true;
    });
  } finally {
    await input.keyUp('Shift');
  }
  await input.typeText('hello');
});

test('a held non-modifier and a chord do not block typeText', async () => {
  const input = _makeTestInput();
  await input.chord('a', ['Ctrl']);
  await input.keyDown('a');
  try {
    await input.typeText('hello');
  } finally {
    await input.keyUp('a');
  }
});
