"""An open context menu is reachable both by listing and by search (macOS).

Issue #518 reported that search could find an open context menu the
listing did not show. An ``NSView`` context menu on the macOS runner is not
an app-level ``AXMenu``, though: it opens inside the window's subtree
(``window > group > group > menu``). So the listing half is a bounded
``children()`` walk down from ``App.children()``, and it must reach the very
item search found.
"""

from __future__ import annotations

import subprocess
import sys
import time

import pytest

import xa11y

CONTEXT_ACTION = 'menu_item[name="xa11y Context Action"]'


# Bounds on the listing walk. The fixture's tree is a few hundred nodes; the
# caps only stop a regression (a cycle, a runaway subtree) from hanging the
# run, and hitting one is reported rather than read as "not found".
_WALK_MAX_DEPTH = 12
_WALK_MAX_NODES = 3000


def _signature(element: xa11y.Element) -> tuple:
    """Role, name and bounds of the element and each ancestor.

    ``Element`` has no identity comparison across routes, so two elements
    reached by different routes are the same node when this whole path agrees.
    """
    path = []
    node: xa11y.Element | None = element
    while node is not None and len(path) < _WALK_MAX_DEPTH + 2:
        b = node.bounds
        path.append(
            (node.role, node.name, None if b is None else (b.x, b.y, b.width, b.height))
        )
        node = node.parent()
    return tuple(path)


def _walk_to_context_item(app: xa11y.App) -> tuple[list[xa11y.Element], str]:
    """Breadth-first ``children()`` walk from ``App.children()``.

    Returns every ``menu_item`` named like the fixture's action that sits
    under a ``menu``, plus a description of how far the walk got (for the
    failure message).
    """
    item_name = "xa11y Context Action"
    frontier: list[tuple[xa11y.Element, int]] = [(c, 1) for c in app.children()]
    hits: list[xa11y.Element] = []
    visited = 0
    deepest = 0
    capped_depth = 0
    while frontier and visited < _WALK_MAX_NODES:
        node, depth = frontier.pop(0)
        visited += 1
        deepest = max(deepest, depth)
        if depth >= _WALK_MAX_DEPTH:
            capped_depth += 1
            continue
        children = node.children()
        for child in children:
            if (
                child.role == "menu_item"
                and child.name == item_name
                and node.role == "menu"
            ):
                hits.append(child)
            frontier.append((child, depth + 1))
    left = [f"{n.role} {n.name!r}@{d}" for n, d in frontier[:8]]
    summary = (
        f"visited {visited} nodes (cap {_WALK_MAX_NODES}), deepest level {deepest} "
        f"(cap {_WALK_MAX_DEPTH}), {capped_depth} nodes stopped at the depth cap, "
        f"{len(frontier)} left unvisited{': ' + ', '.join(left) if left else ''}"
    )
    return hits, summary


def _ancestry(element: xa11y.Element, limit: int = 12) -> str:
    """The element's parent chain up to the application, bounded."""
    chain = []
    node: xa11y.Element | None = element
    while node is not None and len(chain) < limit:
        chain.append(f"{node.role} {node.name!r}")
        try:
            node = node.parent()
        except xa11y.XA11yError as exc:
            chain.append(f"<parent() failed: {exc}>")
            break
    return " <- ".join(chain)


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


_WINDOW_TITLE_BAR = 28


def _locate_target(app: xa11y.App):
    """Re-read the label and its window; their bounds and the label's centre."""
    target = app.locator('[name="Context Target"]').element()
    window = app.windows()[0]
    bounds, win_bounds = target.bounds, window.bounds
    assert bounds is not None and win_bounds is not None, (
        f"no bounds to click: target={_rect(bounds)} window={_rect(win_bounds)}"
    )
    cx, cy = bounds.x + bounds.width / 2, bounds.y + bounds.height / 2
    return target, bounds, win_bounds, cx, cy


def _visible(win_bounds, desktop, x: float, y: float) -> bool:
    """Whether a point is in the window's content area and on the display."""
    in_window = (
        win_bounds.x <= x < win_bounds.x + win_bounds.width
        and win_bounds.y + _WINDOW_TITLE_BAR <= y < win_bounds.y + win_bounds.height
    )
    if not isinstance(desktop, tuple):
        return in_window
    left, top, right, bottom = desktop
    return in_window and left <= x < right and top <= y < bottom


