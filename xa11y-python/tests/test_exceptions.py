"""Tests for exception hierarchy and error mapping."""

import contextlib

import xa11y

# ── Hierarchy ────────────────────────────────────────────────────────────────


def test_base_exception():
    assert issubclass(xa11y.XA11yError, Exception)


def test_permission_denied_inherits():
    assert issubclass(xa11y.PermissionDeniedError, xa11y.XA11yError)


def test_app_not_found_inherits():
    assert issubclass(xa11y.AppNotFoundError, xa11y.XA11yError)


def test_selector_not_matched_inherits():
    assert issubclass(xa11y.SelectorNotMatchedError, xa11y.XA11yError)


def test_action_not_supported_inherits():
    assert issubclass(xa11y.ActionNotSupportedError, xa11y.XA11yError)


def test_timeout_inherits():
    assert issubclass(xa11y.TimeoutError, xa11y.XA11yError)


def test_invalid_selector_inherits():
    assert issubclass(xa11y.InvalidSelectorError, xa11y.XA11yError)


def test_platform_error_inherits():
    assert issubclass(xa11y.PlatformError, xa11y.XA11yError)


# ── Catching with base class ────────────────────────────────────────────────


def test_catch_with_base_class(tree):
    with contextlib.suppress(xa11y.XA11yError):
        tree.query("[[[bad")


def test_catch_with_specific_class(tree):
    with contextlib.suppress(xa11y.InvalidSelectorError):
        tree.query("[[[bad")


# ── Error messages ───────────────────────────────────────────────────────────


def test_selector_not_matched_message(tree):
    try:
        tree.press("menu_item")
    except xa11y.SelectorNotMatchedError as e:
        assert "menu_item" in str(e)


def test_invalid_selector_message(tree):
    try:
        tree.query("[[[bad")
    except xa11y.InvalidSelectorError as e:
        assert "bad" in str(e) or "Invalid" in str(e)
