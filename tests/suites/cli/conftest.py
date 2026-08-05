"""Fixtures for the xa11y CLI integration test suite.

The app is launched by pytest-xa11y from the shared recipes in
``tests/launchers.py`` — the same ones the Python suite uses. This file adds
only what is specific to driving the CLI binary.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

from tests.harness.launch import cli_binary_not_found_message, find_cli_binary
from tests.launchers import launcher_for

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent.parent


# ── CLI binary ────────────────────────────────────────────────────────────────


@pytest.fixture(scope="session")
def cli_bin() -> list[str]:
    """Return the command prefix to invoke the xa11y CLI.

    ``XA11Y_CLI`` — set by tests/harness/launch.py — is authoritative when
    present. Before issue #327 this fixture ran its own discovery and ignored
    the harness's choice entirely, so under CI it silently exercised the
    ``python -m xa11y._cli`` wrapper rather than the ``xa11y`` binary the
    harness had located and the workspace build had just produced.

    Standalone runs (no harness) fall back to the *same* discovery function the
    harness uses, so both paths agree on what "the CLI" is. Not finding one is
    a hard failure: a session-wide skip here is indistinguishable from "the CLI
    is intentionally not covered", which is the exact failure mode this suite
    is meant to catch.
    """
    from_env = os.environ.get("XA11Y_CLI")
    if from_env:
        if not Path(from_env).is_file():
            pytest.fail(f"XA11Y_CLI points at a non-existent file: {from_env}")
        return [from_env]

    found = find_cli_binary()
    if found is None:
        pytest.fail(cli_binary_not_found_message())
    return [found]


# ── run_cli helper ────────────────────────────────────────────────────────────


@pytest.fixture(scope="session")
def run_cli(cli_bin: list[str]):
    """Return a callable that runs the CLI and returns (returncode, stdout, stderr)."""

    def _run(*args: str, **kwargs) -> tuple[int, str, str]:
        # Decode as UTF-8 explicitly. The CLI writes UTF-8 (the tree formatter
        # emits box-drawing connectors), but `text=True` alone decodes with the
        # locale encoding — cp1252 on Windows runners — which silently turns
        # those connectors into mojibake instead of failing loudly.
        result = subprocess.run(
            cli_bin + list(args),
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=30,
            **kwargs,
        )
        return result.returncode, result.stdout, result.stderr

    return _run


# ── Test app ──────────────────────────────────────────────────────────────────


@pytest.fixture(scope="session")
def app_name() -> str:
    """The name of the app under test, from XA11Y_TEST_APP (default: tauri)."""
    return os.environ.get("XA11Y_TEST_APP", "tauri")


@pytest.fixture(scope="session")
def xa11y_launcher(app_name: str):
    """Tell pytest-xa11y how to launch (or attach to) the app under test."""
    return launcher_for(app_name)


@pytest.fixture(scope="session")
def app(xa11y_app):
    """The running test app.

    An alias for the plugin's ``xa11y_app``, kept because every test in this
    suite names it ``app``.
    """
    return xa11y_app


@pytest.fixture(scope="session")
def app_pid(app) -> int:
    """Return the PID of the running test app."""
    pid = app.pid
    assert pid is not None and pid > 0, "test app has no PID"
    return pid
