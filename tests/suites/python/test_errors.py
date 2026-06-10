"""Error-path tests for the public Python API.

These tests pin down the exception *types* the bindings raise for the
documented failure modes: selector parse errors, selector misses, wait
timeouts, unsupported actions, and invalid action data. Assertions are on
exception types and the exception hierarchy — never on message strings —
so they stay stable across platforms and providers.

Widget names come from ``app_config`` so the module runs against every
toolkit the Python suite supports. Tests that need a never-matching
selector use fail-fast operations (``element()``, ``elements()``) or an
explicit short ``timeout`` so the suite stays quick.
"""

from __future__ import annotations

import time

import pytest
import xa11y

# Syntactically valid selector that can never match anything in the test
# apps. Lookups via element()/elements() fail fast (no auto-wait).
NO_MATCH_SELECTOR = 'button[name="xa11y-no-such-element-3f9a"]'

# Syntactically invalid selector (mirrors the Rust integ test
# `error_invalid_selector` in xa11y/tests/integ/actions.rs).
INVALID_SELECTOR = "$$$invalid!!!"

# Generous upper bound for operations that are supposed to honour a short
# (0.5 s) explicit timeout. Polling overhead and slow trees add latency, but
# a short timeout must never degrade into the 5 s default.
SHORT_TIMEOUT = 0.5
SHORT_TIMEOUT_CEILING = 4.0


# ---------------------------------------------------------------------------
# Exception hierarchy
# ---------------------------------------------------------------------------


def test_exception_hierarchy():
    """Every public exception type derives from XA11yError."""
    for exc_type in (
        xa11y.PermissionDeniedError,
        xa11y.AccessibilityNotEnabledError,
        xa11y.SelectorNotMatchedError,
        xa11y.ActionNotSupportedError,
        xa11y.TimeoutError,
        xa11y.InvalidSelectorError,
        xa11y.InvalidActionDataError,
        xa11y.PlatformError,
    ):
        assert issubclass(exc_type, xa11y.XA11yError)
    assert issubclass(xa11y.XA11yError, Exception)
    # xa11y.TimeoutError is deliberately distinct from the builtin
    # TimeoutError — consumers catch xa11y.XA11yError subtypes.
    assert not issubclass(xa11y.TimeoutError, TimeoutError)


# ---------------------------------------------------------------------------
# Invalid selector syntax → InvalidSelectorError
# ---------------------------------------------------------------------------


def test_invalid_selector_elements_raises(app):
    """elements() with unparseable selector syntax raises InvalidSelectorError."""
    with pytest.raises(xa11y.InvalidSelectorError) as excinfo:
        app.locator(INVALID_SELECTOR).elements()
    assert isinstance(excinfo.value, xa11y.XA11yError)


def test_invalid_selector_element_raises(app):
    """element() with unparseable selector syntax raises InvalidSelectorError."""
    with pytest.raises(xa11y.InvalidSelectorError):
        app.locator(INVALID_SELECTOR).element()


def test_invalid_selector_exists_raises(app):
    """exists() swallows only SelectorNotMatched; a parse error must surface.

    Design tenet 1 (no silent fallbacks): exists() answers "did a valid
    selector match?", so an *invalid* selector is an error, not False.
    """
    with pytest.raises(xa11y.InvalidSelectorError):
        app.locator(INVALID_SELECTOR).exists()


def test_invalid_selector_action_fails_fast(app):
    """An action on an invalid selector raises immediately, not after auto-wait.

    The auto-wait loop only retries SelectorNotMatched; parse errors
    propagate on the first poll.
    """
    start = time.monotonic()
    with pytest.raises(xa11y.InvalidSelectorError):
        app.locator(INVALID_SELECTOR).press()
    assert time.monotonic() - start < SHORT_TIMEOUT_CEILING, (
        "InvalidSelectorError should propagate on the first poll, "
        "not after the auto-wait timeout"
    )


# ---------------------------------------------------------------------------
# Valid selector, no match → SelectorNotMatchedError / empty results
# ---------------------------------------------------------------------------


def test_no_match_element_raises_selector_not_matched(app):
    """element() resolves once and fails fast with SelectorNotMatchedError."""
    with pytest.raises(xa11y.SelectorNotMatchedError) as excinfo:
        app.locator(NO_MATCH_SELECTOR).element()
    assert isinstance(excinfo.value, xa11y.XA11yError)


def test_no_match_tree_raises_selector_not_matched(app):
    """tree() resolves once (no auto-wait) and fails with SelectorNotMatchedError."""
    with pytest.raises(xa11y.SelectorNotMatchedError):
        app.locator(NO_MATCH_SELECTOR).tree()


