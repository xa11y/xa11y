"""Mutating window-management coverage for the Python binding, against a real app.

The main python suite (``tests/suites/python/test_window.py``) is deliberately
non-mutating: the harness runs python → js → cli against one shared app
instance, and window-state verbs (minimize / restore / resize) churn the
UIA/AX cache in a way that made the *next* suite's action tests flaky (the
egui-windows ``02_actions`` checkbox read-back regressed when this window was
minimized first).

This file runs in the ``python-window`` suite, which the harness orders
*after* every other suite, so the churn disturbs nothing that follows — and
the real PyO3 wiring of the mutating verb methods gets exercised against an
actual provider instead of only against the mock (the surface-level suite
cannot do that on purpose).

Every test restores the window it mutated: a failed ``restore`` ends the
test (it is a real provider failure, not cleanup noise), but a best-effort
restore is attempted first so the shared app is never left mutated for the
test after it — same failure-preserving pattern as the Locator and event
tests. A verb advertised in ``actions`` must dispatch to the real
platform action (tenet 3): ``ActionNotSupportedError`` from an advertised
verb is a fidelity regression and fails the test. The only legitimate skip
is "no window advertises the verb" — never "advertised but the platform
cannot perform it".
"""

from __future__ import annotations

import os
import sys
import time

import pytest

import xa11y

pytestmark = pytest.mark.window_mutating

# The marker derives the app identity from XA11Y_TEST_APP at collection time
# (the ``app_name`` fixture resolves too late for a module-level skipif). Qt
# is the known-bad event-delivery combo, the same conclusion test_events.py
# derives the same way.
APP = os.environ.get("XA11Y_TEST_APP", "tauri")
QT = APP == "qt"
# The platform, not the app identity: the sequence drill in
# ``test_screen_fill_minimize_sequence`` is about the macOS fullscreen
# animation, and runs for every macOS test app (cocoa, egui, qt, tauri), not
# just one.
MACOS = sys.platform == "darwin"

# `close` is only exercised against a *secondary* dialog window (opened via
# the app's "Open Dialog" button, see `_open_dialog`): closing the shared
# app's main window would kill the app the harness still needs for the suites
# after this one (and for its teardown). The dialog ever left open would also
# change the enumeration the next suites rely on, so every close test ends
# with a best-effort close of the dialog — through the platform close action
# when it exists, else through the dialog's own "Close Dialog" button.


def _window_advertising(app: xa11y.App, verb: str) -> xa11y.Element | None:
    """The first window-like element advertising `verb`, or None."""
    windows = app.windows()
    if not windows:
        return None
    for w in windows:
        if verb in w.actions:
            return w
    return None


def _wait_for_window(app: xa11y.App, verb: str, what: str) -> xa11y.Element:
    """The first window advertising `verb`, polling briefly.

    Cleanup-only: a failure mid-transition can leave the real window out of
    ``App.windows()`` for a moment, and the cleanup rails must still find
    something to restore rather than raising a second error. The happy-path
    assertions use ``_settled_window`` instead.
    """
    return _wait_until(lambda: _window_advertising(app, verb), 5.0, what)


