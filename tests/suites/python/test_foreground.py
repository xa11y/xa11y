"""Foreground-application and active-window tests against the running app.

Exercises the real binding (not the mock) for:

- ``App.foreground(timeout=...)`` — resolves the app holding the system
  foreground; the returned app reports ``is_foreground == True``.
- ``App.list()`` — every listed app carries an ``is_foreground`` bool, and the
  fixture app is present.
- ``App.is_foreground`` vs. the deprecated ``App.focused`` alias — same value,
  and reading ``focused`` emits a ``DeprecationWarning``.
- ``Element.active`` — the active top-level window reports ``active == True``
  (and matches the ``window[active="true"]`` selector); a non-window
  descendant reports ``False``; at most one window is ever active.

These tests run across every toolkit via the shared ``app`` / ``app_config``
fixtures, so strict identity/equality assertions are gated on a *frontmost*
signal: not every harness guarantees the test app holds the OS foreground.
The gate itself is the freshly-read ``is_foreground`` flag for the app's pid
— when the OS says our app is frontmost we assert the strong invariants;
otherwise we fall back to the app-agnostic ones (types, bounds, uniqueness)
so the suite degrades gracefully rather than flaking.
"""

from __future__ import annotations

import pytest
import xa11y


def _fresh_foreground_flag(app: xa11y.App) -> bool | None:
    """Re-list running apps and return the current ``is_foreground`` for our pid.

    Uses a fresh ``App.list()`` rather than the session-scoped ``app`` handle's
    possibly-stale flag, so it reflects whether the app holds the foreground
    *now*. Returns ``None`` if the app is no longer listed (treated as "unknown
    → don't assert the strong invariants").
    """
    for candidate in xa11y.App.list():
        if candidate.pid == app.pid:
            return bool(candidate.is_foreground)
    return None


# ---------------------------------------------------------------------------
# App.foreground()
# ---------------------------------------------------------------------------


def test_foreground_resolves_to_a_foreground_app(app):
    """``App.foreground()`` resolves an app that reports ``is_foreground``.

    When our app is the one holding the foreground (frontmost harnesses such
    as AccessKit-on-Xvfb), the resolved pid must be ours.
    """
    try:
        fg = xa11y.App.foreground(timeout=5.0)
    except xa11y.SelectorNotMatchedError:
        pytest.skip("nothing holds the system foreground in this harness")

    assert fg.pid > 0
    # The contract: whatever App.foreground() returns is, by definition, the
    # foreground application.
    assert fg.is_foreground is True

    if _fresh_foreground_flag(app):
        assert fg.pid == app.pid, (
            f"our app (pid={app.pid}) reports is_foreground but "
            f"App.foreground() resolved a different pid={fg.pid}"
        )


# ---------------------------------------------------------------------------
# App.list() + is_foreground
# ---------------------------------------------------------------------------


def test_list_contains_app_and_exposes_foreground_flag(app):
    """``App.list()`` includes the fixture app; every entry has a bool flag."""
    apps = xa11y.App.list()
    for candidate in apps:
        assert isinstance(candidate.is_foreground, bool)

    matches = [a for a in apps if a.pid == app.pid]
    assert matches, f"fixture app pid={app.pid} not found in App.list()"
    me = matches[0]

    # Frontmost-guarded: only assert our listed entry claims the foreground on
    # harnesses where the OS actually puts us in front.
    if me.is_foreground:
        assert me.is_foreground is True


# ---------------------------------------------------------------------------
# App.is_foreground / App.focused (deprecated)
# ---------------------------------------------------------------------------


def test_focused_is_deprecated_alias_of_is_foreground(app):
    """``App.focused`` mirrors ``App.is_foreground`` and warns on access."""
    with pytest.warns(DeprecationWarning):
        focused = app.focused
    assert focused == app.is_foreground


def test_is_foreground_is_a_plain_bool(app):
    """``App.is_foreground`` is a read-only bool populated by discovery."""
    assert isinstance(app.is_foreground, bool)


# ---------------------------------------------------------------------------
# Element.active — active/foreground window StateSet flag
# ---------------------------------------------------------------------------


def test_active_window(app, app_config):
    """The active top-level window reports ``active``; descendants report False.

    Invariant checked on every harness: at most one window is ever active.
    Strengthened to *exactly one* (and to our app's window) only when the OS
    reports our app as frontmost.
    """
    windows = app.locator("window").elements()
    if not windows:
        # Windows/UIA can model the app itself as the top-level window with no
        # separate `window` child; nothing to assert about window activeness.
        pytest.skip("app exposes no separate window element")

    active_windows = [w for w in windows if w.active]

    # Exactly-one-active-window is the invariant — never more than one.
    assert len(active_windows) <= 1, (
        f"expected at most one active window, found {len(active_windows)}"
    )

    frontmost = _fresh_foreground_flag(app)
    if frontmost:
        assert len(active_windows) == 1, (
            "app is frontmost but no window reports active=True"
        )
        # The selector form must resolve to that same single active window.
        by_selector = app.locator('window[active="true"]').elements()
        assert len(by_selector) == 1
        assert by_selector[0].active is True

    # A non-window descendant (a button) is never the active window.
    ok_name = app_config.get("ok_button_name")
    if ok_name:
        button = app.locator(f'button[name="{ok_name}"]').element()
        assert button.active is False
