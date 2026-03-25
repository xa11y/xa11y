"""Tests for Provider: app(), all_apps(), list_apps(), check_permissions(), context manager."""

import pytest
from xa11y._native import _make_test_provider

# ── Provider creation ────────────────────────────────────────────────────────


def test_provider_repr(provider):
    assert repr(provider) == "Provider()"


# ── app() ────────────────────────────────────────────────────────────────────


def test_app_by_name(provider):
    tree = provider.app("TestApp")
    assert tree.app_name == "TestApp"
    assert len(tree) == 13


def test_app_by_pid(provider):
    tree = provider.app(pid=9999)  # mock ignores target, returns canned tree
    assert len(tree) > 0


def test_app_no_target(provider):
    with pytest.raises(ValueError, match="Either name or pid"):
        provider.app()


def test_app_query_options(provider):
    tree = provider.app("TestApp", max_depth=2, visible_only=True)
    assert len(tree) > 0  # mock returns full tree regardless


def test_app_with_roles(provider):
    tree = provider.app("TestApp", roles=["button", "text_field"])
    assert len(tree) > 0


def test_app_include_raw(provider):
    tree = provider.app("TestApp", include_raw=True)
    assert len(tree) > 0


# ── all_apps() ───────────────────────────────────────────────────────────────


def test_all_apps(provider):
    tree = provider.all_apps()
    assert len(tree) > 0


def test_all_apps_with_options(provider):
    tree = provider.all_apps(max_depth=1)
    assert len(tree) > 0


# ── list_apps() ──────────────────────────────────────────────────────────────


def test_list_apps(provider):
    apps = provider.list_apps()
    assert len(apps) == 2
    assert apps[0].name == "TestApp"
    assert apps[0].pid == 1234
    assert apps[0].bundle_id == "com.test.app"
    assert apps[1].name == "OtherApp"
    assert apps[1].pid == 5678
    assert apps[1].bundle_id is None


def test_app_info_repr(provider):
    apps = provider.list_apps()
    r = repr(apps[0])
    assert "TestApp" in r
    assert "1234" in r
    assert "com.test.app" in r

    r2 = repr(apps[1])
    assert "OtherApp" in r2
    assert "bundle_id" not in r2


# ── check_permissions() ─────────────────────────────────────────────────────


def test_check_permissions(provider):
    assert provider.check_permissions() == "granted"


# ── Context manager ──────────────────────────────────────────────────────────


def test_context_manager():
    with _make_test_provider() as p:
        tree = p.app("TestApp")
        assert len(tree) > 0


def test_context_manager_does_not_suppress_exceptions():
    with pytest.raises(ValueError), _make_test_provider():
        raise ValueError("test")


def test_methods_fail_without_context_manager():
    p = _make_test_provider()
    with pytest.raises(RuntimeError, match="context manager"):
        p.app("TestApp")


def test_methods_fail_after_exit():
    p = _make_test_provider()
    with p:
        p.app("TestApp")  # works inside
    with pytest.raises(RuntimeError, match="context manager"):
        p.app("TestApp")  # fails after exit
