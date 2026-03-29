"""Tests for module-level functions: app(), apps(), check_permissions()."""

# ── app() (via _make_test_app mock) ───────────────────────────────────────


def test_app_has_name(test_app):
    assert test_app.name == "TestApp"


def test_app_has_pid(test_app):
    assert test_app.pid == 1234


def test_app_nodes_has_children(test_app):
    tree = test_app.nodes()
    assert len(tree.children) > 0
