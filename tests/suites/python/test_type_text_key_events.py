"""`type_text` reaches a control that reads key events (macOS).

The Cocoa test app's "Key Events" view keeps the first character of each
key event, as Calculator does. A string posted as one Unicode key event
typed only its first character there: "7*6=" arrived as "7".
"""

from __future__ import annotations

import sys
import time

import pytest

import xa11y

SETTLE_TIMEOUT = 3.0


@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
def test_cocoa_type_text_sends_each_character_as_a_key_event(app_name, app):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the key-event fixture belongs to the macOS Cocoa cell")

    view = app.locator('[name="Key Events"]')
    view.focus()
    view.wait_focused(timeout=SETTLE_TIMEOUT)
    xa11y.input_sim().type_text("7*6=é😀")

    deadline = time.monotonic() + SETTLE_TIMEOUT
    value = ""
    while time.monotonic() < deadline:
        value = view.element().value or ""
        if value == "7*6=é😀":
            return
        time.sleep(0.05)
    pytest.fail(
        f"Key Events kept {value!r}, not '7*6=é😀'. Only the first character "
        f"means the text arrived as one key event."
    )
