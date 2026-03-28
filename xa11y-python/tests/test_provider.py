"""Tests for module-level functions: app(), apps(), check_permissions()."""

# ── app() (via _make_test_tree mock) ────────────────────────────────────────


def test_app_root_has_name(tree):
    assert tree.name == "TestApp"


def test_app_root_has_children(tree):
    assert len(tree.children) > 0
