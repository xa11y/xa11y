"""Tests for module-level functions: app(), all_apps(), list_apps(), check_permissions()."""

from xa11y._native import _make_test_apps


# ── app() (via _make_test_tree mock) ────────────────────────────────────────



def test_app_tree_has_nodes(tree):
    assert tree.app_name == "TestApp"
    assert len(tree) == 13


def test_app_tree_has_pid(tree):
    assert tree.pid == 1234


# ── list_apps() ─────────────────────────────────────────────────────────────


def test_list_apps():
    apps = _make_test_apps()
    assert len(apps) == 2
    assert apps[0].name == "TestApp"
    assert apps[0].pid == 1234
    assert apps[0].bundle_id == "com.test.app"
    assert apps[1].name == "OtherApp"
    assert apps[1].pid == 5678
    assert apps[1].bundle_id is None


def test_app_info_repr():
    apps = _make_test_apps()
    r = repr(apps[0])
    assert "TestApp" in r
    assert "1234" in r
    assert "com.test.app" in r

    r2 = repr(apps[1])
    assert "OtherApp" in r2
    assert "bundle_id" not in r2
