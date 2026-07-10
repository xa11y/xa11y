"""Tests for the `App.by_name` / `App.by_pid` binding surface.

The actual polling behavior is covered by the Rust unit tests in
`xa11y/tests/unit_test.rs` (`by_name_with_polls_until_app_appears`,
`by_name_with_zero_timeout_is_single_attempt`, etc). These tests cover the
Python-binding-specific concerns: keyword argument plumbing and timeout
validation — which fires before the provider is touched, so the tests work
on CI runners that have no AT-SPI session.
"""

import pytest
import xa11y


def test_by_name_rejects_negative_timeout():
    with pytest.raises(ValueError, match="non-negative"):
        xa11y.App.by_name("ignored", timeout=-1.0)


def test_by_pid_rejects_negative_timeout():
    with pytest.raises(ValueError, match="non-negative"):
        xa11y.App.by_pid(0, timeout=-0.5)


def test_by_name_rejects_nan_timeout():
    with pytest.raises(ValueError, match="non-negative"):
        xa11y.App.by_name("ignored", timeout=float("nan"))


def test_by_name_rejects_infinite_timeout():
    with pytest.raises(ValueError, match="non-negative"):
        xa11y.App.by_name("ignored", timeout=float("inf"))


def test_by_name_timeout_is_keyword_only():
    # The `*` in `#[pyo3(signature = (name, *, timeout=5.0))]` makes `timeout`
    # keyword-only. Passing positionally must raise TypeError.
    with pytest.raises(TypeError):
        xa11y.App.by_name("ignored", -1.0)


def test_by_name_zero_timeout_validates():
    # 0.0 is a valid non-negative value — must not raise ValueError. (The
    # downstream lookup may still raise, but not for the timeout argument.)
    try:
        xa11y.App.by_name("__xa11y_no_such_app_for_test__", timeout=0.0)
    except ValueError:
        pytest.fail("timeout=0.0 must be accepted as a no-wait sentinel")
    except xa11y.XA11yError:
        # Any xa11y-level error is fine — proves the call reached the lookup.
        pass


def test_foreground_rejects_negative_timeout():
    with pytest.raises(ValueError, match="non-negative"):
        xa11y.App.foreground(timeout=-1.0)


def test_foreground_timeout_is_keyword_only():
    # `foreground` takes only a keyword-only `timeout` (signature `(*, timeout=None)`).
    # Passing a positional argument must raise TypeError.
    with pytest.raises(TypeError):
        xa11y.App.foreground(0.0)


def test_foreground_zero_timeout_validates():
    # 0.0 is a valid non-negative value — must not raise ValueError. (The
    # downstream lookup may still raise if nothing holds focus, but not for
    # the timeout argument.)
    try:
        xa11y.App.foreground(timeout=0.0)
    except ValueError:
        pytest.fail("timeout=0.0 must be accepted as a no-wait sentinel")
    except xa11y.XA11yError:
        # Any xa11y-level error is fine — proves the call reached the lookup.
        pass


def test_app_is_foreground_is_bool(mock_app):
    # The mock provider reports its application root as the foreground app,
    # so an app resolved through the finder carries `is_foreground=True`. The
    # flag must be a plain bool.
    assert isinstance(mock_app.is_foreground, bool)
    assert mock_app.is_foreground is True


def test_app_is_foreground_matches_element_shape(mock_app):
    # `App.is_foreground` is the application-level foreground flag: a read-only
    # boolean property. Assigning must fail.
    with pytest.raises(AttributeError):
        mock_app.is_foreground = True


def test_app_focused_is_deprecated_alias(mock_app):
    # `focused` is retained only as a deprecated alias for `is_foreground`; it
    # must return the same value and emit a DeprecationWarning pointing at the
    # access site.
    with pytest.warns(DeprecationWarning):
        value = mock_app.focused
    assert value == mock_app.is_foreground
    assert value is True
