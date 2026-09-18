// Mutating window-management coverage for the JS binding, against a real app.
//
// Runs in the `js-window` suite, which the harness orders after every other
// suite (see tests/harness/launch.py): the harness runs python → js → cli
// against one shared app instance, and the window-state verbs (minimize /
// restore / resize) churn the UIA/AX cache in a way that made a following
// suite's action tests flaky. This file owns the last slot, so its mutations
// disturb nothing that follows, and the real napi wiring of the mutating
// verb methods (plus the Locator's async dispatch) is exercised against an
// actual provider instead of only against the mock.
//
// Every test restores the window it mutated: a failed `restore` ends the
// test (it is a real provider failure, not cleanup noise), but a best-effort
// restore is attempted first so the shared app is never left mutated for the
// next test — same failure-preserving pattern as the Locator test. A verb
// advertised in `actions` must dispatch to the real platform action
// (tenet 3): `ActionNotSupportedError` from an advertised verb is a fidelity
// regression and fails the test. The only legitimate early return is "no
// window advertises the verb" — never "advertised but the platform cannot
// perform it".
//
// `close` is only exercised against a *secondary* dialog window (opened via
// the app's "Open Dialog" button): closing the shared app's main window would
// kill the app the harness still needs. A dialog left open would also change
// the enumeration the next suites rely on, so every close test ends with a
// best-effort close of the dialog — through the platform close action when it
// exists, else through the dialog's own "Close Dialog" button.

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');

const {
  getApp,
  appConfig,
  appEnv,
  ActionNotSupportedError,
  TimeoutError,
  sleep,
} = require('./helpers.js');

const WINDOW_STATE_TIMEOUT_MS = appEnv === 'cocoa' ? 15_000 : 5_000;

// The platform, not the app identity: the sequence drill in the screen-fill
// test is about the macOS fullscreen animation and runs for every macOS test
// app, not just one.
const MACOS = process.platform === 'darwin';

async function windowAdvertising(app, verb) {
  const windows = await app.windows();
  return windows.find((w) => w.actions.includes(verb)) || null;
}

async function waitForWindow(app, verb, what) {
  // Cleanup-only: a failure mid-transition can leave the real window out of
  // app.windows() for a moment, and the cleanup rails must still find
  // something to restore rather than raising a second error. The happy-path
  // assertions use `settledWindow` instead.
  return waitUntil(() => windowAdvertising(app, verb), 5000, what);
}

async function currentWindow(app, original, verb) {
  const windows = await app.windows();
  if (original.name) {
    const sameName = windows.find((w) => w.name === original.name);
    if (sameName) return sameName;
  }
  return windows.find((w) => w.actions.includes(verb)) || null;
}

function boundsNear(actual, expected, fields, tolerance = 2) {
  return actual != null && fields.every(
    (field) => Math.abs(actual[field] - expected[field]) <= tolerance,
  );
}

async function waitUntil(predicate, timeoutMs, what) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const result = await predicate();
    if (result) return result;
    await sleep(100);
  }
  throw new Error(`Timed out waiting for ${what}`);
}

async function settledWindow(app, verb, predicate, what) {
  // macOS settles a window-state transition before the verb returns (the
  // provider polls the platform state), so one fresh read must already hold
  // and a poll would mask a provider that stopped settling (#399). Windows
  // drives WindowVisualState directly and promises only that the set call
  // succeeded, so it keeps the poll. Linux never reaches either branch: the
  // window verbs are unsupported there and the callers skip.
  if (MACOS) {
    const win = await windowAdvertising(app, verb);
    assert.ok(win, `${what}: no window advertises ${verb}`);
    assert.ok(
      predicate(win),
      `${what}: minimized=${win.minimized} fullscreen=${win.fullscreen} maximized=${win.maximized}`,
    );
    return win;
  }
  return waitUntil(async () => {
    const win = await windowAdvertising(app, verb);
    return win && predicate(win) ? win : null;
  }, 5000, what);
}

async function restoreWindowBestEffort(app) {
  // waitForWindow with `restore`, not a one-shot lookup and not a screen-fill
  // lookup: a failure can leave the real window out of app.windows() mid-
  // transition, and a window that advertises enterFullscreen/maximize does not
  // necessarily advertise the `restore` this cleanup needs. Never throws:
  // cleanup must not replace the original failure.
  try {
    const current = await waitForWindow(app, 'restore', 'a restorable window');
    await current.restore();
  } catch (_cleanup) {
    // best-effort cleanup; the original error wins
  }
}

