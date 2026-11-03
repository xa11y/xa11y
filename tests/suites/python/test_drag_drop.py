"""A synthesized drag delivers a real AppKit drop (macOS, issue #515).

The Cocoa fixture's "Drag Source" starts an NSDraggingSession on the first
dragged event, and its "Drop Target" counts string drops in its value. AppKit
starts the session only after the button has been held briefly, and the target
registers the drop only if the pointer rests on it before release, which is
what the macOS backend's hold pauses provide.
"""

from __future__ import annotations

import subprocess
import sys
import time

import pytest

import xa11y


def _drops(app: xa11y.App) -> int:
    value = app.locator('[name="Drop Target"]').element().value or ""
    assert value.startswith("drops: "), f"unexpected Drop Target value {value!r}"
    return int(value.removeprefix("drops: "))


def _usable_area() -> tuple[float, float, float, float] | str:
    """The main display's ``visibleFrame`` as ``(left, top, right, bottom)``.

    Top-left origin, in points, like element bounds. ``visibleFrame`` excludes
    the menu bar and the Dock, which AppKit lets a window extend under: a
    pointer event posted there reaches the Dock, not the window. Read through
    JXA, which needs no Automation consent. A string instead when it cannot
    be read, so the diagnosis says why.
    """
    script = (
        'ObjC.import("AppKit"); var s = $.NSScreen.mainScreen; '
        "var f = s.frame; var v = s.visibleFrame; "
        '[f.size.height, v.origin.x, v.origin.y, v.size.width, v.size.height].join(",")'
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
        screen_h, x, y, w, h = (float(v) for v in out.stdout.split(","))
    except ValueError:
        return f"<unavailable: rc={out.returncode} stdout={out.stdout!r} stderr={out.stderr!r}>"
    # AppKit's origin is bottom-left; flip to the top-left origin bounds use.
    return (x, screen_h - (y + h), x + w, screen_h - y)


def _centre_inside(bounds: xa11y.Rect, area: tuple[float, float, float, float]) -> bool:
    cx = bounds.x + bounds.width / 2
    cy = bounds.y + bounds.height / 2
    left, top, right, bottom = area
    return left <= cx < right and top <= cy < bottom


# The drag is synthesized, and the harness runs the Cocoa app in the
# background: with Finder frontmost, AppKit never started a drag session.
@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
def test_cocoa_drag_delivers_a_drop(app_name, app):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the AppKit drag-and-drop fixture belongs to the macOS Cocoa cell")

    source = app.locator('[name="Drag Source"]').element()
    target = app.locator('[name="Drop Target"]').element()
    before = _drops(app)

    # A drag whose end points sit under the Dock never reaches the window, and
    # would read as a broken drag rather than a misplaced fixture.
    area = _usable_area()
    assert not isinstance(area, str), f"could not read the usable display area: {area}"
    for label, element in (("source", source), ("target", target)):
        assert _centre_inside(element.bounds, area), (
            f"drag {label} centre lies outside the display's usable area "
            f"(visibleFrame, top-left origin) {area}: {label} bounds {element.bounds}; "
            f"source {source.bounds}, target {target.bounds}"
        )

    xa11y.input_sim().drag(source, target, duration=0.3)

    deadline = time.monotonic() + 5.0
    after = before
    while time.monotonic() < deadline:
        after = _drops(app)
        if after > before:
            break
        time.sleep(0.1)
    # Each fixture reports, through its description, which steps of the drag
    # AppKit saw, so a failure names the stage that broke.
    source_stages = app.locator('[name="Drag Source"]').element().description
    target_stages = app.locator('[name="Drop Target"]').element().description
    assert after == before + 1, (
        f"drag from {source.bounds} to {target.bounds} delivered {after - before} "
        f"drops; source saw [{source_stages}], target saw [{target_stages}]"
    )
