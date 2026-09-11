"""End-to-end shell-surface coverage requiring a native desktop fixture."""

from __future__ import annotations

import sys
import time

import pytest

import xa11y


def test_cocoa_status_item_menu_is_a_flyout(app_name, app):
    """An open, real NSStatusItem menu is discoverable outside the app tree."""
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the native NSStatusItem fixture belongs to the macOS Cocoa cell")

    app.locator('button[name="Open Status Menu"]').press()
    menu_closed = False
    try:
        deadline = time.monotonic() + 5.0
        flyouts = []
        while time.monotonic() < deadline:
            flyouts = [
                surface
                for surface in xa11y.ShellSurface.list()
                if surface.kind == "flyout" and surface.pid == app.pid
            ]
            if flyouts:
                break
            time.sleep(0.1)

        assert len(flyouts) == 1, (
            "the Cocoa fixture opened a native status menu, but no flyout was "
            f"reported for pid {app.pid}; surfaces={xa11y.ShellSurface.list()!r}"
        )

        action = flyouts[0].locator('menu_item[name="xa11y Status Action"]').element()
        assert action.visible is True
        action.press()
        menu_closed = True
    finally:
        # Dismiss the system-modal menu even when discovery itself regresses,
        # so one failure cannot poison the rest of the shared Cocoa app run.
        if not menu_closed:
            try:
                xa11y.input_sim().press("Escape")
            except xa11y.XA11yError:
                pass  # Preserve the discovery failure if cleanup also fails.
