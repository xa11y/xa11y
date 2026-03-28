"""Tests for snapshot navigation: root, children, parent, query via locator."""

import pytest
import xa11y

# ── Root ─────────────────────────────────────────────────────────────────────


def test_root_is_application(tree):
    assert tree.role == "application"
    assert tree.name == "TestApp"


def test_root_pid(tree):
    assert tree.pid is None  # test tree nodes have pid=None


# ── Navigation (via Node.children / Node.parent) ────────────────────────────


def test_children_of_root(tree):
    children = tree.children
    assert len(children) == 1
    assert children[0].role == "window"
    assert children[0].name == "Main Window"


def test_children_of_window(tree):
    window = tree.children[0]
    children = window.children
    assert len(children) == 2
    assert children[0].role == "toolbar"
    assert children[1].role == "group"


def test_children_of_leaf(test_app):
    buttons = test_app.locator("button").nodes()
    back = next(b for b in buttons if b.name == "Back")
    assert back.children == []


def test_parent_of_root_is_none(tree):
    assert tree.parent is None


def test_parent_of_button(test_app):
    buttons = test_app.locator("button").nodes()
    back = next(b for b in buttons if b.name == "Back")
    parent = back.parent
    assert parent is not None
    assert parent.role == "toolbar"
    assert parent.name == "Navigation"


def test_parent_child_roundtrip(tree):
    window = tree.children[0]
    toolbar = window.children[0]
    assert toolbar.parent.name == "Main Window"


def test_deep_graph_traversal(tree):
    """Verify node.children[i].children[j] style navigation works."""
    toolbar = tree.children[0].children[0]
    assert toolbar.role == "toolbar"
    back = toolbar.children[0]
    assert back.name == "Back"
    assert back.parent.parent.role == "window"


# ── Query via locator ───────────────────────────────────────────────────────


def test_query_by_role(test_app):
    buttons = test_app.locator("button").nodes()
    assert len(buttons) == 2
    names = {b.name for b in buttons}
    assert names == {"Back", "Forward"}


def test_query_by_name(test_app):
    results = test_app.locator('[name="Search"]').nodes()
    assert len(results) == 1
    assert results[0].role == "text_field"


def test_query_by_name_contains(test_app):
    results = test_app.locator('[name*="Item"]').nodes()
    assert len(results) == 3  # "Items" list + "Item 1" + "Item 2"


def test_query_descendant_combinator(test_app):
    results = test_app.locator("toolbar button").nodes()
    assert len(results) == 2


def test_query_child_combinator(test_app):
    results = test_app.locator("toolbar > button").nodes()
    assert len(results) == 2


def test_query_no_match(test_app):
    results = test_app.locator("menu").nodes()
    assert results == []


def test_query_invalid_selector(test_app):
    with pytest.raises(xa11y.InvalidSelectorError):
        test_app.locator("[[[invalid").nodes()


# ── Node dunders ─────────────────────────────────────────────────────────────


def test_node_len_children_count(test_app):
    tree = test_app.nodes()
    assert len(tree) == 1  # one child (window)
    window = tree.children[0]
    assert len(window) == 2  # toolbar + group
    buttons = test_app.locator("button").nodes()
    back = next(b for b in buttons if b.name == "Back")
    assert len(back) == 0  # leaf