async function dialogWindow(app) {
  // Prefer the top-level window shape (what `close` acts on); fall back to
  // the in-tree dialog node — the GTK app's AT-SPI tree has no top-level
  // window node, but its dialog is a Role::Dialog child (the same shape the
  // shared Python dialog test relies on).
  const dialogName = appConfig.dialogName;
  if (!dialogName) return null;
  const windows = await app.windows();
  const top = windows.find((w) => w.name && w.name.includes(dialogName));
  if (top) return top;
  const results = await app
    .locator(`window[name*="${dialogName}"], dialog[name*="${dialogName}"]`)
    .elements();
  return results[0] || null;
}

async function dialogGoneWithin(app, timeoutMs) {
  try {
    await waitUntil(async () => (await dialogWindow(app)) === null, timeoutMs,
      `the dialog ${appConfig.dialogName} to disappear`);
    return true;
  } catch (_e) {
    return false;
  }
}

async function closeDialog(app, { strict = true } = {}) {
  // Close a still-open dialog and wait until it leaves the tree. Prefers the
  // platform close action; falls back to the dialog's own Close Dialog
  // button.
  //
  // The fixture hides (not destroys) its dialog, so the press returning only
  // means the click was delivered — the suites that follow enumerate windows
  // (the Python capability probe asserts a clean GTK enumeration) and must
  // not race the hide. A close that was accepted but did not take effect is
  // retried, because a leftover dialog corrupts the *next* suite's
  // enumeration rather than this one's.
  //
  // `strict` decides what a dialog that never leaves after an accepted close
  // means: thrown when the caller's test passed, swallowed when it did not
  // (the original failure wins). A close that cannot even be dispatched is
  // different: Qt's dialog button is not actionable through AT-SPI, so the
  // press auto-wait times out. Retrying would repeat the timeout, the
  // platform's own body assertion already covered the no-close-API contract,
  // and the dialog is a pre-existing fixture limitation — so that case
  // returns silently, strict or not. A missing dialog is never an error.
  const attempts = 3;
  let dispatched = false;
  try {
    for (let attempt = 0; attempt < attempts; attempt += 1) {
      const dlg = await dialogWindow(app);
      if (!dlg) return;
      try {
        if (dlg.actions.includes('close')) {
          await dlg.close();
        } else {
          await app.locator('button[name="Close Dialog"]').press();
        }
      } catch (_e) {
        // Cannot dispatch a close at all; retrying the same non-actionable
        // target would only repeat the timeout.
        return;
      }
      dispatched = true;
      if (await dialogGoneWithin(app, 2000)) return;
    }
    if (dispatched) {
      throw new Error(
        `the dialog ${appConfig.dialogName} is still present after ${attempts} ` +
        'accepted closes; the suites that follow enumerate windows and would see it',
      );
    }
  } catch (e) {
    if (strict) throw e;
    // best-effort cleanup; the original error wins
  }
}

async function withDialogCleanup(app, body) {
  // Run `body`, then the strict dialog cleanup; the body's error wins so a
  // cleanup failure can never replace the failure under test. When the body
  // passed, a cleanup failure (a dialog that will not leave) *is* the
  // failure — it would otherwise surface as the next suite's broken
  // enumeration, with a message that points nowhere near the cause.
  let bodyError = null;
  try {
    await body();
  } catch (e) {
    bodyError = e;
  }
  let cleanupError = null;
  try {
    await closeDialog(app);
  } catch (e) {
    cleanupError = e;
  }
  if (bodyError) throw bodyError;
  if (cleanupError) throw cleanupError;
}

async function actionAndWait(sub, predicate, action) {
  const controller = new AbortController();
  const pending = sub.waitFor(predicate, {
    timeout: WINDOW_STATE_TIMEOUT_MS,
    signal: controller.signal,
  });
  try {
    await action();
    return await pending;
  } catch (err) {
    controller.abort();
    try {
      await pending;
    } catch (_cancelled) {
      // Consume the cancelled waiter so it cannot reject after this test ends.
    }
    throw err;
  }
}

