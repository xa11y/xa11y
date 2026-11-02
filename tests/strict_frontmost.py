"""Fail, rather than skip, a frontmost-marked test whose claim failed.

pytest-xa11y skips an ``@pytest.mark.xa11y_frontmost`` test when its app
cannot claim the macOS front slot. That is the right default for a consumer
and the wrong one for CI: a cell whose app can never be frontmost reports
those tests as skipped on every run, and a skip reads as "not applicable"
rather than "never ran". The Cocoa cell did exactly that until its app
stopped launching as an accessory app.

With ``XA11Y_REQUIRE_FRONTMOST`` set (the integ cells set it) the Python
suite's ``pytest_runtest_makereport`` hands each report to :func:`enforce`,
which turns that one kind of skip into a failure. Same reasoning as
``XA11Y_REQUIRE_ALL_CLI``.

Kept free of ``xa11y`` and ``pytest_xa11y`` imports so the harness self-test
job, which has neither, can exercise it with pytester.
"""

from __future__ import annotations

import os
from collections.abc import Callable
from pathlib import Path

import pytest

ENV = "XA11Y_REQUIRE_FRONTMOST"

# The two places pytest-xa11y's plugin.py skips on a failed frontmost claim:
# the `pytest_runtest_call` hook (app already running) and the
# `xa11y_app_factory` fixture's inner `launch` (app started mid-test). Any
# other skip — `Capabilities.guard()`, `skip_unless()`, the test body's own —
# is left alone.
_CLAIM_FUNCTIONS = frozenset({"pytest_runtest_call", "launch"})


def is_frontmost_claim_skip(item, call, plugin_file: Path) -> bool:
    """True when ``call`` was skipped by the frontmost claim in ``plugin_file``."""
    if item.get_closest_marker("xa11y_frontmost") is None or call.excinfo is None:
        return False
    if not call.excinfo.errisinstance(pytest.skip.Exception):
        return False
    plugin_file = plugin_file.resolve()
    return any(
        entry.name in _CLAIM_FUNCTIONS
        and Path(str(entry.path)).resolve() == plugin_file
        for entry in call.excinfo.traceback
    )


def enforce(
    item,
    call,
    rep,
    plugin_file: Path,
    visible_processes: Callable[[], str] | None = None,
) -> bool:
    """Convert ``rep`` to a failure if strictness is on and it is a claim skip.

    Returns whether it converted. ``visible_processes`` is called only on
    that failure path, so the diagnostic costs nothing on a passing run.
    """
    if not (
        rep.when == "call"
        and rep.skipped
        and os.environ.get(ENV)
        and is_frontmost_claim_skip(item, call, plugin_file)
    ):
        return False
    lines = [
        (
            f"{ENV} is set, and this @pytest.mark.xa11y_frontmost test could not "
            f"claim the macOS frontmost slot: {call.excinfo.value}"
        ),
    ]
    if visible_processes is not None:
        lines.append(f"visible processes: {visible_processes()}")
    rep.outcome = "failed"
    rep.longrepr = "\n".join(lines)
    return True
