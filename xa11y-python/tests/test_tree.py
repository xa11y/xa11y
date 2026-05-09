"""Tests for lazy navigation: children(), parent(), tree(), dump(), query via locator."""

import pytest
import xa11y

# ── Element.tree() ───────────────────────────────────────────────────────────


def test_tree_root_node(test_app):
    node = test_app.element().tree()
    assert node["role"] == "application"
    assert node["name"] == "TestApp"
    assert node["value"] is None


def test_tree_full_subtree_has_descendants(test_app):
    node = test_app.element().tree()
    assert len(node["children"]) == 1
    win = node["children"][0]
    assert win["role"] == "window"
    assert len(win["children"]) == 2


def test_tree_max_depth_zero_no_children(test_app):
    node = test_app.element().tree(max_depth=0)
    assert node["role"] == "application"
    assert node["children"] == []


def test_tree_max_depth_one_stops_at_children(test_app):
    node = test_app.element().tree(max_depth=1)
    assert len(node["children"]) == 1
    assert node["children"][0]["children"] == []


def test_tree_value_included(test_app):
    node = test_app.descendant("text_field").element().tree(max_depth=0)
    assert node["value"] == "hello"


def test_tree_leaf_has_empty_children(test_app):
    node = test_app.descendant('button[name="Back"]').element().tree()
    assert node["role"] == "button"
    assert node["children"] == []


# ── Element.dump() ───────────────────────────────────────────────────────────


def test_dump_returns_string(test_app):
    result = test_app.element().dump()
    assert isinstance(result, str)


def test_dump_contains_role_and_name(test_app):
    result = test_app.element().dump()
    assert 'application "TestApp"' in result


def test_dump_is_indented(test_app):
    result = test_app.element().dump()
    assert '  window "Main Window"' in result


def test_dump_max_depth_zero_is_one_line(test_app):
    result = test_app.element().dump(max_depth=0)
    lines = [line for line in result.splitlines() if line.strip()]
    assert len(lines) == 1
    assert "application" in lines[0]


def test_dump_includes_value(test_app):
    result = test_app.descendant("text_field").element().dump(max_depth=0)
    assert 'value="hello"' in result


# ── Locator.tree() / Locator.dump() ─────────────────────────────────────────


def test_locator_tree_shorthand(test_app):
    node = test_app.descendant("window").tree(max_depth=0)
    assert node["role"] == "window"
    assert node["name"] == "Main Window"
    assert node["children"] == []


def test_locator_dump_shorthand(test_app):
    result = test_app.descendant("window").dump(max_depth=0)
    assert 'window "Main Window"' in result


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