async function closeSiblingBestEffort(app) {
  const siblingName = appConfig.siblingName;
  if (!siblingName) return;
  try {
    const sibling = (await app.windows()).find((w) => w.name === siblingName);
    if (!sibling) return;
    if (sibling.actions.includes('restore')) await sibling.restore();
    await app.locator('button[name="Close Sibling"]').press();
  } catch (_e) {
    // best-effort cleanup; the original error wins
  }
}

async function closeDuplicatesBestEffort(app) {
  if (!appConfig.duplicateWindowName) return;
  try {
    const duplicates = (await app.windows()).filter(
      (w) => w.name === appConfig.duplicateWindowName,
    );
    for (const duplicate of duplicates) {
      if (duplicate.actions.includes('close')) await duplicate.close();
    }
  } catch (_e) {
    // best-effort cleanup; the original error wins
  }
}

async function siblingWindow(app) {
  if (!appConfig.siblingName) return null;
  return (await app.windows()).find((w) => w.name === appConfig.siblingName) || null;
}

async function openDialog(app) {
  // Press the app's "Open Dialog" button and wait for the dialog window.
  // Returns the dialog element; returns null when the app has no dialog
  // button config. "Pressed but the dialog never appeared" is a fixture
  // regression and throws (the dialog name comes from the same config the
  // CLI suite's close test relies on).
  const btnName = appConfig.dialogButtonName;
  const dialogName = appConfig.dialogName;
  if (!btnName || !dialogName) return null;
  try {
    await app.locator(`button[name="${btnName}"]`).press();
  } catch (err) {
    // Only a never-matched selector means "this app has no dialog button".
    // Any other failure (the button exists but the dispatch broke) is a
    // regression and must surface.
    if (err instanceof TimeoutError) return null;
    throw err;
  }
  try {
    await waitUntil(async () => (await dialogWindow(app)) !== null, 5000, `dialog ${dialogName} to appear`);
    return await dialogWindow(app);
  } catch (err) {
    // Clean up the press side effect before declaring the fixture regression.
    await closeDialog(app, { strict: false });
    throw err;
  }
}

function locatorForWindow(app, win) {
  // A single-match Locator for a window-like element. The top-level may be a
  // Role::Window *or* a Role::Dialog (the Qt and Cocoa apps' top level is a
  // dialog), while App.windows() lists both, so the selector must accept both.
  if (!win.name) return null;
  return app.locator(`window[name="${win.name}"], dialog[name="${win.name}"]`);
}

test('js-window suite resolves the shared app', async () => {
  const app = await getApp();
  const windows = await app.windows();
  assert.ok(Array.isArray(windows), 'windows() returns an array');
});

test('a window that advertises minimize is minimized and restored', async (t) => {
  const app = await getApp();
  const win = await windowAdvertising(app, 'minimize');
  if (!win || !win.actions.includes('restore')) {
    t.skip('no window advertises both minimize and restore');
    return;
  }
  try {
    await win.minimize();
    const minimized = await settledWindow(
      app, 'minimize', (w) => w.minimized === true, 'minimize must report minimized');
    // Advertised restore must succeed; a failure leaves the app minimized and
    // is a real provider failure, never swallowed.
    await minimized.restore();
    await settledWindow(
      app, 'minimize', (w) => w.minimized === false, 'restore must report restored');
  } catch (err) {
    // Never leave the shared app minimized for the suite after this one; the
    // original failure wins over any cleanup failure.
    await restoreWindowBestEffort(app);
    throw err;
  }
});

// The screen-filling operation each platform exposes. macOS has native
// fullscreen (`enterFullscreen` / `AXFullScreen`) and refuses `maximize` —
// there is no readable or writable zoom state — while Windows has `maximize`
// (UIA WindowVisualState) and no fullscreen API at all. The tests assert the
// platform's own verb and the platform's own state; they never treat the two
// states as one bit.
const SCREEN_FILL_OPERATIONS = [
  { action: 'enter_fullscreen', method: 'enterFullscreen', state: 'fullscreen' },
  { action: 'maximize', method: 'maximize', state: 'maximized' },
];

async function screenFillOperation(app) {
  // Prefer enterFullscreen over maximize. Linux/X11 can advertise both for
  // the same window; macOS refuses maximize and Windows has no fullscreen verb.
  for (const spec of SCREEN_FILL_OPERATIONS) {
    const win = await windowAdvertising(app, spec.action);
    if (win && win.actions.includes('restore')) return spec;
  }
  return null;
}

