"""Tenet 5 enforcement for the entry points that need a real provider.

``xa11y-python/tests/test_gil_release.py`` is the main home for these tests,
but everything it covers runs against the mock provider. ``App.find`` cannot:
it resolves the platform provider through ``xa11y::provider()``, so proving it
releases the GIL needs a live accessibility bus, which is what this suite has.

The shape is the same as the unit-test file's: a background Python thread
increments a counter while the main thread blocks in a native call. If the
call held the GIL, the counter would stay near zero.

Regression test for issue #358, where ``App.find`` held the GIL for its whole
poll loop — including the sleeps between polls — freezing every other thread
in the consumer's process for the full timeout.
"""

from __future__ import annotations

import threading
import time

import pytest
import xa11y

# A 1-second native wait. With the GIL released the 1 ms spin loop gets
# hundreds of iterations; with it held it gets approximately zero.
_WAIT_S = 1.0
_MIN_TICKS = 50


def _ticks_during(blocked_call) -> int:
    """Spin a background thread while ``blocked_call`` blocks the main one."""
    ticks = {"n": 0}
    started = threading.Event()
    stop = threading.Event()

    def spin():
        started.set()
        while not stop.is_set():
            ticks["n"] += 1
            time.sleep(0.001)

    thread = threading.Thread(target=spin, daemon=True)
    thread.start()
    if not started.wait(timeout=5):
        raise AssertionError("spin thread failed to start")
    ticks["n"] = 0  # discount any pre-wait iterations
    try:
        blocked_call()
    finally:
        stop.set()
        thread.join(timeout=5)
    return ticks["n"]


def test_app_find_releases_gil_between_predicate_calls(app):
    """``App.find`` calls back into Python, so it must hold the GIL only for
    each predicate call — not across the sleeps between polls."""

    def blocked():
        with pytest.raises((xa11y.TimeoutError, xa11y.SelectorNotMatchedError)):
            xa11y.App.find(lambda candidate: False, timeout=_WAIT_S)

    ticks = _ticks_during(blocked)
    assert ticks >= _MIN_TICKS, (
        f"background thread made only {ticks} iterations during a {_WAIT_S}s "
        f"App.find — the poll loop is holding the GIL (tenet 5, issue #358)"
    )


def test_app_find_still_propagates_a_predicate_exception(app):
    """Releasing the GIL must not change how a raising predicate surfaces.

    The predicate's exception is stashed across the ``allow_threads`` boundary
    and re-raised; a regression here would show up as a misleading "no match"
    timeout instead of the caller's own error.
    """

    class Sentinel(Exception):
        pass

    def boom(candidate):
        raise Sentinel("predicate blew up")

    with pytest.raises(Sentinel, match="predicate blew up"):
        xa11y.App.find(boom, timeout=_WAIT_S)


def test_app_find_still_matches(app):
    """The happy path, so the GIL change cannot silently break discovery."""
    found = xa11y.App.find(lambda candidate: candidate.pid == app.pid, timeout=5.0)
    assert found.pid == app.pid