# The right-click is a synthesized event: without the frontmost slot the
# click lands behind Finder and no menu opens.
@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
def test_cocoa_context_menu_is_listed_and_searchable(app_name, app):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the native context-menu fixture belongs to the macOS Cocoa cell")

    window = app.windows()[0]
    desktop = _desktop_bounds()
    # The fixture's document scrolls, and on a display shorter than the
    # window AppKit shrinks the window, so the label can sit outside the
    # visible viewport. Neither scroll_into_view (labels do not advertise
    # AXScrollToVisible) nor a pointer can be relied on to fix that, so the
    # document is scrolled semantically through the scroll bar's AXValue.
    # The window's own scroll view is its first child; its scroll bar is a
    # direct child of that, unlike the bars of the nested text and table views.
    scroll_bars = [
        bar
        for area in window.children()[:1]
        for bar in area.children()
        if bar.role == "scroll_bar"
    ]
    scroll_bar = scroll_bars[0] if scroll_bars else None
    original_scroll = scroll_bar.numeric_value if scroll_bar is not None else None
    original_origin = None
    tried: list[str] = []
    try:
        target, bounds, win_bounds, cx, cy = _locate_target(app)
        for value in (None, 0.5, 0.25, 0.75, 0.0, 1.0):
            if _visible(win_bounds, desktop, cx, cy):
                break
            if value is None or scroll_bar is None:
                tried.append(f"scroll={original_scroll!r}: centre ({cx}, {cy})")
                continue
            scroll_bar.set_numeric_value(value)
            time.sleep(0.2)
            target, bounds, win_bounds, cx, cy = _locate_target(app)
            tried.append(f"scroll={value}: centre ({cx}, {cy})")
        # A runner display smaller than the fixture window can still leave the
        # label below the screen edge. Bring it on screen by moving the
        # window, and put the window back afterwards.
        window = app.windows()[0]
        if not _visible(win_bounds, desktop, cx, cy) and (
            isinstance(desktop, tuple) and "move_to" in window.actions
        ):
            left, top, right, bottom = desktop
            dx = max(0.0, left + 40 - cx) - max(0.0, cx - (right - 40))
            dy = max(0.0, top + 60 - cy) - max(0.0, cy - (bottom - 40))
            if dx or dy:
                original_origin = (int(win_bounds.x), int(win_bounds.y))
                window.move_to(int(win_bounds.x + dx), int(win_bounds.y + dy))
                time.sleep(0.3)
                target, bounds, win_bounds, cx, cy = _locate_target(app)
                window = app.windows()[0]
        geometry = (
            f"target={_rect(bounds)} actions={target.actions!r}; "
            f"window={_rect(win_bounds)} actions={window.actions!r}; "
            f"desktop={desktop!r}; moved from={original_origin!r}; "
            f"scroll attempts={tried!r}"
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
        while time.monotonic() < deadline:
            found = app.locator(CONTEXT_ACTION).count()
            if found:
                break
            time.sleep(0.1)

        assert found == 1, (
            f"search found {found} context-menu items after the right-click; "
            f"{geometry}; "
            f"children={[(c.role, c.name) for c in app.children()]!r}; "
            f"app tree:\n{app.dump(max_depth=3)}"
        )
        searched = app.locator(CONTEXT_ACTION).element()
        listed, walk = _walk_to_context_item(app)
        assert len(listed) == 1, (
            f"search found the open context menu's item, but a children() walk "
            f"from App.children() reached {len(listed)} of it; walk: {walk}; "
            f"children={[(c.role, c.name) for c in app.children()]!r}; "
            f"the searched item's ancestry: {_ancestry(searched)}"
        )
        # The listing is a navigable route to the same node search found.
        item = listed[0]
        assert _signature(item) == _signature(searched), (
            "the children() walk and search reached different context-menu items: "
            f"walk={_ancestry(item)}; search={_ancestry(searched)}"
        )
        # Not a window, so App.windows() keeps leaving it out.
        assert all(w.role in ("window", "dialog") for w in app.windows())
        item.press()
    finally:
        # Dismiss the menu even when discovery regresses, so one failure
        # cannot poison the rest of the shared Cocoa app run.
        # AXPress on the item runs its action but need not end the menu's
        # tracking loop: the next left mouse-down in the shared app is then
        # spent dismissing the menu and never reaches a view (the drag test,
        # issue #515, saw dragged and up events but no down). So whether or
        # not the press went through, close the menu if it is still open and
        # wait for it to go.
        try:
            if app.locator(CONTEXT_ACTION).count():
                xa11y.input_sim().press("Escape")
                deadline = time.monotonic() + 5.0
                while (
                    app.locator(CONTEXT_ACTION).count() and time.monotonic() < deadline
                ):
                    time.sleep(0.1)
        except xa11y.XA11yError:
            pass  # Preserve the test's own failure if the cleanup also fails.
        if scroll_bar is not None and original_scroll is not None:
            try:
                scroll_bar.set_numeric_value(original_scroll)
            except xa11y.XA11yError:
                pass  # Preserve the test's own failure if the restore also fails.
        if original_origin is not None:
            try:
                app.windows()[0].move_to(*original_origin)
            except xa11y.XA11yError:
                pass  # Preserve the test's own failure if the restore also fails.
