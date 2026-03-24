"""Tests for Tree class: metadata, navigation, queries, iteration, dunders."""

import pytest
import xa11y

# ── Metadata ─────────────────────────────────────────────────────────────────


def test_tree_app_name(tree):
    assert tree.app_name == "TestApp"


def test_tree_pid(tree):
    assert tree.pid == 1234


def test_tree_screen_size(tree):
    assert tree.screen_size == (1920, 1080)


# ── Root ─────────────────────────────────────────────────────────────────────


def test_tree_root_is_application(tree):
    root = tree.root
    assert root.role == "application"
    assert root.name == "TestApp"


def test_tree_root_depth_zero(tree):
    assert tree.root.depth == 0


# ── Navigation (via Node.children / Node.parent) ────────────────────────────


def test_children_of_root(tree):
    children = tree.root.children
    assert len(children) == 1
    assert children[0].role == "window"
    assert children[0].name == "Main Window"


def test_children_of_window(tree):
    window = tree.root.children[0]
    children = window.children
    assert len(children) == 2
    assert children[0].role == "toolbar"
    assert children[1].role == "group"


def test_children_of_leaf(tree):
    buttons = tree.query("button")
    back = next(b for b in buttons if b.name == "Back")
    assert back.children == []


def test_parent_of_root_is_none(tree):
    assert tree.root.parent is None


def test_parent_of_button(tree):
    buttons = tree.query("button")
    back = next(b for b in buttons if b.name == "Back")
    parent = back.parent
    assert parent is not None
    assert parent.role == "toolbar"
    assert parent.name == "Navigation"


def test_parent_child_roundtrip(tree):
    window = tree.root.children[0]
    toolbar = window.children[0]
    assert toolbar.parent.name == "Main Window"


def test_deep_graph_traversal(tree):
    """Verify node.children[i].children[j] style navigation works."""
    root = tree.root
    toolbar = root.children[0].children[0]
    assert toolbar.role == "toolbar"
    back = toolbar.children[0]
    assert back.name == "Back"
    assert back.parent.parent.role == "window"


# ── Query ────────────────────────────────────────────────────────────────────


def test_query_by_role(tree):
    buttons = tree.query("button")
    assert len(buttons) == 2
    names = {b.name for b in buttons}
    assert names == {"Back", "Forward"}


def test_query_by_name(tree):
    results = tree.query('[name="Search"]')
    assert len(results) == 1
    assert results[0].role == "text_field"


def test_query_by_name_contains(tree):
    results = tree.query('[name*="Item"]')
    assert len(results) == 3  # "Items" list + "Item 1" + "Item 2"


def test_query_descendant_combinator(tree):
    results = tree.query("toolbar button")
    assert len(results) == 2


def test_query_child_combinator(tree):
    results = tree.query("toolbar > button")
    assert len(results) == 2


def test_query_no_match(tree):
    results = tree.query("menu")
    assert results == []


def test_query_invalid_selector(tree):
    with pytest.raises(xa11y.InvalidSelectorError):
        tree.query("[[[invalid")


# ── find_by_role / find_by_name ──────────────────────────────────────────────


def test_find_by_role(tree):
    items = tree.find_by_role("list_item")
    assert len(items) == 2
    assert {i.name for i in items} == {"Item 1", "Item 2"}


def test_find_by_role_unknown(tree):
    with pytest.raises(ValueError, match="Unknown role"):
        tree.find_by_role("nonexistent_role")


def test_find_by_name(tree):
    results = tree.find_by_name("item")
    assert len(results) == 3  # "Items" list + "Item 1" + "Item 2"


def test_find_by_name_case_insensitive(tree):
    results = tree.find_by_name("SEARCH")
    assert len(results) == 1
    assert results[0].role == "text_field"


def test_find_by_name_no_match(tree):
    assert tree.find_by_name("zzz_no_match") == []


# ── Dunder protocols ─────────────────────────────────────────────────────────


def test_tree_len(tree):
    assert len(tree) == 13


def test_tree_iter(tree):
    nodes = list(tree)
    assert len(nodes) == 13
    assert nodes[0].role == "application"
    assert nodes[-1].role == "list_item"


def test_tree_repr(tree):
    r = repr(tree)
    assert "TestApp" in r
    assert "1234" in r
    assert "13" in r


def test_tree_str_is_dump(tree):
    s = str(tree)
    assert "[0] application" in s
    assert "button" in s
    assert "Back" in s


def test_tree_dump(tree):
    d = tree.dump()
    lines = d.strip().split("\n")
    assert len(lines) == 13
    assert lines[0].startswith("[0]")
