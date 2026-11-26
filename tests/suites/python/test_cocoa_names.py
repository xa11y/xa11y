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


@pytest.mark.parametrize(
    ("selector", "expected"),
    [
        ('button[name="OK"], button[name="OK"]', 1),
        ('button[name="OK"], button[name="Cancel"], button[name="OK"]', 2),
        ('button:nth(1), button:nth(1)', 1),
        ('button:nth(1), button:nth(2)', 2),
        ('button:nth(10000), button[name="No such Cocoa control"]', 0),
        ('static_text[name="Team Lunch. 13 November 2026"]', 1),
    ],
)
def test_projected_query_agrees_with_element_resolution(app, selector, expected):
    locator = app.locator(selector)
    assert locator.count() == len(locator.elements()) == expected
    assert locator.exists() is (expected > 0)


def test_ordered_first_button_agrees_with_exhaustive_walk(app):
    buttons = app.locator("button")
    all_buttons = buttons.elements()
    assert len(all_buttons) > 1
    assert buttons.first().element().same_as(all_buttons[0])
    assert buttons.first().exists()

    late = app.locator('button[name="Remove Item"]')
    assert late.first().element().same_as(late.elements()[0])
    assert late.first().exists()
    assert not app.locator('button[name="No such Cocoa control"]').first().exists()


def test_projected_queries_read_fresh_names(app):
    ready = app.locator('static_text[name="Status: Ready"]')
    submitted = app.locator('static_text[name="Status: Submitted"]')
    submit = app.locator('button[name="Submit"]')
    starting, changed = (ready, submitted) if ready.exists() else (submitted, ready)
    assert starting.count() == 1
    assert not changed.exists()
    submit.press()
    try:
        changed.wait_visible(timeout=5.0)
        assert changed.count() == 1
        assert not starting.exists()
    finally:
        submit.press()
        starting.wait_visible(timeout=5.0)
