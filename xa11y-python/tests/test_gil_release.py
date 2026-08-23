"""Tenet 5 enforcement: native waits must release the GIL.

Each test runs a Python background thread that increments a counter in a
tight loop while the main thread blocks inside a native xa11y wait. If the
wait held the GIL, the background thread could not be scheduled and the
counter would stay near zero for the duration of the wait.

Everything here runs against the mock provider. ``App.find`` cannot: it
resolves the platform provider through ``xa11y::provider()``, so its tenet-5
coverage lives in ``tests/suites/python/test_gil_release.py``, where a live
accessibility bus exists. Issue #358 went unnoticed because that gap was not
recorded anywhere.

``_cli_main`` is the other known gap, and it is deliberate. It wraps the whole
CLI run in ``allow_threads`` (which matters most for ``xa11y events`` and
``xa11y mcp``, both of which block for the life of the process), but no CLI
subcommand blocks for a bounded, display-free duration, so a tick-counting
assertion would be flaky. The one construction that is deterministic —
redirecting fd 0 to a pipe and closing the write end from a timer thread —
*deadlocks* rather than fails if the GIL is held, because the closing thread
cannot be scheduled. A hanging CI job is worse than a recorded gap, so the
property is left to review of the one-line wrapper. Recording it here rather
than nowhere is the lesson of issue #358.

Referenced from AGENTS.md (Design Tenets, tenet 5) — keep the file name
stable.
"""

import threading
import time

import pytest
import xa11y
from xa11y._native import _find_test_shell_surface

# A 1-second native wait. With the GIL released the 1 ms spin loop gets
# hundreds of iterations; with the GIL held it gets approximately zero
# (only the iterations before the wait starts and after it returns).
_WAIT_S = 1.0
_MIN_TICKS = 50


def _ticks_during(blocked_call):
    """Run `blocked_call` on the main thread while a background thread spins;
    return how many spin iterations happened in that window."""
    ticks = {"n": 0}
    started = threading.Event()
    stop = threading.Event()

    def spin():
        started.set()
        while not stop.is_set():
            ticks["n"] += 1
            time.sleep(0.001)

    t = threading.Thread(target=spin, daemon=True)
    t.start()
    if not started.wait(timeout=5):
        raise AssertionError("spin thread failed to start")
    ticks["n"] = 0  # discount any pre-wait iterations
    try:
        blocked_call()
    finally:
        stop.set()
        t.join(timeout=5)
    return ticks["n"]


def test_wait_visible_releases_gil(test_app):
    missing = test_app.descendant('button[name="Nope"]')

    def blocked():
        with pytest.raises(xa11y.TimeoutError):
            missing.wait_visible(timeout=_WAIT_S)

    ticks = _ticks_during(blocked)
    assert ticks >= _MIN_TICKS, (
        f"background thread made only {ticks} iterations during a "
        f"{_WAIT_S}s wait_visible — the wait is holding the GIL (tenet 5)"
    )


def test_wait_detached_releases_gil(test_app):
    present = test_app.descendant("button")

    def blocked():
        with pytest.raises(xa11y.TimeoutError):
            present.wait_detached(timeout=_WAIT_S)

    ticks = _ticks_during(blocked)
    assert ticks >= _MIN_TICKS, (
        f"background thread made only {ticks} iterations during a "
        f"{_WAIT_S}s wait_detached — the wait is holding the GIL (tenet 5)"
    )


def test_shell_surface_by_kind_releases_gil():
    """`ShellSurface.by_kind` polls the shell enumeration between 100 ms
    sleeps. The public entry point resolves the platform provider, so this
    exercises the same binding path against the mock, which vends no `dock`
    surface and therefore waits out the full timeout."""

    def blocked():
        with pytest.raises(xa11y.SelectorNotMatchedError):
            _find_test_shell_surface("dock", timeout=_WAIT_S)

    ticks = _ticks_during(blocked)
    assert ticks >= _MIN_TICKS, (
        f"background thread made only {ticks} iterations during a "
        f"{_WAIT_S}s by_kind wait — the poll loop is holding the GIL (tenet 5)"
    )


def test_wait_until_releases_gil_between_predicate_calls(test_app):
    """wait_until calls back into Python, so it must hold the GIL only for
    each predicate call — not across the 100 ms sleeps between polls."""

    def blocked():
        with pytest.raises(xa11y.TimeoutError):
            test_app.wait_until(lambda el: False, timeout=_WAIT_S)

    ticks = _ticks_during(blocked)
    assert ticks >= _MIN_TICKS, (
        f"background thread made only {ticks} iterations during a "
        f"{_WAIT_S}s wait_until — the poll loop is holding the GIL (tenet 5)"
    )
