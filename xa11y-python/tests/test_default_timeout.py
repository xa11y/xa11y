"""Tests for the process-wide default timeout (issue #259).

`set_default_timeout` / `get_default_timeout` configure the default used by
every auto-waiting action method, ``wait_*`` call, and app lookup that
doesn't pass an explicit ``timeout=``. The env-var path
(``XA11Y_DEFAULT_TIMEOUT``) is read once at import, so it is exercised in
subprocesses with a controlled environment.
"""

import os
import subprocess
import sys
import time

import pytest
import xa11y
from xa11y._native import _make_test_action_probe

BUILTIN_DEFAULT = 5.0

# Ceiling for "this must not have waited out a long timeout" assertions.
# Generous so slow CI runners don't flake, while still far below the long
# timeouts the tests configure.
FAST_CEILING = 3.0


@pytest.fixture(autouse=True)
def _restore_default_timeout():
    """Reset the global default after each test — it is process-wide state."""
    yield
    xa11y.set_default_timeout(BUILTIN_DEFAULT)


def test_builtin_default_is_five_seconds():
    assert xa11y.get_default_timeout() == BUILTIN_DEFAULT


def test_set_and_get_round_trip():
    xa11y.set_default_timeout(12.5)
    assert xa11y.get_default_timeout() == 12.5


def test_set_default_timeout_rejects_negative():
    with pytest.raises(ValueError, match="non-negative"):
        xa11y.set_default_timeout(-1.0)


def test_set_default_timeout_rejects_nan():
    with pytest.raises(ValueError, match="non-negative"):
        xa11y.set_default_timeout(float("nan"))


def test_set_default_timeout_rejects_infinity():
    with pytest.raises(ValueError, match="non-negative"):
        xa11y.set_default_timeout(float("inf"))


def test_invalid_value_does_not_change_default():
    with pytest.raises(ValueError):
        xa11y.set_default_timeout(-1.0)
    assert xa11y.get_default_timeout() == BUILTIN_DEFAULT


def test_auto_wait_action_uses_global_default():
    """Action methods (which take no timeout parameter) honor the global."""
    probe = _make_test_action_probe()
    missing = probe.locator('button[name="DoesNotExist"]')

    xa11y.set_default_timeout(0.3)
    start = time.monotonic()
    with pytest.raises(xa11y.TimeoutError):
        missing.press()
    assert time.monotonic() - start < FAST_CEILING, (
        "auto-wait must use the 0.3s global default, not the built-in 5s"
    )


def test_wait_method_uses_global_default_when_no_arg():
    probe = _make_test_action_probe()
    missing = probe.locator('button[name="DoesNotExist"]')

    xa11y.set_default_timeout(0.3)
    start = time.monotonic()
    with pytest.raises(xa11y.TimeoutError):
        missing.wait_visible()
    assert time.monotonic() - start < FAST_CEILING


def test_explicit_timeout_beats_global_default():
    probe = _make_test_action_probe()
    missing = probe.locator('button[name="DoesNotExist"]')

    xa11y.set_default_timeout(60.0)
    start = time.monotonic()
    with pytest.raises(xa11y.TimeoutError):
        missing.wait_visible(timeout=0.2)
    assert time.monotonic() - start < FAST_CEILING, (
        "explicit timeout=0.2 must win over the 60s global default"
    )


def test_zero_default_is_single_attempt():
    """``0`` keeps "single attempt, no polling": an actionable element still
    succeeds on the one attempt; a missing one fails immediately."""
    probe = _make_test_action_probe()
    xa11y.set_default_timeout(0.0)

    probe.locator('button[name="Back"]').press()
    assert any(action == "press" for _, action, _ in probe.actions()), (
        "zero timeout must still allow one successful attempt"
    )

    start = time.monotonic()
    with pytest.raises(xa11y.TimeoutError):
        probe.locator('button[name="DoesNotExist"]').press()
    assert time.monotonic() - start < FAST_CEILING


def test_wait_visible_rejects_negative_timeout():
    # Regression: a negative explicit timeout used to reach
    # Duration::from_secs_f64 and panic; it must be a ValueError.
    probe = _make_test_action_probe()
    with pytest.raises(ValueError, match="non-negative"):
        probe.locator("button").wait_visible(timeout=-1.0)


def test_app_lookup_validates_explicit_timeout_with_global_set():
    # App.by_name with an explicit bad timeout still raises ValueError
    # regardless of the global default.
    xa11y.set_default_timeout(0.1)
    with pytest.raises(ValueError, match="non-negative"):
        xa11y.App.by_name("ignored", timeout=-1.0)


# ── Environment variable (read once at import → subprocess tests) ────────────


def _run_python(code: str, env_value: str | None) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    env.pop("XA11Y_DEFAULT_TIMEOUT", None)
    if env_value is not None:
        env["XA11Y_DEFAULT_TIMEOUT"] = env_value
    return subprocess.run(
        [sys.executable, "-c", code],
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
    )


def test_env_var_sets_default():
    result = _run_python("import xa11y; print(xa11y.get_default_timeout())", env_value="12.5")
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "12.5"


def test_set_default_timeout_overrides_env_var():
    result = _run_python(
        "import xa11y; xa11y.set_default_timeout(2.0); print(xa11y.get_default_timeout())",
        env_value="12.5",
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "2.0"


def test_invalid_env_var_fails_import():
    result = _run_python("import xa11y", env_value="not-a-number")
    assert result.returncode != 0, (
        "import must fail loudly on a malformed XA11Y_DEFAULT_TIMEOUT, "
        "not silently fall back to the built-in default"
    )
    assert "XA11Y_DEFAULT_TIMEOUT" in result.stderr
    assert "ValueError" in result.stderr


def test_negative_env_var_fails_import():
    result = _run_python("import xa11y", env_value="-3")
    assert result.returncode != 0
    assert "XA11Y_DEFAULT_TIMEOUT" in result.stderr
