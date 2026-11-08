// The capability table (`capabilities()`): static per-platform answers, so
// these run without a desktop.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const { Capabilities, InvalidActionDataError, capabilities } = require('../../index.js');

const CURRENT = { linux: 'linux', darwin: 'macos', win32: 'windows' }[process.platform];

test('describes the running platform by default', () => {
  const caps = capabilities();
  assert.ok(caps instanceof Capabilities);
  assert.equal(caps.platform, CURRENT);
  if (CURRENT === 'linux') assert.equal(caps.osVersion, null);
});

test('every platform answers every feature, with a reason when not supported', () => {
  const names = Capabilities.names();
  assert.equal(new Set(names).size, names.length);
  for (const platform of ['linux', 'macos', 'windows']) {
    const caps = capabilities({ platform });
    assert.equal(caps.platform, platform);
    const entries = caps.entries();
    assert.deepEqual(
      entries.map((c) => c.name),
      names,
    );
    for (const c of entries) {
      assert.ok(['supported', 'partial', 'unsupported'].includes(c.support), c.name);
      if (c.support !== 'supported') assert.ok(c.note, `${platform} ${c.name}: no reason`);
    }
  }
});

test('answers differ by platform, spelled as in Python', () => {
  assert.equal(capabilities({ platform: 'linux' }).support('event:menu_opened'), 'unsupported');
  assert.equal(capabilities({ platform: 'macos' }).support('event:menu_opened'), 'supported');
  const win = capabilities({ platform: 'windows' });
  assert.equal(win.support('window:enter_fullscreen'), 'unsupported');
  assert.equal(win.get('action:blur').note, 'focuses the desktop root');
  assert.equal(win.note('event:focus_changed'), null);
  assert.equal(win.has('action:blur'), true);
  assert.equal(win.has('action:nope'), false);
});

test('unknown names, platforms and versions throw typed errors', () => {
  const caps = capabilities({ platform: 'macos' });
  assert.throws(
    () => caps.get('event:menu_open'),
    (e) => {
      assert.ok(e instanceof InvalidActionDataError);
      assert.match(e.message, /event:menu_opened/);
      return true;
    },
  );
  assert.throws(() => capabilities({ platform: 'darwin' }), InvalidActionDataError);
  assert.throws(
    () => capabilities({ platform: 'linux', osVersion: '6.1' }),
    /Linux has no single platform version/,
  );
  assert.throws(
    () => capabilities({ platform: 'windows', osVersion: 'eleven' }),
    InvalidActionDataError,
  );
});

test('an OS version is recorded', () => {
  const caps = capabilities({ platform: 'windows', osVersion: '10.0.19045' });
  assert.equal(caps.osVersion, '10.0.19045');
});
