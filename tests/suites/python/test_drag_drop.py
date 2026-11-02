"""A synthesized drag delivers a real AppKit drop (macOS, issue #515).

The Cocoa fixture's "Drag Source" starts an NSDraggingSession on the first
dragged event, and its "Drop Target" counts string drops in its value. AppKit
starts the session only after the button has been held briefly, and the target
registers the drop only if the pointer rests on it before release, which is
what the macOS backend's hold pauses provide.
"""

from __future__ import annotations

import sys
import time

import pytest

import xa11y


def _drops(app: xa11y.App) -> int:
    value = app.locator('[name="Drop Target"]').element().value or ""
    assert value.startswith("drops: "), f"unexpected Drop Target value {value!r}"
    return int(value.removeprefix("drops: "))


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

    xa11y.input_sim().drag(source, target, duration=0.3)

    deadline = time.monotonic() + 5.0
    after = before
    while time.monotonic() < deadline:
        after = _drops(app)
        if after > before:
            break
        time.sleep(0.1)
    assert after == before + 1, (
        f"drag from {source.bounds} to {target.bounds} delivered {after - before} "
        "drops; the pointer moved but AppKit never completed a drag session onto "
        "the target"
    )