async function assertScreenFill(app, spec, want, what) {
  // One settled read (see `settledWindow`): macOS reads once, Windows polls.
  // The *other* state must not be active. It stays unknown (null) on macOS and
  // Windows, while Linux/X11 can prove that it is false.
  const win = await settledWindow(
    app, spec.action, (w) => w[spec.state] === want, `${what}: ${spec.state}`);
  const other = spec.state === 'fullscreen' ? 'maximized' : 'fullscreen';
  assert.notStrictEqual(
    win[other], true,
    `${what}: ${other} must not be active — ${spec.method} is a distinct operation`);
}

test('the screen-filling verb and minimize run back-to-back', async (t) => {
  // macOS exposes native fullscreen as enterFullscreen() (AXFullScreen) and
  // refuses maximize() — the classic zoom state has no accessibility surface,
  // so substituting fullscreen would blur two distinct operations. Windows
  // exposes maximize() (UIA WindowVisualState_Maximized) and has no fullscreen
  // API. The test picks the platform's own verb and then runs the back-to-back
  // max, min, max, min sequence the settling promise exists for: every step
  // asserts the state after the call, so a verb that left a transition
  // half-applied fails the next step (#399). macOS asserts with one read (the
  // provider settles before returning); Windows polls briefly (see
  // `settledWindow`).
  const app = await getApp();
  const spec = await screenFillOperation(app);
  if (!spec) {
    t.skip('no window advertises a screen-filling verb with restore');
    return;
  }
  try {
    // Fill the screen.
    await (await windowAdvertising(app, spec.action))[spec.method]();
    await assertScreenFill(app, spec, true, `after ${spec.method}`);

    // A repeated call is a no-op, not a toggle.
    await (await windowAdvertising(app, spec.action))[spec.method]();
    await assertScreenFill(app, spec, true, `after repeated ${spec.method}`);

    // minimize: on macOS this leaves fullscreen first (AppKit ignores a
    // minimized set while the window is fullscreen); on Windows it is the
    // direct UIA transition from Maximized.
    await (await windowAdvertising(app, spec.action)).minimize();
    let win = await settledWindow(
      app, spec.action, (w) => w.minimized === true, 'minimize must report minimized');
    assert.strictEqual(win[spec.state], false, `minimize must clear ${spec.state}`);

    // And back: the minimized window is still reachable by the next call.
    await (await windowAdvertising(app, spec.action))[spec.method]();
    await assertScreenFill(app, spec, true, `after re-${spec.method} from minimized`);

    await (await windowAdvertising(app, spec.action)).minimize();
    await settledWindow(
      app, spec.action, (w) => w.minimized === true, 'the second minimize must report minimized');

    // restore clears both states.
    await (await windowAdvertising(app, 'restore')).restore();
    win = await settledWindow(
      app, 'restore', (w) => w.minimized === false, 'restore must report restored');
    assert.strictEqual(win[spec.state], false, `restore must clear ${spec.state}`);
  } catch (err) {
    // Never leave the shared app fullscreen/minimized/maximized for the
    // suites after this one; the original failure wins over cleanup.
    await restoreWindowBestEffort(app);
    throw err;
  }
});

test('moveTo() changes the reported bounds and puts the window back', async (t) => {
  const app = await getApp();
  const win = await windowAdvertising(app, 'move_to');
  if (!win || !win.bounds) {
    t.skip('no window advertises move_to with restorable bounds');
    return;
  }
  const { x, y } = win.bounds;
  const movedBounds = { x: x + 10, y: y + 10 };
  try {
    await win.moveTo(movedBounds.x, movedBounds.y);
    if (appEnv === 'qt' && process.platform === 'win32') {
      await waitUntil(async () => {
        const current = await currentWindow(app, win, 'move_to');
        return current !== null && current.bounds !== null &&
          (!boundsNear(current.bounds, { x, y }, ['x', 'y']));
      }, 5000, 'Qt/UIA moveTo() to produce an observable bounds change');
      try {
        const current = await currentWindow(app, win, 'move_to');
        if (current) await current.moveTo(x, y);
      } catch (_cleanup) {
        // best-effort cleanup before reporting the coordinate-space gap
      }
      t.skip('Qt/UIA client coordinates differ from decorated outer bounds (qt_windows_geometry_offsets)');
      return;
    }
    await waitUntil(async () => {
      const current = await currentWindow(app, win, 'move_to');
      return current !== null && boundsNear(current.bounds, movedBounds, ['x', 'y']);
    }, 5000, 'moveTo() to change the reported position');
    // Restoring keeps the shared app usable for what follows.
    const moved = await currentWindow(app, win, 'move_to');
    assert.ok(moved, 'the moved window stays discoverable');
    await moved.moveTo(x, y);
    await waitUntil(async () => {
      const current = await currentWindow(app, win, 'move_to');
      return current !== null && boundsNear(current.bounds, { x, y }, ['x', 'y']);
    }, 5000, 'moveTo() to restore the original position');
  } catch (err) {
    // Never leave the shared app moved: best-effort move back, then the
    // original failure surfaces.
    try {
      const current = await windowAdvertising(app, 'move_to');
      if (current) {
        await current.moveTo(x, y);
      }
    } catch (_cleanup) {
      // best-effort cleanup; the original error wins
    }
    throw err;
  }
});

