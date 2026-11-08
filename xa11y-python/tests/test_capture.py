"""Bounded capture (#482) through the Python binding, against the mock."""

from __future__ import annotations

import pytest
import xa11y
from xa11y._native import _make_test_app, _make_test_document_locator


def _app():
    return _make_test_app(False)


def test_a_capture_is_pre_order_with_indices_that_agree():
    cap = _app().capture()
    assert len(cap) == len(cap.nodes) > 1
    assert not cap.truncated
    assert cap.vanished == 0
    assert cap.started <= cap.finished
    root = cap.root()
    assert root.index == 0 and root.parent is None and root.depth == 0
    assert root.role == "application"
    for node in cap.nodes:
        assert node.children_count == len(node.children)
        assert not node.children_truncated
        assert node.errors == []
        for child in node.children:
            assert cap.nodes[child].parent == node.index
            assert child > node.index
    names = [n.name for n in cap.nodes]
    nav = names.index("Navigation")
    assert [n.name for n in cap.subtree(nav)] == ["Navigation", "Back", "Forward"]
    assert cap.subtree(len(cap)) == []


def test_the_node_limit_cuts_and_says_where():
    cap = _app().capture(max_nodes=3)
    assert len(cap) == 3
    assert cap.truncated.by_nodes and cap.truncated.any()
    cut = [n for n in cap.nodes if n.children_truncated]
    assert cut and all(n.children_count > len(n.children) for n in cut)
    assert "by_nodes=True" in repr(cap.truncated)


def test_the_depth_limit_cuts_and_keeps_the_count():
    cap = _app().capture(max_depth=1)
    assert cap.truncated.by_depth
    assert max(n.depth for n in cap.nodes) == 1
    window = next(n for n in cap.nodes if n.depth == 1)
    assert window.children == [] and window.children_count > 0


def test_a_captured_node_is_the_element_itself():
    cap = _app().capture()
    back = next(n for n in cap.nodes if n.name == "Back")
    assert isinstance(back.element, xa11y.Element)
    assert back.element == back.element
    back.element.press()


def test_relations_resolve_to_indices_inside_the_capture():
    window = _make_test_document_locator("window")
    cap = window.capture(detail={"relations"})
    field = next(n for n in cap.nodes if n.name == "Email")
    (label,) = field.relations["labelled_by"]
    assert isinstance(label, int)
    assert cap.nodes[label].name == "Email address"
    assert field.relations["controls"] == []
    alone = field.element.capture(detail=["relations"])
    (outside,) = alone.root().relations["labelled_by"]
    assert isinstance(outside, xa11y.Element)
    assert outside.name == "Email address"


def test_text_detail_reads_within_the_budget():
    from xa11y._native import _make_test_table_locator

    notes = _make_test_table_locator('text_area[name="Notes"]')
    full = notes.capture(detail=("text",)).root().text
    assert isinstance(full, xa11y.CapturedText)
    assert full.content == "Hello 😀 world\nsecond line"
    assert full.length == 26 and not full.truncated
    cut = notes.capture(detail={"text"}, max_text_bytes=8)
    assert cut.truncated.by_text
    assert cut.root().text.truncated
    assert notes.capture().root().text is None


def test_a_spent_time_budget_returns_a_partial_capture():
    cap = _app().capture(timeout=0)
    assert cap.truncated.by_time
    assert len(cap) == 1
    assert cap.root().children_count is None


@pytest.mark.parametrize(
    "kwargs, exc",
    [
        ({"max_nodes": 0}, xa11y.InvalidActionDataError),
        ({"detail": {"formatting"}}, ValueError),
        ({"detail": "text"}, ValueError),
        ({"timeout": -1}, ValueError),
        ({"timeout": float("nan")}, ValueError),
    ],
)
def test_bad_options_are_refused(kwargs, exc):
    with pytest.raises(exc):
        _app().capture(**kwargs)
