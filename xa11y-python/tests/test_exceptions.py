"""Tests for exception hierarchy and error mapping."""

import contextlib

import pytest
import xa11y
from xa11y import _native

# Every documented public exception class. Kept in one place so the regression
# tests for #189 (`__module__` / `__name__` / public-vs-`_native` identity)
# stay in sync if the public surface grows.
_PUBLIC_EXCEPTIONS = [
    "XA11yError",
    "PermissionDeniedError",
    "AccessibilityNotEnabledError",
    "SelectorNotMatchedError",
    "ActionNotSupportedError",
    "TimeoutError",
    "InvalidSelectorError",
    "InvalidActionDataError",
    "PlatformError",
]

# ── Hierarchy ────────────────────────────────────────────────────────────────


def test_base_exception():
    assert issubclass(xa11y.XA11yError, Exception)


def test_permission_denied_inherits():
    assert issubclass(xa11y.PermissionDeniedError, xa11y.XA11yError)


def test_accessibility_not_enabled_inherits():
    assert issubclass(xa11y.AccessibilityNotEnabledError, xa11y.XA11yError)


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


def test_catch_with_base_class(test_app):
    with contextlib.suppress(xa11y.XA11yError):
        test_app.descendant("[[[bad").elements()


def test_catch_with_specific_class(test_app):
    with contextlib.suppress(xa11y.InvalidSelectorError):
        test_app.descendant("[[[bad").elements()


# ── Error messages ───────────────────────────────────────────────────────────


def test_selector_not_matched_message(test_app):
    loc = test_app.descendant("menu_item")
    # element() raises SelectorNotMatchedError immediately (no auto-wait)
    try:
        loc.element()
    except xa11y.SelectorNotMatchedError as e:
        assert "menu_item" in str(e)


def test_invalid_selector_message(test_app):
    try:
        test_app.descendant("[[[bad").elements()
    except xa11y.InvalidSelectorError as e:
        assert "bad" in str(e) or "Invalid" in str(e)


# ── Public surface (issue #189) ──────────────────────────────────────────────
#
# These tests guard against a regression where exception classes leaked their
# private `_native` module path or an internal Rust-only class name into
# tracebacks, e.g. `_native.XA11yTimeoutError: …` for what is documented as
# `xa11y.TimeoutError`. The mismatch silently broke `try/except` blocks that
# followed the documented API and was invisible at code-review time.


@pytest.mark.parametrize("cls_name", _PUBLIC_EXCEPTIONS)
def test_exception_public_module(cls_name):
    """`__module__` is the public package, not `_native`.

    Tracebacks render as `xa11y.TimeoutError: …` rather than
    `_native.XA11yTimeoutError: …` — the latter advertises a private path and
    breaks the documented public API contract.
    """
    cls = getattr(xa11y, cls_name)
    assert cls.__module__ == "xa11y", (
        f"{cls_name}.__module__ is {cls.__module__!r}; "
        f"a `_native` prefix in tracebacks would contradict the documented "
        f"`xa11y.{cls_name}` public surface (issue #189)."
    )


@pytest.mark.parametrize("cls_name", _PUBLIC_EXCEPTIONS)
def test_exception_public_qualname(cls_name):
    """`__name__` / `__qualname__` match the documented public name."""
    cls = getattr(xa11y, cls_name)
    assert cls.__name__ == cls_name
    assert cls.__qualname__ == cls_name


@pytest.mark.parametrize("cls_name", _PUBLIC_EXCEPTIONS)
def test_exception_identity_public_eq_native(cls_name):
    """`xa11y.X` and `xa11y._native.X` are the same class object.

    `isinstance` and `except` clauses are class-identity-based, so divergence
    here would silently break documented-API exception handlers.
    """
    public = getattr(xa11y, cls_name)
    native = getattr(_native, cls_name)
    assert public is native


# ── Triggered error paths (issue #189) ───────────────────────────────────────
#
# The bug in #189 was caught while writing an E2E test — `except xa11y.X`
# didn't catch what was actually raised. These tests trigger each error path
# we can produce from the mock provider and assert `isinstance(exc, xa11y.X)`,
# matching the pattern documented in the public guide.


def test_timeout_error_is_caught_as_public_class(test_app):
    """`wait_detached` on something always present raises `xa11y.TimeoutError`.

    Regression for #189: prior to the fix this raised
    `_native.XA11yTimeoutError`, which `except xa11y.TimeoutError:` only
    caught by accident of shared class identity — and the traceback
    advertised a private path users shouldn't depend on.
    """
    with pytest.raises(xa11y.TimeoutError) as exc_info:
        test_app.descendant("button").wait_detached(timeout=0.05)
    exc = exc_info.value
    assert isinstance(exc, xa11y.TimeoutError)
    assert isinstance(exc, xa11y.XA11yError)
    # Traceback path: the class advertises itself as `xa11y.TimeoutError`,
    # not `_native.<anything>`.
    assert type(exc).__module__ == "xa11y"
    assert type(exc).__name__ == "TimeoutError"


def test_selector_not_matched_is_caught_as_public_class(test_app):
    with pytest.raises(xa11y.SelectorNotMatchedError) as exc_info:
        test_app.descendant("nonexistent_role").element()
    assert isinstance(exc_info.value, xa11y.SelectorNotMatchedError)
    assert type(exc_info.value).__module__ == "xa11y"


def test_invalid_selector_is_caught_as_public_class(test_app):
    with pytest.raises(xa11y.InvalidSelectorError) as exc_info:
        test_app.descendant("[[[bad").elements()
    assert isinstance(exc_info.value, xa11y.InvalidSelectorError)
    assert type(exc_info.value).__module__ == "xa11y"


def test_invalid_action_data_is_caught_as_public_class(test_app):
    """`Locator.nth(0)` is rejected at the binding boundary — was a panic
    before, is `InvalidActionDataError` now (see `lib.rs::Locator::nth`)."""
    with pytest.raises(xa11y.InvalidActionDataError) as exc_info:
        test_app.nth(0)
    assert isinstance(exc_info.value, xa11y.InvalidActionDataError)
    assert type(exc_info.value).__module__ == "xa11y"


def test_platform_error_is_caught_as_public_class(test_app):
    """The mock provider intentionally returns `Error::Platform` from
    `subscribe()`, which exercises the `PlatformError` mapping path."""
    with pytest.raises(xa11y.PlatformError) as exc_info:
        test_app.element().subscribe()
    assert isinstance(exc_info.value, xa11y.PlatformError)
    assert type(exc_info.value).__module__ == "xa11y"


def test_no_internal_timeout_alias_leaks():
    """The renamed-away Rust struct `XA11yTimeoutError` must not be a public
    attribute on either the package or the private `_native` submodule —
    callers should only reach the class via `xa11y.TimeoutError`."""
    assert not hasattr(xa11y, "XA11yTimeoutError")
    assert not hasattr(_native, "XA11yTimeoutError")
