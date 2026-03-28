"""Tests for module-level exports and convenience functions."""

import xa11y

# ── Exports ──────────────────────────────────────────────────────────────────


def test_all_classes_exported():
    assert hasattr(xa11y, "Node")
    assert hasattr(xa11y, "Locator")
    assert hasattr(xa11y, "Rect")
    assert hasattr(xa11y, "AppInfo")


def test_all_exceptions_exported():
    assert hasattr(xa11y, "XA11yError")
    assert hasattr(xa11y, "PermissionDeniedError")
    assert hasattr(xa11y, "AppNotFoundError")
    assert hasattr(xa11y, "SelectorNotMatchedError")
    assert hasattr(xa11y, "ActionNotSupportedError")
    assert hasattr(xa11y, "TimeoutError")
    assert hasattr(xa11y, "InvalidSelectorError")
    assert hasattr(xa11y, "PlatformError")


def test_all_functions_exported():
    assert callable(xa11y.app)
    assert callable(xa11y.all_apps)
    assert callable(xa11y.locator)
    assert callable(xa11y.list_apps)
    assert callable(xa11y.check_permissions)


# ── __all__ ──────────────────────────────────────────────────────────────────


def test_all_list():
    assert isinstance(xa11y.__all__, list)
    assert "Node" in xa11y.__all__
    assert "app" in xa11y.__all__
    assert "all_apps" in xa11y.__all__
    assert "locator" in xa11y.__all__
    assert "XA11yError" in xa11y.__all__


# ── Types are correct classes ────────────────────────────────────────────────


def test_node_is_type():
    assert isinstance(xa11y.Node, type)


def test_locator_is_type():
    assert isinstance(xa11y.Locator, type)


def test_rect_is_type():
    assert isinstance(xa11y.Rect, type)


def test_app_info_is_type():
    assert isinstance(xa11y.AppInfo, type)