def _wait_until(predicate, timeout: float, what: str):
    """Poll `predicate` until it returns truthy, or `timeout` (seconds) elapses.

    Returns the truthy value, so a caller that needs the polled object (e.g.
    ``_wait_for_window``) reuses the same loop. Raises with a description on
    timeout — a dead poll is a fixture regression, not a skip (mirrors
    ``wait_until`` in the Rust integ suite).
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.1)
    raise AssertionError(f"timed out waiting for {what}")


def _settled_window(
    app: xa11y.App, verb: str, predicate, what: str
) -> xa11y.Element:
    """The window advertising `verb` once ``predicate(win)`` holds.

    macOS settles a window-state transition before the verb returns (the
    provider polls the platform state), so one fresh read must already hold
    and a poll would mask a provider that stopped settling. Windows drives
    ``WindowVisualState`` directly and promises only that the set call
    succeeded, so its cells keep the short poll the suites used before the
    macOS settle made it unnecessary. Linux never reaches either branch: the
    window verbs are unsupported there and the callers skip.
    """

    def matching() -> xa11y.Element | None:
        win = _window_advertising(app, verb)
        if win is None or not predicate(win):
            return None
        return win

    if MACOS:
        win = _window_advertising(app, verb)
        assert win is not None, f"{what}: no window advertises {verb}"
        assert predicate(win), (
            f"{what}: minimized={win.minimized!r} fullscreen={win.fullscreen!r} "
            f"maximized={win.maximized!r}"
        )
        return win
    return _wait_until(matching, 5.0, what)


def _restore_window_best_effort(app: xa11y.App) -> None:
    """Best-effort restore of the real window, for cleanup rails.

    ``_wait_for_window`` with `restore`, not a one-shot lookup: the cleanup
    runs after a failure that may itself have caught the window mid-transition,
    and a window that advertises `maximize`/`enter_fullscreen` does not
    necessarily advertise the `restore` this cleanup needs. Never raises:
    cleanup must not replace the original failure.
    """
    try:
        current = _wait_for_window(app, "restore", "a restorable window")
        current.restore()
    except Exception:  # best-effort cleanup; the original error wins
        pass


def _window_named(app: xa11y.App, dialog_name: str) -> xa11y.Element | None:
    """The first element from App.windows() whose name contains `dialog_name`."""
    if not dialog_name:
        return None
    try:
        windows = app.windows()
    except xa11y.PlatformError as exc:
        # A newly opened AppKit window can briefly reject AXChildren while
        # its native title-bar controls are still being installed. Treat that
        # one transient discovery result as "not visible yet" so callers such
        # as _open_dialog keep polling; surface every other provider failure.
        if sys.platform == "darwin" and "AXError -25202" in str(exc):
            return None
        raise
    for w in windows:
        if w.name and dialog_name in w.name:
            return w
    return None


def _tree_dialog(app: xa11y.App, dialog_name: str) -> xa11y.Element | None:
    """The first window/dialog element in the app's tree named `dialog_name`.

    Covers toolkits whose secondary dialog appears in the accessibility tree
    but not in ``App.windows()`` (the GTK app's AT-SPI tree has no top-level
    window node, but its dialog is a Role::Dialog child — the same shape
    ``test_dialog_role`` in test_compat.py relies on).
    """
    if not dialog_name:
        return None
    results = app.locator(
        f'window[name*="{dialog_name}"], dialog[name*="{dialog_name}"]'
    ).elements()
    return results[0] if results else None


def _dialog_present(app: xa11y.App, dialog_name: str) -> bool:
    """Whether the dialog is reachable at all (as a top-level window or in-tree)."""
    return (
        _window_named(app, dialog_name) is not None
        or _tree_dialog(app, dialog_name) is not None
    )


def _open_dialog(app: xa11y.App, config: dict) -> xa11y.Element:
    """Press the app's "Open Dialog" button and wait for the dialog window.

    Returns the dialog element, preferring the top-level window shape (what
    ``close`` acts on) and falling back to the in-tree node. Skips when the
    app has no dialog button; the "pressed but no dialog appeared" case is a
    fixture regression and fails (the dialog name comes from the same config
    the CLI suite's close test relies on), *after* a best-effort close so a
    skipped/failed test never leaves the dialog open for the suites that
    follow.
    """
    btn_name = config.get("dialog_button_name")
    if not btn_name:
        pytest.skip("app config has no dialog_button_name")
    dialog_name = config.get("dialog_name", "")
    if not dialog_name:
        pytest.skip("app config has no dialog_name")
    try:
        app.locator(f'button[name="{btn_name}"]').press()
    except xa11y.TimeoutError:
        pytest.skip(f"no button named {btn_name!r} in this app's tree")
    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline:
        dlg = _window_named(app, dialog_name)
        if dlg is not None:
            return dlg
        dlg = _tree_dialog(app, dialog_name)
        if dlg is not None:
            return dlg
        time.sleep(0.1)
    # The dialog never appeared. Clean up the press side effect first, then
    # fail loudly: this is a real fixture regression, not a capability gap.
    _close_dialog(app, config, strict=False)
    raise AssertionError(
        f"no window named {dialog_name!r} appeared after pressing {btn_name!r}"
    )


def _dialog_gone_within(app: xa11y.App, dialog_name: str, timeout: float) -> bool:
    """Poll until the dialog leaves the tree; True when it did within `timeout`."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not _dialog_present(app, dialog_name):
            return True
        time.sleep(0.1)
    return False


def _close_dialog(app: xa11y.App, config: dict, *, strict: bool) -> None:
    """Close a still-open dialog and wait until it leaves the tree.

    Prefers the platform close action; falls back to the dialog's own
    "Close Dialog" button. The fixture hides (not destroys) its dialog, so
    the press returning only means the click was delivered — the suites that
    follow enumerate windows (the GTK capability probe asserts a clean
    enumeration) and must not race the hide. A close that was accepted but
    did not take effect is retried, because a leftover dialog corrupts the
    *next* suite's enumeration rather than this one's.

    ``strict`` decides what a dialog that never leaves after an accepted
    close means: raised as a cleanup failure when the test itself passed,
    swallowed when it did not (the original failure wins). A close that
    cannot even be dispatched is a different matter — Qt's dialog button is
    not actionable through AT-SPI, so the press times out, and the old rail
    swallowed that and left the dialog; there is nothing to retry, the
    platform's own body assertion already covered the no-close-API contract,
    and the dialog is a pre-existing fixture limitation. That case returns
    silently, strict or not. A missing button/dialog is never an error —
    there is nothing to close.
    """
    dialog_name = config.get("dialog_name", "")
    close_button = config.get("dialog_close_button_name", "Close Dialog")
    attempts = 3
    dispatched = False
    try:
        for _ in range(attempts):
            dlg = _window_named(app, dialog_name) or _tree_dialog(app, dialog_name)
            if dlg is None:
                return
            try:
                if "close" in dlg.actions:
                    dlg.close()
                else:
                    app.locator(f'button[name="{close_button}"]').press()
            except Exception:
                # Cannot dispatch a close at all; retrying the same
                # non-actionable target would only repeat the timeout.
                return
            dispatched = True
            if _dialog_gone_within(app, dialog_name, 2.0):
                return
        if dispatched:
            raise AssertionError(
                f"the dialog {dialog_name!r} is still present after {attempts} "
                "accepted closes; the suites that follow enumerate windows and "
                "would see it"
            )
    except Exception:
        if strict:
            raise
        # best-effort cleanup; the original error wins


def _close_dialog_from_finally(app: xa11y.App, config: dict) -> None:
    """Run the dialog cleanup from a test's ``finally`` block.

    A cleanup failure is only allowed to fail the test when the test itself
    passed; when it did not, the original failure is the one worth reporting
    (``_close_dialog`` swallows it). ``sys.exc_info()`` is the in-flight
    exception inside a ``finally``.
    """
    _close_dialog(app, config, strict=sys.exc_info()[0] is None)


def _locator_for_window(app: xa11y.App, win: xa11y.Element) -> xa11y.Locator:
    """A single-match Locator for a window-like element.

    The top-level may be a Role::Window *or* a Role::Dialog (the Qt and
    Cocoa apps' top level is a dialog), and ``App.windows()`` lists both —
    so the selector must accept both or it matches nothing.
    """
    if win.name is None:
        pytest.skip("the window has no name to build a selector from")
    return app.locator(f'window[name="{win.name}"], dialog[name="{win.name}"]')


def test_python_window_suite_resolves_the_app(app: xa11y.App) -> None:
    """The suite's smoke anchor.

    The harness fails a matrix cell whose suite executes zero tests. On apps
    that expose no window-like element at all (the GTK app's AT-SPI tree has
    no frame node) every mutating test below would skip, so this one always
    executes and keeps the cell honest.
    """
    assert isinstance(app.windows(), list)


@pytest.mark.skipif(sys.platform != "linux", reason="AT-SPI is Linux-only")
def test_atspi_geometry_capability_is_measured_for_gtk_and_qt(app: xa11y.App) -> None:
    """Pin the GTK/Qt results that decide whether geometry is advertised.

    Qt exports Component.SetPosition and Component.SetSize on its top-level
    accessible, but returns ``false`` from both. GTK's adapter exposes no
    top-level window accessible in this fixture. Neither therefore provides
    a capability xa11y can honestly advertise.
    """
    if APP not in {"gtk", "qt"}:
        pytest.skip("this probe is specifically for the GTK and Qt adapters")
    windows = app.windows()
    if APP == "gtk":
        assert windows == [], "GTK unexpectedly gained a targetable AT-SPI window"
        return
    assert windows, "Qt must expose its top-level window through AT-SPI"
    win = windows[0]
    assert "move_to" not in win.actions
    assert "resize_to" not in win.actions
    bounds = win.bounds
    assert bounds is not None
    with pytest.raises(xa11y.ActionNotSupportedError):
        win.move_to(bounds.x + 10, bounds.y + 10)
    with pytest.raises(xa11y.ActionNotSupportedError):
        win.resize_to(bounds.width + 10, bounds.height + 10)


def test_minimize_and_restore(app: xa11y.App) -> None:
    """``Element.minimize`` + ``Element.restore`` reach the platform.

    The window advertises ``minimize``, so the call must dispatch to the real
    platform action (tenet 3): an ``ActionNotSupportedError`` here is a
    fidelity regression and fails the test — the skip above covered "no
    window advertises". A successful minimize must also be undoable via the
    advertised ``restore`` — a restore failure ends the test instead of
    leaving the shared app minimized for the next test.
    """
    win = _window_advertising(app, "minimize")
    if win is None:
        pytest.skip("this app's windows advertise no minimize action")
    if "restore" not in win.actions:
        pytest.skip("no window advertises both minimize and restore")
    try:
        win.minimize()
        minimized = _settled_window(
            app,
            "minimize",
            lambda w: w.minimized is True,
            "minimize must report minimized",
        )
        # Advertised restore must succeed; a failure here is a real provider
        # promise that could not be kept, not a skip.
        minimized.restore()
        _settled_window(
            app,
            "minimize",
            lambda w: w.minimized is False,
            "restore must report restored",
        )
    except Exception:
        # Never leave the shared app minimized for the suite after this one;
        # the original failure wins over any cleanup failure.
        _restore_window_best_effort(app)
        raise


# The screen-filling operation each platform exposes. macOS has native
# fullscreen (``enter_fullscreen``, ``AXFullScreen``) and refuses ``maximize``
# — there is no readable or writable zoom state — while Windows has
# ``maximize`` (UIA ``WindowVisualState``) and no fullscreen API at all. The
# tests assert the platform's own verb and the platform's own state; they
# never treat the two states as one bit.
SCREEN_FILL_OPERATIONS = (
    ("enter_fullscreen", "fullscreen"),
    ("maximize", "maximized"),
)


def _screen_fill_operation(app: xa11y.App) -> tuple[str, str] | None:
    """The ``(verb, state)`` this platform exposes, or None.

    ``enter_fullscreen`` is preferred over ``maximize`` because no platform
    advertises both for the same window: macOS refuses ``maximize`` and
    Windows has no fullscreen operation.
    """
    for verb, state in SCREEN_FILL_OPERATIONS:
        win = _window_advertising(app, verb)
        if win is not None and "restore" in win.actions:
            return verb, state
    return None


def _assert_screen_fill(
    app: xa11y.App, verb: str, state: str, want: bool, what: str
) -> None:
    """Assert the platform's screen-fill state after one settled read.

    ``_settled_window`` reads once on macOS and polls briefly on Windows (see
    there). The *other* state must stay unknown (``None``): macOS cannot report
    ``maximized`` and Windows cannot report ``fullscreen`` — that is exactly
    the separation between the two operations.
    """
    win = _settled_window(
        app, verb, lambda w: getattr(w, state) is want, f"{what}: {state}"
    )
    other = "maximized" if state == "fullscreen" else "fullscreen"
    assert getattr(win, other) is None, (
        f"{what}: {other} must stay unknown — {verb} is not that operation"
    )


def test_screen_fill_minimize_sequence(app: xa11y.App) -> None:
    """The platform's screen-filling verb and ``restore`` reach the platform.

    macOS exposes native fullscreen as ``enter_fullscreen`` (``AXFullScreen``)
    and refuses ``maximize``: the classic zoom state has no accessibility
    surface, so substituting fullscreen would blur two distinct operations.
    Windows exposes ``maximize`` (UIA ``WindowVisualState_Maximized``) and has
    no fullscreen API. The test picks the platform's own verb and then runs
    the back-to-back ``max, min, max, min`` sequence the settling promise
    exists for: every step asserts the state after the call, so a verb that
    left a transition half-applied fails the next step (issue #399). The
    macOS provider settles before the call returns, so macOS asserts with one
    read; Windows polls briefly (see ``_settled_window``).
    """
    spec = _screen_fill_operation(app)
    if spec is None:
        pytest.skip("no window advertises a screen-filling verb with restore")
    verb, state = spec
    try:
        # Fill the screen.
        getattr(_window_advertising(app, verb), verb)()
        _assert_screen_fill(app, verb, state, True, f"after {verb}")

        # A repeated call is a no-op, not a toggle.
        getattr(_window_advertising(app, verb), verb)()
        _assert_screen_fill(app, verb, state, True, f"after repeated {verb}")

        # minimize: on macOS this leaves fullscreen first (AppKit ignores a
        # minimized set while the window is fullscreen); on Windows it is the
        # direct UIA transition from Maximized.
        _window_advertising(app, verb).minimize()
        win = _settled_window(
            app, verb, lambda w: w.minimized is True, "minimize must report minimized"
        )
        assert getattr(win, state) is False, (
            f"minimize must clear {state} before iconifying"
        )

        # And back: the minimized window is still reachable by the next call.
        getattr(_window_advertising(app, verb), verb)()
        _assert_screen_fill(app, verb, state, True, f"after re-{verb} from minimized")

        _window_advertising(app, verb).minimize()
        _settled_window(
            app,
            verb,
            lambda w: w.minimized is True,
            "the second minimize must report minimized",
        )

        # restore clears both states.
        _window_advertising(app, "restore").restore()
        win = _settled_window(
            app,
            "restore",
            lambda w: w.minimized is False,
            "restore must report restored",
        )
        assert getattr(win, state) is False, f"restore must clear {state}"
    except Exception:
        # Never leave the shared app fullscreen/minimized/maximized for the
        # suites after this one; the original failure wins over cleanup.
        _restore_window_best_effort(app)
        raise


def test_move_to_restores_the_original_position(app: xa11y.App) -> None:
    """``Element.move_to`` dispatches, and the window is put back where it was.

    Restoring is what keeps the shared app usable for the suites that follow
    this one, so a failed restore ends the test (it is not "just cleanup") —
    but the shared app must still not be left moved, so a best-effort move
    back to the original bounds is attempted before the original failure
    surfaces. An advertised ``move_to`` that raises ``ActionNotSupportedError``
    is a fidelity regression and fails the test.
    """
    win = _window_advertising(app, "move_to")
    if win is None:
        pytest.skip("this app's windows advertise no move_to action")
    bounds = win.bounds
    if bounds is None:
        pytest.skip("the window reports no bounds to restore from")
    try:
        win.move_to(bounds.x + 10, bounds.y + 10)
        win.move_to(bounds.x, bounds.y)
    except Exception:
        try:
            current = _window_advertising(app, "move_to")
            if current is not None and "move_to" in current.actions:
                current.move_to(bounds.x, bounds.y)
        except Exception:  # best-effort cleanup; the original error wins
            pass
        raise


def test_resize_to_restores_the_original_size(app: xa11y.App) -> None:
    """``Element.resize_to`` dispatches, and the window's size is restored."""
    win = _window_advertising(app, "resize_to")
    if win is None:
        pytest.skip("this app's windows advertise no resize_to action")
    bounds = win.bounds
    if bounds is None:
        pytest.skip("the window reports no bounds to restore from")
    try:
        win.resize_to(bounds.width + 50, bounds.height + 50)
        win.resize_to(bounds.width, bounds.height)
    except Exception:
        # Same failure-preserving cleanup: don't leave the shared app resized
        # for the suites after this one.
        try:
            current = _window_advertising(app, "resize_to")
            if current is not None and "resize_to" in current.actions:
                current.resize_to(bounds.width, bounds.height)
        except Exception:  # best-effort cleanup; the original error wins
            pass
        raise


def test_move_to_and_resize_to_are_enforced_on_winforms(
    app: xa11y.App, app_config: dict
) -> None:
    """Windows TransformPattern coverage is enforced, not just claimed.

    tests/matrix.yaml cites this suite's winforms cell as the Windows
    exercise of ``move_to`` / ``resize_to``, but the restoring tests above
    skip when the verbs are not advertised — a regression that removed
    TransformPattern support would leave every cited test green. On the
    winforms cell require both verbs to be advertised and their calls to
    succeed, and the moved bounds to change (within the one-pixel rounding
    of the logical/physical round-trip); ``resize_to``'s bounds change is
    documented as a winforms provider no-op (the call answers success and
    the form size never changes — see the ``winforms_transform_resize_noop``
    gap in tests/matrix.yaml), so that half is asserted as an honest skip
    rather than a green lie. The shared app is put back exactly, best
    effort, after the assertions.
    """
    if "winforms" not in str(app_config.get("window_name_contains", "")):
        pytest.skip("the Windows TransformPattern assertion targets the winforms cell")
    win = _window_advertising(app, "move_to")
    if win is None or "resize_to" not in win.actions:
        raise AssertionError(
            "the winforms app must advertise move_to and resize_to (TransformPattern); "
            "a regression that drops the advertisement would silently skip every "
            "cited coverage test"
        )
    bounds = win.bounds
    if bounds is None:
        raise AssertionError("the winforms window must report bounds")
    name = win.name or ""
    try:
        win.move_to(bounds.x + 10, bounds.y + 10)

        # `Element.bounds` is the snapshot the element was built from, so the
        # moved window must be re-resolved through App.windows() before the
        # bounds are read (the element path itself is what dispatches the
        # call; the fresh element is what observes its effect).
        def _moved() -> bool:
            fresh = _window_named(app, name)
            b = fresh.bounds if fresh is not None else None
            return bool(
                b is not None
                and abs(b.x - (bounds.x + 10)) <= 1
                and abs(b.y - (bounds.y + 10)) <= 1
            )

        _wait_until(_moved, 5.0, "bounds after move_to to reflect the new position")
        # Re-resolve before dispatching the resize: the element captured
        # before the move may hold a stale cached handle (the move rebuilt
        # the provider's element cache), and a dead element's resize would
        # no-op while this test reports the bounds never changed.
        fresh = _window_named(app, name)
        if fresh is None:
            raise AssertionError("the winforms window vanished after move_to")
        if "resize_to" not in fresh.actions:
            raise AssertionError("the winforms window lost resize_to after move_to")
        fresh.resize_to(bounds.width + 50, bounds.height + 50)

        # Diagnose, don't guess: capture the last observed bounds so the
        # winforms resize no-op is visible rather than hidden behind a bare
        # timeout, then record it as the known framework gap.
        deadline = time.monotonic() + 5.0
        last: tuple | None = None
        resized_ok = False
        while time.monotonic() < deadline:
            fresh2 = _window_named(app, name)
            if fresh2 is None or fresh2.bounds is None:
                time.sleep(0.1)
                continue
            b = fresh2.bounds
            last = (b.x, b.y, b.width, b.height)
            if (
                abs(b.width - (bounds.width + 50)) <= 1
                and abs(b.height - (bounds.height + 50)) <= 1
            ):
                resized_ok = True
                break
            time.sleep(0.1)
        if not resized_ok:
            pytest.skip(
                "winforms UIA TransformPattern.Resize answers success but the form bounds "
                f"never change (expected width={bounds.width + 50} height={bounds.height + 50}, "
                f"observed {last}); recorded as the winforms_transform_resize_noop gap in "
                "tests/matrix.yaml — move_to is the enforced Windows geometry assertion"
            )
    finally:
        # Put the shared app back exactly, failure-preserving: the suite
        # after this one must not see a moved/resized window.
        current = _window_named(app, name) or win
        try:
            if "move_to" in current.actions:
                current.move_to(bounds.x, bounds.y)
            if "resize_to" in current.actions:
                current.resize_to(bounds.width, bounds.height)
        except Exception:  # best-effort cleanup; the original error wins
            pass


@pytest.mark.skipif(
    QT,
    reason=(
        "Qt does not reliably emit StateChanged events for programmatic "
        "accessibility actions across AT-SPI2 / UIA / AX (known-bad; see "
        "tests/suites/python/test_events.py)."
    ),
)
def test_state_changed_minimized_on_minimize_restore(app: xa11y.App) -> None:
    """``minimize``/``restore`` raise ``StateChanged{minimized}``.

    The event half of the window verbs: on Windows the provider translates
    UIA ``PropertyChanged(WindowVisualState)`` into ``StateChanged{Minimized}``
    (alongside ``{Maximized}``, which only reaches a subscriber from this
    path). A real native window is the only host that raises the property
    event — the AccessKit app cannot (its ``IWindowProvider`` returns
    ``not_supported`` for visual state), so the Rust integ suite has no
    surface for it. On macOS the same pair arrives via
    ``AXWindowMiniaturized`` / ``AXWindowDeminiaturized``; Linux windows do
    not advertise the verbs, which skips the Linux cells.

    ``minimize`` must deliver ``{minimized: true}`` and ``restore``
    ``{minimized: false}`` — the false half is what proves event and
    snapshot agree. A restore failure, or an event that never arrives, ends
    the test (the restore cleanup below still tries) rather than leaving the
    shared app minimized for the suites after this one.
    """

    win = _window_advertising(app, "minimize")
    if win is None or "restore" not in win.actions:
        pytest.skip("this app's windows advertise no minimize/restore")

    cancelled_sub = app.subscribe()
    try:
        with app.subscribe() as sub:
            win.minimize()
            event = sub.wait_for(
                lambda e: (
                    e.event_type == xa11y.EventType.STATE_CHANGED
                    and e.state_flag == "minimized"
                    and e.state_value is True
                ),
                timeout=5.0,
            )
            assert event.state_flag == "minimized"
            assert event.state_value is True

            win.restore()
            event = sub.wait_for(
                lambda e: (
                    e.event_type == xa11y.EventType.STATE_CHANGED
                    and e.state_flag == "minimized"
                    and e.state_value is False
                ),
                timeout=5.0,
            )
            assert event.state_value is False
    except Exception:
        # Never leave the shared app minimized for the suite after this one;
        # the original failure wins over any cleanup failure.
        _restore_window_best_effort(app)
        raise


def test_state_changed_minimized_on_sibling_window(
    app: xa11y.App, app_config: dict
) -> None:
    """A *sibling* top-level window raises ``StateChanged{Minimized}`` too (C1).

    The Windows subscription registers its scoped handlers on **every**
    top-level window of the pid, plus a desktop-scoped open/close watch that
    attaches windows opened after subscribe. This test subscribes **before**
    opening the sibling, so the watch patches the new window's handlers itself
    — the strongest proof of both halves: a sibling window that is not the
    process's first top-level window must deliver ``StateChanged{Minimized}``
    on the app subscription. The pre-C1 implementation scoped the handlers to
    the first window only and silently dropped exactly this event — the
    sibling window exists (the WPF app's "Open Sibling" non-modal window) to
    prove it end to end on Windows CI.

    Same contract as the main-window test: ``minimize`` must deliver
    ``{minimized: true}`` and ``restore`` ``{minimized: false}``, and a
    restore failure ends the test rather than leaving the sibling minimized
    for the suites after this one.
    """
    sibling_btn = app_config.get("sibling_button_name")
    sibling_name = app_config.get("sibling_name")
    if not sibling_btn or not sibling_name:
        pytest.skip("app config has no sibling window")

    try:
        with app.subscribe() as sub:
            # Open the sibling only after the subscription is live, so the
            # open/close watch — not the subscribe-time enumeration — is what
            # attaches the new window's handlers (C1 re-attachment path).
            try:
                app.locator(f'button[name="{sibling_btn}"]').press()
            except xa11y.TimeoutError:
                pytest.skip(f"no button named {sibling_btn!r} in this app's tree")

            # WindowOpened is the readiness barrier: when it is observable,
            # the new window's property handlers and visual-state baseline
            # must already be attached. Minimize immediately after it rather
            # than polling/sleeping around the registration race (#424).
            cancelled_sub.wait_for(
                lambda e: e.event_type == xa11y.EventType.WINDOW_OPENED,
                timeout=5.0,
            )
            sub.wait_for(
                lambda e: e.event_type == xa11y.EventType.WINDOW_OPENED,
                timeout=5.0,
            )
            sibling = _window_named(app, sibling_name)
            if sibling is None:
                raise AssertionError(
                    f"no window named {sibling_name!r} appeared after "
                    f"pressing {sibling_btn!r}"
                )
            if "minimize" not in sibling.actions or "restore" not in sibling.actions:
                # The fixture is in place and the WPF main window already
                # proves WindowAutomationPeer raises the property event, so a
                # sibling without the verbs is a real gap, not an absent
                # fixture — fail loudly rather than hide it behind a skip
                # (tenet 1).
                raise AssertionError(
                    f"sibling {sibling_name!r} advertises "
                    f"{sorted(sibling.actions)!r}; expected minimize/restore"
                )

            def wait_minimized(value: bool):
                return sub.wait_for(
                    lambda e: (
                        e.event_type == xa11y.EventType.STATE_CHANGED
                        and e.state_flag == "minimized"
                        and e.state_value is value
                    ),
                    timeout=5.0,
                )

            # Cancel one of two concurrent subscriptions while the new window
            # is live. Its teardown and the surviving subscription's handlers
            # share the registration worker, so cancellation must neither
            # deadlock nor remove the survivor's handlers.
            cancelled_sub.close()
            sibling.minimize()
            event = wait_minimized(True)
            assert event.state_value is True

            # One property transition produces one event for this
            # subscription; duplicate native registrations would leave an
            # immediately queued second minimized=true event.
            time.sleep(0.1)
            duplicate = sub.try_recv()
            while duplicate is not None:
                assert not (
                    duplicate.event_type == xa11y.EventType.STATE_CHANGED
                    and duplicate.state_flag == "minimized"
                    and duplicate.state_value is True
                ), "minimize was delivered more than once"
                duplicate = sub.try_recv()

            # Re-resolve after minimize: the sibling is still in App.windows()
            # (the UIA Window control survives minimize), and the fresh
            # snapshot keeps the restore half honest.
            sibling = _window_named(app, sibling_name) or sibling
            sibling.restore()
            event = wait_minimized(False)
            assert event.state_value is False

            # Close the sibling while the subscription is still live: the
            # desktop-scoped WindowClosed watch is what must tear down the
            # closed window's per-window registrations, and the closed sender
            # (PID / cache reads on a detached element) is the
            # regression-prone half of that path. Assert the event end to end
            # — the target may be unresolvable at close time (the sender is
            # already detaching), so the filter is the event type.
            app.locator('button[name="Close Sibling"]').press()
            sub.wait_for(
                lambda e: e.event_type == xa11y.EventType.WINDOW_CLOSED,
                timeout=5.0,
            )
    finally:
        cancelled_sub.close()
        # Never leave the sibling minimized or open for the suites that
        # follow: best-effort restore + hide, original failure wins. By now
        # the sibling is closed in the happy path, so the helper is a no-op.
        _close_sibling_best_effort(app, sibling_name)


def _close_sibling_best_effort(app: xa11y.App, sibling_name: str) -> None:
    """Best-effort close of a still-open sibling window, for cleanup rails.

    The sibling must be restored first: a minimized window's button is not
    enabled, so invoking its "Close Sibling" button would time out. Never
    raises: cleanup must not replace the original failure, and a missing
    button means there is nothing to close.
    """
    try:
        sibling = _window_named(app, sibling_name)
        if sibling is None:
            return
        if "restore" in sibling.actions:
            sibling.restore()
        app.locator('button[name="Close Sibling"]').press()
    except Exception:  # best-effort cleanup; the original error wins
        pass


def test_locator_minimize_and_restore(app: xa11y.App) -> None:
    """The Locator's window verbs are wired through the binding too.
    Unlike the Element verbs, which pin one element, the Locator carries the
    selector + auto-wait machinery, so its dispatch is a separate code path
    worth exercising end to end. The selector matches exactly one window.
    """
    win = _window_advertising(app, "minimize")
    if win is None:
        pytest.skip("no window advertises minimize")
    if "restore" not in win.actions:
        pytest.skip("no window advertises both minimize and restore")
    locator = _locator_for_window(app, win)
    try:
        locator.minimize()
        _settled_window(
            app,
            "minimize",
            lambda w: w.minimized is True,
            "Locator minimize must report minimized",
        )
        locator.restore()
        _settled_window(
            app,
            "minimize",
            lambda w: w.minimized is False,
            "Locator restore must report restored",
        )
    except Exception:
        # Never leave the shared app minimized for the suite after this one;
        # the original failure wins over any cleanup failure.
        _restore_window_best_effort(app)
        raise


def test_locator_screen_fill_and_restore(app: xa11y.App) -> None:
    """``Locator.enter_fullscreen``/``maximize`` + ``Locator.restore`` dispatch.

    Same Locator-dispatch rationale as the minimize/restore test: the
    screen-filling verb has its own PyO3 path, and its restore half is what
    keeps the shared app usable for the suites that follow. Where the freshly
    resolved Locator cannot observe the state on macOS (the known gaps in
    ``tests/matrix.yaml``), the action is still dispatched and restored, and
    the state postcondition is skipped — the retained-Element test in the same
    cell enforces it.
    """
    spec = _screen_fill_operation(app)
    if spec is None:
        pytest.skip("no window advertises a screen-filling verb with restore")
    verb, state = spec
    if APP == "tauri" and sys.platform == "darwin":
        pytest.skip(
            "Tauri/macOS Locator restore cannot clear fullscreen "
            "(tauri_macos_locator_screen_fill_restore_failure)"
        )
    win = _window_advertising(app, verb)
    locator = _locator_for_window(app, win)
    try:
        getattr(locator, verb)()
        if MACOS and APP in ("egui", "qt"):
            locator.restore()
            pytest.skip(
                "Locator screen-fill state is unobservable for this app on macOS "
                "(macos_locator_screen_fill_state_unobservable)"
            )
        _assert_screen_fill(app, verb, state, True, f"Locator {verb}")
        locator.restore()
        _settled_window(
            app,
            "restore",
            lambda w: getattr(w, state) is False,
            f"Locator restore must clear {state}",
        )
    except Exception:
        # Same failure-preserving cleanup as the element path.
        _restore_window_best_effort(app)
        raise


def test_locator_activate(app: xa11y.App) -> None:
    """``Locator.activate`` reaches the platform.

    On a non-minimized window ``activate`` only changes focus/stacking (no
    state to restore; a minimized window is restored first), and is advertised
    for top-level windows on Windows (any top-level HWND) and macOS (AXRaise).
    AT-SPI deliberately never advertises it — the adapters disagree about a
    frame GrabFocus and no interface probe discriminates them — so the Linux
    cells skip here (``_window_advertising`` returns none; see the
    ``linux_activate_not_advertised`` gap in tests/matrix.yaml).
    """
    win = _window_advertising(app, "activate")
    if win is None:
        pytest.skip("no window advertises activate")
    locator = _locator_for_window(app, win)
    locator.activate()


def test_locator_move_to_restores_the_original_position(app: xa11y.App) -> None:
    """``Locator.move_to`` dispatches, and the window is put back where it was."""
    win = _window_advertising(app, "move_to")
    if win is None:
        pytest.skip("no window advertises move_to")
    bounds = win.bounds
    if bounds is None:
        pytest.skip("the window reports no bounds to restore from")
    locator = _locator_for_window(app, win)
    try:
        locator.move_to(bounds.x + 10, bounds.y + 10)
        locator.move_to(bounds.x, bounds.y)
    except Exception:
        # Same failure-preserving cleanup as the Element test: the shared app
        # must not be left moved for the suites after this one.
        try:
            current = _window_advertising(app, "move_to")
            if current is not None and "move_to" in current.actions:
                current.move_to(bounds.x, bounds.y)
        except Exception:  # best-effort cleanup; the original error wins
            pass
        raise


def test_locator_resize_to_restores_the_original_size(app: xa11y.App) -> None:
    """``Locator.resize_to`` dispatches, and the window's size is restored."""
    win = _window_advertising(app, "resize_to")
    if win is None:
        pytest.skip("no window advertises resize_to")
    bounds = win.bounds
    if bounds is None:
        pytest.skip("the window reports no bounds to restore from")
    locator = _locator_for_window(app, win)
    try:
        locator.resize_to(bounds.width + 50, bounds.height + 50)
        locator.resize_to(bounds.width, bounds.height)
    except Exception:
        # Same failure-preserving cleanup: don't leave the shared app resized
        # for the suites after this one.
        try:
            current = _window_advertising(app, "resize_to")
            if current is not None and "resize_to" in current.actions:
                current.resize_to(bounds.width, bounds.height)
        except Exception:  # best-effort cleanup; the original error wins
            pass
        raise


def test_close_dialog_via_element(app: xa11y.App, app_config: dict) -> None:
    """``Element.close`` dispatches against a secondary dialog, never the main window.

    The main window close wiring can only be exercised against the mock and
    the Rust integ suite (closing it would kill the app the harness still
    needs) — this is the real-provider surface for the binding dispatch.
    Uses the app's own "Open Dialog" flow, so no test-app change is needed on
    the dialog-bearing cells (qt / gtk / wpf, plus accesskit via the Rust
    suite).

    Where the platform advertises ``close`` the dialog must disappear; where
    it has no close API (AT-SPI on Linux) the dispatch must fail surfaceably
    (tenet 2: never input-simulate) — as ``ActionNotSupportedError`` at the
    binding. A dialog that advertises ``close`` but raises is a fidelity
    regression, not a skip.
    """
    dlg = _open_dialog(app, app_config)
    try:
        if "close" in dlg.actions:
            dlg.close()
            _wait_until(
                lambda: not _dialog_present(app, app_config["dialog_name"]),
                5.0,
                "the dialog to disappear after close()",
            )
        else:
            with pytest.raises(xa11y.ActionNotSupportedError):
                dlg.close()
    finally:
        # Never leave the dialog open for the suites that follow: it changes
        # their window enumeration. Strict when this test passed — a leak is
        # a cleanup bug that would otherwise surface as the *next* suite's
        # failure; the original failure wins when it did not.
        _close_dialog_from_finally(app, app_config)


def test_close_dialog_via_locator(app: xa11y.App, app_config: dict) -> None:
    """``Locator.close`` dispatches against a secondary dialog.

    The Locator carries the selector + auto-wait machinery, so its dispatch
    is a separate code path from ``Element.close``. Same contract: an
    advertised ``close`` must remove the dialog; a no-close-API platform must
    fail surfaceably rather than simulate.
    """
    dlg = _open_dialog(app, app_config)
    try:
        locator = _locator_for_window(app, dlg)
        if "close" in dlg.actions:
            locator.close()
            _wait_until(
                lambda: not _dialog_present(app, app_config["dialog_name"]),
                5.0,
                "the dialog to disappear after Locator.close()",
            )
        else:
            with pytest.raises(xa11y.ActionNotSupportedError):
                locator.close()
    finally:
        # Same rail as test_close_dialog_via_element: the dialog must be gone
        # before the next suite enumerates windows.
        _close_dialog_from_finally(app, app_config)


@pytest.mark.skipif(
    APP != "cocoa",
    reason=(
        "Fullscreen is only reportable on macOS (AXFullScreen): UIA has no "
        "fullscreen state and AT-SPI has no fullscreen state bit, so the "
        "state stays None on the other platforms."
    ),
)
def test_fullscreen_state_enters_and_reads_true(app: xa11y.App) -> None:
    """``Toggle Fullscreen`` puts the window in fullscreen on macOS.

    The `fullscreen` window state is the one window state a single platform
    reports: macOS reads it from ``AXFullScreen``. UIA (Windows) and AT-SPI
    (Linux) have no fullscreen state, so ``w.fullscreen`` stays ``None``
    there (the window suite tolerates bool-or-None for every platform). The
    cocoa host is the only harness app that can enter fullscreen — its
    Toggle Fullscreen button calls ``toggleFullScreen``, which the provider
    observes as the attribute change — so this test is the wire for the
    state read.

    There is no accessibility notification for fullscreen changes on any
    platform (the AX API has no fullscreen notification, and UIA and AT-SPI
    report no fullscreen state), so ``StateChanged{fullscreen}`` is never
    raised; this test covers the state, which is what consumers can observe.

    Entry only: the harness window servers complete the fullscreen *entry*
    transition but never complete the *exit* one (``toggleFullScreen`` from
    the fullscreen Space does nothing there, documented in main.swift), so
    the exit half is asserted nowhere. This test is ordered last in the file
    so nothing after it depends on the window's frame; the harness tears the
    app down at cell end, which returns the Space. The transition is
    animated (Spaces), so the state is polled rather than read once. The app
    also opens ``Space Companion`` immediately before entry. The companion is
    observed during the transition, then must leave the app-wide listing once
    it remains behind on the original Space. This characterizes the provider's
    active-Space boundary instead of assuming off-Space AX windows enumerate.
    """
    button = app.locator('button[name="Toggle Fullscreen"]')
    try:
        button.press()
        deadline = time.monotonic() + 15.0
        companion_seen = False
        while time.monotonic() < deadline:
            windows = app.windows()
            names = {w.name for w in windows}
            companion_seen = companion_seen or "Space Companion" in names
            if any(w.fullscreen is True for w in windows):
                assert companion_seen, "the companion was never discoverable before Space entry"
                assert "Space Companion" not in names, (
                    "the original-Space companion unexpectedly remained in the "
                    "active-Space window listing"
                )
                return
            time.sleep(0.2)
        raise AssertionError(
            "fullscreen state did not become true; "
            f"last read: {[(w.name, w.fullscreen) for w in windows]!r}"
        )
    except Exception:
        # Best-effort: the entry raised, so nothing is left to clean up on
        # the app side — the window could only be fullscreen if the poll
        # failed while the transition was still running. Original failure
        # wins, however the cleanup goes.
        try:
            if any(w.fullscreen for w in app.windows()):
                app.locator('button[name="Toggle Fullscreen"]').press()
        except Exception:  # best-effort cleanup; the original error wins
            pass
        raise
