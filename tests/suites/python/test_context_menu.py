"""An open context menu is reachable both by listing and by search (macOS).

A macOS context menu is an app-level ``AXMenu`` child of the application,
never an entry in ``AXWindows``. Search walks ``AXChildren`` while the
application's listing read ``AXWindows`` alone, so a selector found a menu
that ``App.children()`` did not show (issue #518).
"""

from __future__ import annotations

import subprocess
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


def _rect(r) -> str:
    return "None" if r is None else f"({r.x}, {r.y}, {r.width}x{r.height})"


def _desktop_bounds() -> tuple[int, int, int, int] | str:
    """The main display as ``(left, top, right, bottom)`` in points.

    Read from AppKit's ``NSScreen.mainScreen`` through JXA, which needs no
    Automation consent: a Finder Apple Event could raise a TCC dialog that
    takes the frontmost slot and swallows the right-click. On the
    single-display CI runner the main display holds the global origin, so its
    top-left is ``(0, 0)``. A string
    instead when it cannot be read, so the diagnosis says why.
    """
    script = (
        'ObjC.import("AppKit"); var f = $.NSScreen.mainScreen.frame; '
        'f.size.width + "," + f.size.height'
    )
    try:
        out = subprocess.run(
            ["osascript", "-l", "JavaScript", "-e", script],
            capture_output=True,
            check=False,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return f"<unavailable: {exc}>"
    try:
        width, height = (int(float(v)) for v in out.stdout.split(","))
    except ValueError:
        return f"<unavailable: stdout={out.stdout.strip()!r} stderr={out.stderr.strip()!r}>"
    return (0, 0, width, height)


def _contains(outer, x: float, y: float) -> bool:
    return (
        outer.x <= x < outer.x + outer.width and outer.y <= y < outer.y + outer.height
    )


# The right-click is a synthesized event: without the frontmost slot the
# click lands behind Finder and no menu opens.
@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
def test_cocoa_context_menu_is_listed_and_searchable(app_name, app):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the native context-menu fixture belongs to the macOS Cocoa cell")

    target = app.locator('[name="Context Target"]').element()
    # A pointer event lands wherever the label is drawn, so the label must be
    # inside the window's visible area, not scrolled out of the document view.
    if "scroll_into_view" in target.actions:
        target.scroll_into_view()
        target = app.locator('[name="Context Target"]').element()
    window = app.windows()[0]
    desktop = _desktop_bounds()
    original_origin = None
    menu_closed = False
    try:
        bounds, win_bounds = target.bounds, window.bounds
        assert bounds is not None and win_bounds is not None, (
            f"no bounds to click: target={_rect(bounds)} window={_rect(win_bounds)}"
        )
        cx, cy = bounds.x + bounds.width / 2, bounds.y + bounds.height / 2
        # A runner display smaller than the fixture window leaves part of it off
        # screen, and a click there reaches nothing. Bring the label on screen by
        # moving the window, and put the window back afterwards.
        if isinstance(desktop, tuple) and "move_to" in window.actions:
            left, top, right, bottom = desktop
            dx = max(0.0, left + 40 - cx) - max(0.0, cx - (right - 40))
            dy = max(0.0, top + 60 - cy) - max(0.0, cy - (bottom - 40))
            if dx or dy:
                original_origin = (int(win_bounds.x), int(win_bounds.y))
                window.move_to(int(win_bounds.x + dx), int(win_bounds.y + dy))
                time.sleep(0.3)
                target = app.locator('[name="Context Target"]').element()
                window = app.windows()[0]
                bounds, win_bounds = target.bounds, window.bounds
                assert bounds is not None and win_bounds is not None, (
                    f"no bounds to click after moving the window from "
                    f"{original_origin!r}: target={_rect(bounds)} "
                    f"window={_rect(win_bounds)} desktop={desktop!r}"
                )
                cx, cy = bounds.x + bounds.width / 2, bounds.y + bounds.height / 2
        geometry = (
            f"target={_rect(bounds)} actions={target.actions!r}; "
            f"window={_rect(win_bounds)} actions={window.actions!r}; "
            f"desktop={desktop!r}; moved from={original_origin!r}"
        )
        assert _contains(win_bounds, cx, cy), (
            f"the context target's centre ({cx}, {cy}) is outside its window: {geometry}"
        )
        if isinstance(desktop, tuple):
            left, top, right, bottom = desktop
            assert left <= cx < right and top <= cy < bottom, (
                f"the context target's centre ({cx}, {cy}) is off the display: {geometry}"
            )
        xa11y.input_sim().click(target, button="right")
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
            f"{geometry}; "
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
        if original_origin is not None:
            try:
                app.windows()[0].move_to(*original_origin)
            except xa11y.XA11yError:
                pass  # Preserve the test's own failure if the restore also fails.
