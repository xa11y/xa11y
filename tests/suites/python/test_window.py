"""Window-management coverage for the Python binding, against a real app.

Mirrors the Rust integ suite (`xa11y/tests/integ/windows.rs`) across the
actual provider/binding boundary. The Rust suite runs the deep per-platform
success/failure matrix; this file's job is the *surface* — that the Python
calls reach the provider and that its results come back shaped as the
binding promises (state getters present, ``None`` being the honest "this
platform cannot report the state").

Deliberately **non-mutating**: the harness runs python → js → cli suites
against one shared app instance, and window-state verbs (minimize / restore /
resize) churn the UIA/AX cache in a way that made the *next* suite's action
tests flaky (the egui-windows `02_actions` checkbox read-back regressed when
this file minimized/restored the window first). The mutating verbs run
through the binding in the `python-window` suite (test_window_mutating.py),
which the harness orders *after* every other suite, and through the CLI
suite's real-provider dispatch; the deep per-platform matrix stays in the
Rust integ suite, which owns its own app lifecycle and restore guards.
"""

from __future__ import annotations

import pytest


def test_app_windows_lists_the_main_window(app, app_name) -> None:
    """``App.windows()`` returns the app's top-level windows via the provider.

    Not every matrix app exposes a window-like element at all (the GTK app's
    AT-SPI tree starts at a plain group — no frame node), so an empty listing
    is the provider answering honestly rather than a test failure; the
    per-app matrix is the Rust integ suite's ground truth, this file's point
    is the boundary. The assertion is about the shape, not the exact set.
    """
    windows = app.windows()
    assert isinstance(windows, list)
    if not windows:
        pytest.skip(f"{app_name!r} exposes no window-like element")
    for w in windows:
        assert w.role, "a listed window always carries its role"
        # Window state getters are new public surface: they must be readable
        # end-to-end, and `None` is the documented "unknown / not a window".
        for name in ("minimized", "maximized", "fullscreen"):
            value = getattr(w, name)
            assert value is None or isinstance(value, bool), (name, value)


def test_window_raise_reaches_the_platform(app) -> None:
    """``raise_`` (Python's trailing-underscore rename for ``raise``).

    Only exercised on a window whose `actions` advertise ``raise`` — tenet 3:
    an action name reported by the platform must dispatch to the platform
    action, and a real window is what verifies the binding keeps that promise.
    Raise is focus-only, the least invasive window verb, so it stays in this
    suite; the mutating verbs (minimize/restore/resize) live in the
    window-mutating suites, which the harness orders after the action suites
    (`python js cli js-window python-window`), so a minimized or moved window
    cannot disturb the compatibility and action tests before them.
    """
    windows = [w for w in app.windows() if "raise" in w.actions]
    if not windows:
        pytest.skip("this app's windows advertise no raise action")
    windows[0].raise_()
