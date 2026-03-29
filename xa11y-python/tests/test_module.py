"""Tests for module-level exports and convenience functions."""

import xa11y

# ── Exports ──────────────────────────────────────────────────────────────────


def test_all_classes_exported():
    assert hasattr(xa11y, "App")
    assert hasattr(xa11y, "Element")
    assert hasattr(xa11y, "Locator")
    assert hasattr(xa11y, "Rect")


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
    assert callable(xa11y.apps)
    assert callable(xa11y.check_permissions)


# ── __all__ ──────────────────────────────────────────────────────────────────


def test_all_list():
    assert isinstance(xa11y.__all__, list)
    assert "App" in xa11y.__all__
    assert "Element" in xa11y.__all__
    assert "app" in xa11y.__all__
    assert "apps" in xa11y.__all__
    assert "XA11yError" in xa11y.__all__


# ── Types are correct classes ────────────────────────────────────────────────


def test_app_is_type():
    assert isinstance(xa11y.App, type)


def test_element_is_type():
    assert isinstance(xa11y.Element, type)


def test_locator_is_type():
    assert isinstance(xa11y.Locator, type)


def test_rect_is_type():
    assert isinstance(xa11y.Rect, type)