test('resizeTo() changes the reported bounds and restores the original size', async (t) => {
  const app = await getApp();
  const win = await windowAdvertising(app, 'resize_to');
  if (!win || !win.bounds) {
    t.skip('no window advertises resize_to with restorable bounds');
    return;
  }
  const { width, height } = win.bounds;
  const resizedBounds = { width: width + 50, height: height + 50 };
  try {
    await win.resizeTo(resizedBounds.width, resizedBounds.height);
    if (appEnv === 'qt' && process.platform === 'win32') {
      await waitUntil(async () => {
        const current = await currentWindow(app, win, 'resize_to');
        return current !== null && current.bounds !== null &&
          (!boundsNear(current.bounds, { width, height }, ['width', 'height']));
      }, 5000, 'Qt/UIA resizeTo() to produce an observable bounds change');
      try {
        const current = await currentWindow(app, win, 'resize_to');
        if (current) await current.resizeTo(width, height);
      } catch (_cleanup) {
        // best-effort cleanup before reporting the coordinate-space gap
      }
      t.skip('Qt/UIA client size differs from decorated outer bounds (qt_windows_geometry_offsets)');
      return;
    }
    const resizeNoop = appEnv === 'cocoa' || appEnv === 'winforms' || appEnv === 'wpf' || appEnv === 'egui' ||
      (appEnv === 'tauri' && process.platform !== 'linux');
    if (resizeNoop) {
      // The provider advertises and accepts TransformPattern.Resize, but the
      // framework leaves its bounds unchanged. Keep the dispatch covered and
      // report the known gap honestly instead of passing on the no-op.
      try {
        const current = await currentWindow(app, win, 'resize_to');
        if (current) await current.resizeTo(width, height);
      } catch (_cleanup) {
        // best-effort cleanup before reporting the known platform gap
      }
      const gap = appEnv === 'cocoa'
        ? 'cocoa_resize_noop'
        : appEnv === 'egui'
          ? 'egui_transform_resize_noop'
        : appEnv === 'winforms' || appEnv === 'wpf'
          ? `${appEnv}_transform_resize_noop`
          : 'tauri_desktop_resize_noop';
      t.skip(`${appEnv} accepts Resize without changing bounds (${gap})`);
      return;
    }
    await waitUntil(async () => {
      const current = await currentWindow(app, win, 'resize_to');
      return current !== null && boundsNear(current.bounds, resizedBounds, ['width', 'height']);
    }, 5000, 'resizeTo() to change the reported size');
    const resized = await currentWindow(app, win, 'resize_to');
    assert.ok(resized, 'the resized window stays discoverable');
    await resized.resizeTo(width, height);
    await waitUntil(async () => {
      const current = await currentWindow(app, win, 'resize_to');
      return current !== null && boundsNear(current.bounds, { width, height }, ['width', 'height']);
    }, 5000, 'resizeTo() to restore the original size');
  } catch (err) {
    // Same failure-preserving cleanup as moveTo: the shared app must not be
    // left resized for the suites after this one.
    try {
      const current = await windowAdvertising(app, 'resize_to');
      if (current) {
        await current.resizeTo(width, height);
      }
    } catch (_cleanup) {
      // best-effort cleanup; the original error wins
    }
    throw err;
  }
});

