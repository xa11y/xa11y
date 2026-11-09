"""Bounded capture (#482) against the real test app, on every platform cell.

Checks the properties every backend must give a capture: a consistent
pre-order layout, limits that cut where they say, and captured elements that
name exactly the node read.
"""

from __future__ import annotations

import time

import pytest

import xa11y


def test_capture_is_a_consistent_pre_order(app):
    cap = app.capture(max_nodes=500)
    assert len(cap) >= 2, "the app has at least one window"
    root = cap.root()
    assert root.index == 0 and root.parent is None and root.depth == 0
    assert root.role == "application"
    assert cap.started <= cap.finished <= time.time() + 1
    for node in cap.nodes:
        for child in node.children:
            assert cap.nodes[child].parent == node.index
            assert cap.nodes[child].depth == node.depth + 1
            assert child > node.index
        if node.children_count is not None and not node.children_truncated:
            # Every child read is kept, except those that vanished mid-read.
            assert len(node.children) <= node.children_count
        assert all(isinstance(e.error, xa11y.XA11yError) for e in node.errors)
    sub = cap.subtree(cap.nodes[1].index)
    assert sub[0].index == 1
    assert all(n.depth > cap.nodes[1].depth for n in sub[1:])


def test_the_node_limit_keeps_the_top_of_the_tree(app):
    full = app.capture(max_depth=3, max_nodes=5000)
    if len(full) <= 5:
        return
    cut = app.capture(max_depth=3, max_nodes=5)
    assert len(cut) == 5
    assert cut.truncated.by_nodes
    # Breadth-first: the windows (depth 1) are all present before any depth 2.
    windows = [n for n in full.nodes if n.depth == 1]
    if len(windows) < 4:
        assert len([n for n in cut.nodes if n.depth == 1]) == len(windows)
    marked = [n for n in cut.nodes if n.children_truncated]
    assert marked, "the node that lost children says so"


def test_the_depth_limit_cuts_with_the_real_child_count(app):
    cap = app.capture(max_depth=1)
    assert max(n.depth for n in cap.nodes) <= 1
    for window in (n for n in cap.nodes if n.depth == 1):
        assert window.children == []
        if window.children_count:
            assert window.children_truncated and cap.truncated.by_depth


def test_a_captured_element_is_the_node_read(app):
    button = app.locator("button").elements()[0]
    cap = app.capture(max_nodes=20000)
    if cap.truncated.by_nodes:
        pytest.skip("the app tree is larger than the capture limit")
    matches = [n for n in cap.nodes if n.element.same_as(button)]
    assert len(matches) == 1, "the capture holds the node the locator found, once"
    captured = matches[0].element
    assert captured.role == "button"
    captured.check_alive()


def test_text_detail_reads_document_text(app):
    cap = app.capture(max_nodes=2000, detail={"text"}, max_text_bytes=4096)
    texts = [n.text for n in cap.nodes if n.text is not None]
    for text in texts:
        assert len(text.content.encode()) <= 4096
        assert text.truncated == (
            len(text.content.encode("utf-16-le")) // 2 < text.length
        )
    assert sum(len(t.content.encode()) for t in texts) <= 4096


_BACKENDS = {"linux": {"atspi"}, "darwin": {"ax"}, "win32": {"uia", "ia2"}}


def test_a_capture_names_its_backend_and_counts_its_reads(app):
    import sys

    cap = app.capture(max_nodes=500)
    assert cap.backend in _BACKENDS[sys.platform], cap.backend
    stats = cap.stats
    # Structure only: an identity read per node (plus any that vanished or
    # closed a cycle and were dropped), children read on each node the walk
    # expanded, and nothing else.
    assert stats.identity_reads >= len(cap)
    assert 1 <= stats.children_reads <= len(cap)
    assert stats.text_reads == stats.formatting_reads == stats.relation_reads == 0
    assert stats.text_bytes == 0


def test_capture_cost_grows_with_what_it_reads(app):
    small = app.capture(max_nodes=10)
    large = app.capture(max_nodes=2000)
    assert len(small) <= 10
    assert small.stats.identity_reads <= large.stats.identity_reads
    # Reported, not asserted against a threshold: CI machines vary too much.
    print(
        f"capture latency: {len(small)} nodes in "
        f"{(small.finished - small.started) * 1000:.1f} ms, {len(large)} nodes in "
        f"{(large.finished - large.started) * 1000:.1f} ms"
    )


def test_formatting_detail_runs_lie_within_the_text(app):
    cap = app.capture(max_nodes=2000, detail={"formatting"}, max_runs=256)
    texts = [n.text for n in cap.nodes if n.text is not None]
    runs = [r for t in texts for r in (t.runs or [])]
    assert len(runs) <= 256
    assert cap.stats.formatting_reads == len(runs)
    assert cap.truncated.by_formatting == any(t.runs_truncated for t in texts)
    for text in texts:
        for run in text.runs or []:
            assert 0 <= run.range.start <= run.range.end <= text.length


def test_a_cancel_from_another_thread_stops_the_capture(app):
    import threading

    token = xa11y.CancelToken()
    threading.Timer(0.0, token.cancel).start()
    time.sleep(0.05)
    cap = app.capture(max_nodes=20000, cancel=token)
    assert token.is_cancelled()
    assert cap.truncated.by_cancel
    assert len(cap) == 1
