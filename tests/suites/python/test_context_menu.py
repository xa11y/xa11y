"""An open context menu is reachable both by listing and by search (macOS).

A macOS context menu is an app-level ``AXMenu`` child of the application,
never an entry in ``AXWindows``. Search walks ``AXChildren`` while the
application's listing read ``AXWindows`` alone, so a selector found a menu
that ``App.children()`` did not show (issue #518).
"""

from __future__ import annotations

import sys
import time

import pytest

import xa11y

CONTEXT_ACTION = 'menu_item[name="xa11y Context Action"]'


def _context_menus(app: xa11y.App) -> list[xa11y.Element]:
    """The app's listed top-level menus that hold the fixture's item."""
    return [
        child
        for child in app.children()
        if child.role == "menu" and child.locator(CONTEXT_ACTION).count() == 1
    ]


# The right-click is a synthesized event: without the frontmost slot the
# click lands behind Finder and no menu opens.
@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
def test_cocoa_context_menu_is_listed_and_searchable(app_name, app):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the native context-menu fixture belongs to the macOS Cocoa cell")

    target = app.locator('[name="Context Target"]').element()
    xa11y.input_sim().click(target, button="right")
    menu_closed = False
    try:
        deadline = time.monotonic() + 5.0
        found = 0
        listed: list[xa11y.Element] = []
        while time.monotonic() < deadline:
            found = app.locator(CONTEXT_ACTION).count()
            listed = _context_menus(app)
            if found and listed:
                break
            time.sleep(0.1)

        assert found == 1, (
            f"search found {found} context-menu items after the right-click; "
            f"children={[(c.role, c.name) for c in app.children()]!r}; "
            f"app tree:\n{app.dump(max_depth=3)}"
        )
        assert len(listed) == 1, (
            "search found the open context menu's item, but App.children() "
            f"lists {len(listed)} menus holding it; children="
            f"{[(c.role, c.name) for c in app.children()]!r}"
        )
        # The listing is a navigable route to the item, not just a node.
        item = listed[0].locator(CONTEXT_ACTION).element()
        # Not a window, so App.windows() keeps leaving it out.
        assert all(w.role in ("window", "dialog") for w in app.windows())
        item.press()
        menu_closed = True
    finally:
        # Dismiss the menu even when discovery regresses, so one failure
        # cannot poison the rest of the shared Cocoa app run.
        if not menu_closed:
            try:
                xa11y.input_sim().press("Escape")
            except xa11y.XA11yError:
                pass  # Preserve the discovery failure if cleanup also fails.
