// `ShellSurface` behaviour against the Rust mock provider.
//
// The real statics (`ShellSurface.list()` / `.byKind()`) resolve the platform
// singleton provider, which no unit-test environment has — so the fixture
// lookups go through the `_makeTestShellSurface*` helpers, which call the same
// `ShellSurface::list_with` / `by_kind_with` core entry points with the shared
// mock. The mock's shell fixture is:
//
//   taskbar "Taskbar" (pid=4242)
//   ├── button "Show Hidden Icons"
//   └── button "Volume"
//   desktop "Desktop" (pid=4242)
//   └── list_item "Trash"
//
// Argument parsing is the one thing that must NOT need a provider, so the
// unknown-kind test drives the real `ShellSurface.byKind` static.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const xa11y = require('../../index.js');
const {
  ShellSurface,
  InvalidActionDataError,
  SelectorNotMatchedError,
  _makeTestShellSurfaces,
  _makeTestShellSurfaceByKind,
  _makeTestAmbiguousShellSurfaceByKind,
} = xa11y;

test('list() returns the fixture surfaces with kind, name and pid', () => {
  const surfaces = _makeTestShellSurfaces();
  assert.deepEqual(
    surfaces.map((s) => [s.kind, s.name, s.pid]),
    [
      ['taskbar', 'Taskbar', 4242],
      ['desktop', 'Desktop', 4242],
    ],
  );
  for (const surface of surfaces) {
    assert.ok(surface instanceof ShellSurface, 'a listed surface is a ShellSurface');
  }
});

test('the surface kind is stamped onto the root element', async () => {
  // The stamp is on the surface root only, and the raw map is where a
  // consumer reads it back — it is not a selector (a rooted locator emits
  // only descendants of its root, and a TreeNode carries no raw map).
  const taskbar = _makeTestShellSurfaceByKind('taskbar');
  assert.equal(taskbar.asElement().raw.shell_kind, 'taskbar');
  const node = await taskbar.tree(0);
  assert.equal(node.name, 'Taskbar');
});

test('byKind resolves a unique surface', () => {
  const desktop = _makeTestShellSurfaceByKind('desktop');
  assert.equal(desktop.kind, 'desktop');
  assert.equal(desktop.name, 'Desktop');
  assert.equal(desktop.pid, 4242);
});

test('locator() is rooted at the surface', async () => {
  const taskbar = _makeTestShellSurfaceByKind('taskbar');
  const chevron = await taskbar.locator("button[name='Show Hidden Icons']").element();
  assert.equal(chevron.role, 'button');
  assert.equal(chevron.name, 'Show Hidden Icons');

  // The app tree is not in scope: the surface root is its own subtree.
  await assert.rejects(
    taskbar.locator("button[name='Back']").element(),
    (err) => err instanceof SelectorNotMatchedError,
  );
});

test('children(), asElement() and dump() expose the surface subtree', async () => {
  const taskbar = _makeTestShellSurfaceByKind('taskbar');
  const children = await taskbar.children();
  assert.deepEqual(
    children.map((c) => c.name),
    ['Show Hidden Icons', 'Volume'],
  );
  assert.equal(taskbar.asElement().name, 'Taskbar');
  assert.match(await taskbar.dump(), /Show Hidden Icons/);

  const desktop = _makeTestShellSurfaceByKind('desktop');
  const items = await desktop.children();
  assert.deepEqual(
    items.map((c) => [c.role, c.name]),
    [['list_item', 'Trash']],
  );
});

test('a kind with no surface rejects with the surfaces that were present', () => {
  assert.throws(
    () => _makeTestShellSurfaceByKind('dock'),
    (err) => {
      assert.ok(err instanceof SelectorNotMatchedError, 'instanceof SelectorNotMatchedError');
      // Tenet 6: the structured diagnosis, not just the message prose.
      assert.equal(err.selector, 'shell_surface[kind=dock]');
      assert.equal(err.condition, 'a dock shell surface');
      assert.match(err.lastObserved, /no dock surface present/);
      assert.deepEqual(err.candidates, [
        'taskbar "Taskbar" (pid=4242)',
        'desktop "Desktop" (pid=4242)',
      ]);
      return true;
    },
  );
});

test('an ambiguous shell is refused with every candidate named', () => {
  assert.throws(
    () => _makeTestAmbiguousShellSurfaceByKind('taskbar'),
    (err) => {
      assert.ok(err instanceof SelectorNotMatchedError, 'instanceof SelectorNotMatchedError');
      assert.equal(err.selector, 'shell_surface[kind=taskbar]');
      assert.equal(err.condition, 'exactly one taskbar shell surface');
      assert.match(err.lastObserved, /2 taskbar surfaces are present/);
      assert.equal(err.candidates.length, 2);
      return true;
    },
  );
});

test('an unknown kind fails before the provider is touched', async () => {
  // The real static — no mock in sight. Parsing happens before
  // `xa11y::provider()` is resolved, so this is an argument error even on a
  // machine with no accessibility API at all.
  await assert.rejects(ShellSurface.byKind('bogus'), (err) => {
    assert.ok(err instanceof InvalidActionDataError, 'instanceof InvalidActionDataError');
    assert.match(err.message, /unknown shell surface kind: bogus/);
    // The message names the accepted spellings, so the fix needs no docs trip.
    for (const kind of [
      'menu_bar',
      'status_items',
      'taskbar',
      'panel',
      'dock',
      'desktop',
      'flyout',
      'unknown',
    ]) {
      assert.match(err.message, new RegExp(`'${kind}'`));
    }
    return true;
  });

  // Same refusal through the mock-backed lookup, which never gets as far as
  // enumerating the fixture.
  assert.throws(
    () => _makeTestShellSurfaceByKind('Taskbar'),
    (err) => err instanceof InvalidActionDataError,
  );
});

test('the exported class carries both static factories', () => {
  assert.equal(typeof ShellSurface.list, 'function');
  assert.equal(typeof ShellSurface.byKind, 'function');
});