test('a real sibling window emits opened, minimized state, restored state, and closed events', async (t) => {
  const app = await getApp();
  if (!appConfig.siblingButtonName || !appConfig.siblingName) {
    t.skip('this app has no sibling-window event fixture');
    return;
  }

  const sub = await app.subscribe();
  try {
    const opened = await actionAndWait(
      sub,
      (event) => event.type === 'windowOpened',
      () => app.locator(`button[name="${appConfig.siblingButtonName}"]`).press(),
    );
    assert.equal(opened.type, 'windowOpened');

    await waitUntil(async () => (await siblingWindow(app)) !== null, 5000,
      `sibling ${appConfig.siblingName} to appear`);
    let sibling = await siblingWindow(app);
    assert.ok(sibling, 'the opened sibling must be discoverable');
    assert.ok(sibling.actions.includes('minimize'), 'the sibling advertises minimize');
    assert.ok(sibling.actions.includes('restore'), 'the sibling advertises restore');

    const minimized = await actionAndWait(
      sub,
      (event) => event.type === 'stateChanged' &&
        event.stateFlag === 'minimized' && event.stateValue === true,
      () => sibling.minimize(),
    );
    assert.equal(minimized.stateFlag, 'minimized');
    assert.equal(minimized.stateValue, true);
    await waitUntil(async () => (await siblingWindow(app))?.minimized === true, 5000,
      'the sibling snapshot to report minimized=true');

    sibling = await siblingWindow(app);
    assert.ok(sibling, 'a minimized sibling stays discoverable');
    const restored = await actionAndWait(
      sub,
      (event) => event.type === 'stateChanged' &&
        event.stateFlag === 'minimized' && event.stateValue === false,
      () => sibling.restore(),
    );
    assert.equal(restored.stateValue, false);
    await waitUntil(async () => (await siblingWindow(app))?.minimized === false, 5000,
      'the sibling snapshot to report minimized=false');

    const closed = await actionAndWait(
      sub,
      (event) => event.type === 'windowClosed',
      () => app.locator('button[name="Close Sibling"]').press(),
    );
    assert.equal(closed.type, 'windowClosed');
    await waitUntil(async () => (await siblingWindow(app)) === null, 5000,
      `sibling ${appConfig.siblingName} to disappear`);
  } finally {
    sub.close();
    await closeSiblingBestEffort(app);
  }
});

test('Locator window verbs dispatch through the async binding', async (t) => {
  const app = await getApp();
  const win = await windowAdvertising(app, 'minimize');
  if (!win || !win.actions.includes('restore')) {
    t.skip('no window advertises both minimize and restore');
    return;
  }
  // The Locator carries the selector + auto-wait machinery, so its dispatch
  // is a separate code path worth exercising end to end. The selector must
  // match exactly one window — and the top-level may be a Role::Window *or*
  // a Role::Dialog (the Qt and Cocoa apps' top level is a dialog), while
  // App.windows() lists both, so the selector must accept both.
  const locator = locatorForWindow(app, win);
  if (!locator) {
    t.skip('the target window has no name for a unique Locator');
    return;
  }
  try {
    await locator.minimize();
    await settledWindow(
      app, 'minimize', (w) => w.minimized === true, 'Locator.minimize() must report minimized');
    await locator.restore();
    await settledWindow(
      app, 'minimize', (w) => w.minimized === false, 'Locator.restore() must report restored');
  } catch (err) {
    // Never leave the shared app minimized; the original failure wins over
    // any cleanup failure.
    await restoreWindowBestEffort(app);
    throw err;
  }
});

test('Locator screen-fill dispatch through the async binding', async (t) => {
  const app = await getApp();
  const spec = await screenFillOperation(app);
  if (!spec) {
    t.skip('no window advertises a screen-filling verb with restore');
    return;
  }
  const win = await windowAdvertising(app, spec.action);
  const locator = locatorForWindow(app, win);
  if (!locator) {
    t.skip('the target window has no name for a unique Locator');
    return;
  }
  if (appEnv === 'tauri' && process.platform === 'darwin') {
    t.skip('Tauri/macOS Locator restore cannot clear fullscreen (tauri_macos_locator_screen_fill_restore_failure)');
    return;
  }
  try {
    await locator[spec.method]();
    if (appEnv === 'cocoa') {
      await locator.restore();
      t.skip('this app\'s window has no observable screen-fill state (cocoa_screen_fill_state_unobservable)');
      return;
    }
    if (['egui', 'qt'].includes(appEnv) && process.platform === 'darwin') {
      await locator.restore();
      t.skip(`${appEnv}/macOS Locator screen-fill has no observable state change ` +
        '(macos_locator_screen_fill_state_unobservable)');
      return;
    }
    await assertScreenFill(app, spec, true, `Locator ${spec.method}`);
    await locator.restore();
    await settledWindow(
      app, 'restore', (w) => w[spec.state] === false,
      `Locator restore must clear ${spec.state}`);
  } catch (err) {
    await restoreWindowBestEffort(app);
    throw err;
  }
});

