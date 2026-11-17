"""Search from an application reaches what ``children()`` reaches (macOS).

The Cocoa app lists an accessibility child of the application that is neither
a window nor a menu, as AppKit's Touch Bar element does on a Mac that has one.
``children()`` leaves it out, so search from the application must too.
"""

from __future__ import annotations

import sys
from collections import Counter

import pytest

import xa11y

_WALK_MAX_NODES = 3000


@pytest.fixture(autouse=True)
def macos_cocoa_only(app_name):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the app-level fixture belongs to the macOS Cocoa app")


def _key(element: xa11y.Element) -> tuple:
    b = element.bounds
    return (
        element.role,
        element.name,
        None if b is None else (b.x, b.y, b.width, b.height),
    )


def _buttons_by_children(app: xa11y.App) -> Counter:
    frontier = list(app.children())
    found: Counter = Counter()
    visited = 0
    while frontier:
        assert visited < _WALK_MAX_NODES, "children() walk passed its node cap"
        node = frontier.pop(0)
        visited += 1
        if node.role == "button":
            found[_key(node)] += 1
        frontier.extend(node.children())
    return found


def test_search_from_the_app_finds_the_buttons_children_reach(app):
    by_children = _buttons_by_children(app)
    by_search = Counter(_key(e) for e in app.locator("button").elements())

    assert by_children, "the children() walk found no buttons"
    assert by_search == by_children, (
        f"search only: {by_search - by_children}; children() only: {by_children - by_search}"
    )
