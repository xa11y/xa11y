"""Names and descriptions of AppKit elements through macOS accessibility.

macOS only: the widgets are AppKit's.
"""

from __future__ import annotations

import sys

import pytest


@pytest.fixture(autouse=True)
def macos_cocoa_only(app_name):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("pins AppKit's AX behaviour; requires the Cocoa app on macOS")


def test_a_static_text_without_a_value_is_named_by_its_description(app):
    event = app.locator('static_text[name="Team Lunch. 13 November 2026"]').element()
    assert event.description == "This event belongs to calendar “Work”."