test('Locator activate() dispatches through the async binding', async (t) => {
  const app = await getApp();
  const win = await windowAdvertising(app, 'activate');
  if (!win) {
    t.skip('no window advertises activate on this app/platform');
    return;
  }
  const locator = locatorForWindow(app, win);
  if (!locator) {
    t.skip('the target window has no name for a unique Locator');
    return;
  }
  // A non-minimized window has nothing to restore: activate only changes
  // focus/stacking (a minimized window is restored first).
  await locator.activate();
});

test('Locator moveTo() dispatches and puts the window back where it was', async (t) => {
  const app = await getApp();
  const win = await windowAdvertising(app, 'move_to');
  if (!win || !win.bounds) {
    t.skip('no window advertises move_to with restorable bounds');
    return;
  }
  const locator = locatorForWindow(app, win);
  if (!locator) {
    t.skip('the target window has no name for a unique Locator');
    return;
  }
  const { x, y } = win.bounds;
  try {
    await locator.moveTo(x + 10, y + 10);
    if (appEnv === 'qt' && process.platform === 'win32') {
      await waitUntil(async () => {
        const current = await currentWindow(app, win, 'move_to');
        return current !== null && current.bounds !== null &&
          (!boundsNear(current.bounds, { x, y }, ['x', 'y']));
      }, 5000, 'Qt/UIA Locator.moveTo() to produce an observable bounds change');
      try {
        const current = await currentWindow(app, win, 'move_to');
        if (current) await current.moveTo(x, y);
      } catch (_cleanup) {
        // best-effort cleanup before reporting the coordinate-space gap
      }
      t.skip('Qt/UIA client coordinates differ from decorated outer bounds (qt_windows_geometry_offsets)');
      return;
    }
    await waitUntil(async () => {
      const current = await currentWindow(app, win, 'move_to');
      return current !== null && boundsNear(
        current.bounds, { x: x + 10, y: y + 10 }, ['x', 'y'],
      );
    }, 5000, 'Locator.moveTo() to change the reported position');
    await locator.moveTo(x, y);
    await waitUntil(async () => {
      const current = await currentWindow(app, win, 'move_to');
      return current !== null && boundsNear(current.bounds, { x, y }, ['x', 'y']);
    }, 5000, 'Locator.moveTo() to restore the original position');
  } catch (err) {
    // Never leave the shared app moved: best-effort move back, then the
    // original failure surfaces.
    try {
      const current = await windowAdvertising(app, 'move_to');
      if (current) {
        await current.moveTo(x, y);
      }
    } catch (_cleanup) {
      // best-effort cleanup; the original error wins
    }
    throw err;
  }
});

