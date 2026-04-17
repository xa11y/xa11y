"""Tests for Linux/Chromium accessibility-not-enabled detection.

Without `--force-renderer-accessibility`, an Electron app's window has no
descendants. xa11y must raise `AccessibilityNotEnabledError` so callers can
distinguish "this app doesn't have a button" from "the renderer accessibility
bridge isn't enabled".
"""

from __future__ import annotations

import xa11y


def test_no_flag_window_children_raises(electron_app_no_flag):
    """frame.children() on a Chromium app without the flag must raise."""
    app = electron_app_no_flag
    windows = app.children()
    assert len(windows) == 1, f"expected exactly one window, got {len(windows)}"
    window = windows[0]
    try:
        kids = window.children()
    except xa11y.AccessibilityNotEnabledError as e:
        msg = str(e).lower()
        assert "force-renderer-accessibility" in msg, (
            f"error message should mention the flag, got: {e}"
        )
        return
    raise AssertionError(
        f"expected AccessibilityNotEnabledError, got {len(kids)} children: {kids}"
    )


def test_no_flag_locator_raises(electron_app_no_flag):
    """Searching descendants on a Chromium app without the flag must raise."""
    app = electron_app_no_flag
    try:
        app.locator("button").elements()
    except xa11y.AccessibilityNotEnabledError as e:
        assert "force-renderer-accessibility" in str(e).lower()
        return
    raise AssertionError("expected AccessibilityNotEnabledError on locator query")


def test_with_flag_window_has_children(electron_app_with_flag):
    """With the flag, the window subtree is populated normally."""
    app = electron_app_with_flag
    windows = app.children()
    assert len(windows) == 1
    window = windows[0]
    kids = window.children()
    assert len(kids) > 0, "window should have descendants when flag is set"


def test_with_flag_buttons_visible(electron_app_with_flag):
    """With the flag, the OK and Cancel buttons are reachable via locator."""
    app = electron_app_with_flag
    buttons = app.locator("button").elements()
    names = {b.name for b in buttons if b.name}
    assert "OK" in names, f"OK button not found; names={names}"
    assert "Cancel" in names, f"Cancel button not found; names={names}"
