"""Tests for lazy navigation: children(), parent(), query via locator."""

import pytest
import xa11y

# ── Root ─────────────────────────────────────────────────────────────────────


def test_root_is_application(test_app):
    app = test_app.element()
    assert app.role == "application"
    assert app.name == "TestApp"


def test_root_pid(test_app):
    app = test_app.element()
    assert app.pid == 1234


# ── Navigation (via Element.children() / Element.parent()) ──────────────────


def test_children_of_root(test_app):
    app = test_app.element()
    children = app.children()
    assert len(children) == 1
    assert children[0].role == "window"
    assert children[0].name == "Main Window"


def test_children_of_window(test_app):
    app = test_app.element()
    window = app.children()[0]
    children = window.children()
    assert len(children) == 2
    assert children[0].role == "toolbar"
    assert children[1].role == "group"


def test_children_of_leaf(test_app):
    buttons = test_app.descendant("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    assert back.children() == []


def test_parent_of_root_is_none(test_app):
    app = test_app.element()
    assert app.parent() is None


def test_parent_of_button(test_app):
    buttons = test_app.descendant("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    parent = back.parent()
    assert parent is not None
    assert parent.role == "toolbar"
    assert parent.name == "Navigation"


def test_parent_child_roundtrip(test_app):
    app = test_app.element()
    window = app.children()[0]
    toolbar = window.children()[0]
    assert toolbar.parent().name == "Main Window"


def test_deep_graph_traversal(test_app):
    """Verify element.children()[i].children()[j] style navigation works."""
    app = test_app.element()
    toolbar = app.children()[0].children()[0]
    assert toolbar.role == "toolbar"
    back = toolbar.children()[0]
    assert back.name == "Back"
    assert back.parent().parent().role == "window"


# ── Query via locator ───────────────────────────────────────────────────────


def test_query_by_role(test_app):
    buttons = test_app.descendant("button").elements()
    assert len(buttons) == 2
    names = {b.name for b in buttons}
    assert names == {"Back", "Forward"}


def test_query_by_name(test_app):
    results = test_app.descendant('[name="Search"]').elements()
    assert len(results) == 1
    assert results[0].role == "text_field"


def test_query_by_name_contains(test_app):
    results = test_app.descendant('[name*="Item"]').elements()
    assert len(results) == 3  # "Items" list + "Item 1" + "Item 2"


def test_query_descendant_combinator(test_app):
    results = test_app.descendant("toolbar button").elements()
    assert len(results) == 2


def test_query_child_combinator(test_app):
    results = test_app.descendant("toolbar > button").elements()
    assert len(results) == 2


def test_query_no_match(test_app):
    results = test_app.descendant("menu").elements()
    assert results == []


def test_query_invalid_selector(test_app):
    with pytest.raises(xa11y.InvalidSelectorError):
        test_app.descendant("[[[invalid").elements()