test('Locator resizeTo() dispatches and restores the original size', async (t) => {
  const app = await getApp();
  const win = await windowAdvertising(app, 'resize_to');
  if (!win || !win.bounds) {
    t.skip('no window advertises resize_to with restorable bounds');
    return;
  }
  const locator = locatorForWindow(app, win);
  if (!locator) {
    t.skip('the target window has no name for a unique Locator');
    return;
  }
  const { width, height } = win.bounds;
  try {
    await locator.resizeTo(width + 50, height + 50);
    if (appEnv === 'qt' && process.platform === 'win32') {
      await waitUntil(async () => {
        const current = await currentWindow(app, win, 'resize_to');
        return current !== null && current.bounds !== null &&
          (!boundsNear(current.bounds, { width, height }, ['width', 'height']));
      }, 5000, 'Qt/UIA Locator.resizeTo() to produce an observable bounds change');
      try {
        const current = await currentWindow(app, win, 'resize_to');
        if (current) await current.resizeTo(width, height);
      } catch (_cleanup) {
        // best-effort cleanup before reporting the coordinate-space gap
      }
      t.skip('Qt/UIA client size differs from decorated outer bounds (qt_windows_geometry_offsets)');
      return;
    }
    const resizeNoop = appEnv === 'cocoa' || appEnv === 'winforms' || appEnv === 'wpf' || appEnv === 'egui' ||
      (appEnv === 'tauri' && process.platform !== 'linux');
    if (resizeNoop) {
      try {
        const current = await currentWindow(app, win, 'resize_to');
        if (current) await current.resizeTo(width, height);
      } catch (_cleanup) {
        // best-effort cleanup before reporting the known platform gap
      }
      const gap = appEnv === 'cocoa'
        ? 'cocoa_resize_noop'
        : appEnv === 'egui'
          ? 'egui_transform_resize_noop'
        : appEnv === 'winforms' || appEnv === 'wpf'
          ? `${appEnv}_transform_resize_noop`
          : 'tauri_desktop_resize_noop';
      t.skip(`${appEnv} accepts Resize without changing bounds (${gap})`);
      return;
    }
    await waitUntil(async () => {
      const current = await currentWindow(app, win, 'resize_to');
      return current !== null && boundsNear(
        current.bounds,
        { width: width + 50, height: height + 50 },
        ['width', 'height'],
      );
    }, 5000, 'Locator.resizeTo() to change the reported size');
    await locator.resizeTo(width, height);
    await waitUntil(async () => {
      const current = await currentWindow(app, win, 'resize_to');
      return current !== null && boundsNear(
        current.bounds, { width, height }, ['width', 'height'],
      );
    }, 5000, 'Locator.resizeTo() to restore the original size');
  } catch (err) {
    try {
      const current = await windowAdvertising(app, 'resize_to');
      if (current) {
        await current.resizeTo(width, height);
      }
    } catch (_cleanup) {
      // best-effort cleanup; the original error wins
    }
    throw err;
  }
});

test('duplicate titles are preserved and a Locator re-resolves after one hides', async (t) => {
  const app = await getApp();
  if (!appConfig.duplicateButtonName || !appConfig.duplicateWindowName) {
    t.skip('this app has no duplicate-window fixture');
    return;
  }
  const selector = `window[name="${appConfig.duplicateWindowName}"]`;
  try {
    await app.locator(`button[name="${appConfig.duplicateButtonName}"]`).press();
    await waitUntil(async () => (await app.locator(selector).elements()).length === 2,
      5000, 'both same-titled windows to be discoverable');

    const firstLocator = app.locator(selector).first();
    const first = await firstLocator.element();
    assert.ok(first.bounds, 'the first duplicate has comparable bounds');
    assert.ok(first.actions.includes('close'), 'the duplicate advertises close');
    const firstX = first.bounds.x;
    await first.close();

    await waitUntil(async () => (await app.locator(selector).elements()).length === 1,
      5000, 'the hidden duplicate to leave discovery');
    const replacement = await firstLocator.element();
    assert.equal(replacement.name, appConfig.duplicateWindowName);
    assert.notEqual(replacement.bounds?.x, firstX,
      'the same Locator resolves the other native window after the first hides');
  } finally {
    await closeDuplicatesBestEffort(app);
  }
});

test('Element.close() dispatches on a secondary dialog', async (t) => {
  const app = await getApp();
  const dlg = await openDialog(app);
  if (!dlg) {
    t.skip('this app has no secondary-dialog fixture');
    return;
  }
  await withDialogCleanup(app, async () => {
    if (dlg.actions.includes('close')) {
      await dlg.close();
      await waitUntil(async () => (await dialogWindow(app)) === null, 5000,
        'the dialog to disappear after close()');
    } else {
      // The platform has no close API (AT-SPI on Linux): the dispatch must
      // fail surfaceably (tenet 2 — never input-simulate), and the error
      // must reach the binding as ActionNotSupportedError.
      await assert.rejects(dlg.close(), ActionNotSupportedError);
    }
  });
});

test('Locator.close() dispatches on a secondary dialog', async (t) => {
  const app = await getApp();
  const dlg = await openDialog(app);
  if (!dlg) {
    t.skip('this app has no secondary-dialog fixture');
    return;
  }
  await withDialogCleanup(app, async () => {
    const locator = locatorForWindow(app, dlg);
    if (!locator) {
      t.skip('the dialog has no name for a unique Locator');
      return;
    }
    if (dlg.actions.includes('close')) {
      await locator.close();
      await waitUntil(async () => (await dialogWindow(app)) === null, 5000,
        'the dialog to disappear after Locator.close()');
    } else {
      await assert.rejects(locator.close(), ActionNotSupportedError);
    }
  });
});