def test_no_match_query_results(app):
    """Multi-element queries report a miss as empty/False/0, not an exception."""
    loc = app.locator(NO_MATCH_SELECTOR)
    assert loc.elements() == []
    assert loc.exists() is False
    assert loc.count() == 0


# ---------------------------------------------------------------------------
# Never-appearing element + short timeout → TimeoutError
# ---------------------------------------------------------------------------


def test_wait_attached_times_out(app):
    """wait_attached on a never-appearing element raises xa11y.TimeoutError."""
    start = time.monotonic()
    with pytest.raises(xa11y.TimeoutError) as excinfo:
        app.locator(NO_MATCH_SELECTOR).wait_attached(timeout=SHORT_TIMEOUT)
    elapsed = time.monotonic() - start
    assert isinstance(excinfo.value, xa11y.XA11yError)
    assert elapsed < SHORT_TIMEOUT_CEILING, (
        f"wait_attached(timeout={SHORT_TIMEOUT}) took {elapsed:.1f}s — "
        "the explicit short timeout was not honoured"
    )


def test_wait_visible_times_out(app):
    """wait_visible on a never-appearing element raises xa11y.TimeoutError."""
    with pytest.raises(xa11y.TimeoutError):
        app.locator(NO_MATCH_SELECTOR).wait_visible(timeout=SHORT_TIMEOUT)


def test_wait_detached_times_out_for_persistent_element(app, app_config):
    """wait_detached on an element that never goes away raises TimeoutError."""
    ok_name = app_config["ok_button_name"]
    with pytest.raises(xa11y.TimeoutError):
        app.locator(f'button[name="{ok_name}"]').wait_detached(timeout=SHORT_TIMEOUT)


# ---------------------------------------------------------------------------
# Action not supported by the element
# ---------------------------------------------------------------------------


def test_unknown_action_name_raises_action_not_supported(app, app_config):
    """perform_action with an unknown verb raises ActionNotSupportedError.

    All three providers (AT-SPI2, AX, UIA) reject unknown action names with
    Error::ActionNotSupported before touching the platform.
    """
    ok_name = app_config["ok_button_name"]
    with pytest.raises(xa11y.ActionNotSupportedError):
        app.locator(f'button[name="{ok_name}"]').perform_action("frobnicate")


def test_expand_on_button_raises(app, app_config):
    """expand() on a plain button errors, never silently no-ops (tenet 1).

    The concrete type differs per backend: AT-SPI2 and AX report
    ActionNotSupported when the element lacks an expand action; some
    bridges surface the rejection as PlatformError. Both are XA11yError
    subtypes — what must never happen is a silent success.
    """
    ok_name = app_config["ok_button_name"]
    with pytest.raises((xa11y.ActionNotSupportedError, xa11y.PlatformError)):
        app.locator(f'button[name="{ok_name}"]').expand()


# ---------------------------------------------------------------------------
# Invalid action data → InvalidActionDataError (validated before dispatch)
# ---------------------------------------------------------------------------


def test_set_numeric_value_nan_raises_invalid_action_data(app, app_config):
    """set_numeric_value(NaN) fails fast with InvalidActionDataError.

    Validated up-front in the Locator, before auto-wait or provider
    dispatch (mirrors the Rust integ test
    `action_set_numeric_value_via_element_rejects_nan`).
    """
    sel = app_config.get("slider_selector")
    if not sel:
        pytest.skip("app has no slider widget")
    with pytest.raises(xa11y.InvalidActionDataError):
        app.locator(sel).set_numeric_value(float("nan"))


def test_select_text_inverted_range_raises_invalid_action_data(app, app_config):
    """select_text(start > end) fails fast with InvalidActionDataError.

    Range validation happens before selector resolution, so any locator
    works and no auto-wait is burned.
    """
    ok_name = app_config["ok_button_name"]
    with pytest.raises(xa11y.InvalidActionDataError):
        app.locator(f'button[name="{ok_name}"]').select_text(5, 2)


def test_locator_nth_zero_raises_invalid_action_data(app):
    """nth() is 1-based; nth(0) raises InvalidActionDataError, not a crash."""
    with pytest.raises(xa11y.InvalidActionDataError):
        app.locator("button").nth(0)


# ---------------------------------------------------------------------------
# App lookup failures
# ---------------------------------------------------------------------------


def test_app_by_name_not_found_raises_selector_not_matched():
    """App.by_name for a nonexistent app raises SelectorNotMatchedError.

    timeout=0 requests a single attempt with no polling, so the miss is
    immediate (mirrors the Rust integ test `error_app_not_found`).
    """
    with pytest.raises(xa11y.SelectorNotMatchedError):
        xa11y.App.by_name("xa11y-no-such-app-3f9a", timeout=0)
