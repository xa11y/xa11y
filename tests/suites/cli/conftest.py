"""Fixtures for the xa11y CLI integration test suite."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Generator

import pytest

from tests.helpers import launch_test_app

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent.parent


def _connect_to_running_app(xa11y, pid: int):
    """Connect to a test app the harness already launched.

    Tries ``App.by_pid`` first (most precise) with a generous timeout to
    absorb AT-SPI registration delays on Linux, then falls back to
    ``App.by_name`` using the harness-discovered ``XA11Y_TEST_APP_NAME`` —
    on some toolkits the app exposes a name to AT-SPI before its pid lookup
    becomes resolvable.
    """
    try:
        return xa11y.App.by_pid(pid, timeout=10.0)
    except (xa11y.SelectorNotMatchedError, xa11y.PlatformError):
        name = os.environ.get("XA11Y_TEST_APP_NAME")
        if not name:
            raise
        return xa11y.App.by_name(name, timeout=10.0)


# ── Per-app launch configurations ────────────────────────────────────────────

_TAURI_BINARY = str(
    PROJECT_ROOT / "test-apps" / "tauri" / "target" / "debug" / "xa11y-tauri-test-app"
)
_ACCESSKIT_BINARY = str(PROJECT_ROOT / "target" / "debug" / "xa11y-test-app")
_EGUI_BINARY = str(
    PROJECT_ROOT / "test-apps" / "egui" / "target" / "debug" / "xa11y-egui-test-app"
)


def _launch_qt():
    script = str(PROJECT_ROOT / "test-apps" / "qt" / "app.py")
    yield from launch_test_app(
        command=[sys.executable, script],
        app_names=["xa11y-qt-test-app", "xa11y", "python3", "python", "Python", "app.py"],
        env_overrides={"QT_ACCESSIBILITY": "1"},
    )


def _launch_gtk():
    script = str(PROJECT_ROOT / "test-apps" / "gtk" / "app.py")
    yield from launch_test_app(
        command=[sys.executable, script],
        app_names=["xa11y-gtk-test-app", "gtk-test-app", "python3", "python", "Python", "app.py"],
    )


def _launch_cocoa():
    binary = str(PROJECT_ROOT / "test-apps" / "cocoa" / "xa11y-cocoa-test-app")
    if not Path(binary).exists():
        if sys.platform != "darwin":
            pytest.skip("Cocoa test app is macOS-only")
        result = subprocess.run(
            ["make", "build"],
            cwd=str(Path(binary).parent),
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.fail(
                f"Failed to build Cocoa test app:\n{result.stdout}\n{result.stderr}"
            )
    yield from launch_test_app(
        command=[binary],
        app_names=["xa11y-cocoa-test-app"],
    )


def _launch_tauri():
    if not Path(_TAURI_BINARY).exists():
        result = subprocess.run(
            [
                "cargo",
                "build",
                "--manifest-path",
                str(PROJECT_ROOT / "test-apps" / "tauri" / "Cargo.toml"),
            ],
            cwd=str(PROJECT_ROOT),
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.fail(
                f"Failed to build Tauri test app:\n{result.stdout}\n{result.stderr}"
            )
    yield from launch_test_app(
        command=[_TAURI_BINARY],
        app_names=["xa11y-tauri-test-app"],
        content_ready_selector='button[name="OK"]',
    )


def _launch_electron():
    electron_dir = str(PROJECT_ROOT / "test-apps" / "electron")
    npm = "npm.cmd" if sys.platform == "win32" else "npm"
    env_overrides: dict[str, str] = {}
    # Pass cwd via subprocess — launch_test_app does not support cwd directly,
    # so we set it in env_overrides with a sentinel key that Popen uses below.
    # Instead, we use a wrapper: build the command with npx-style absolute path
    # by relying on npm start running from the electron_dir.
    # We pass the cwd by temporarily changing the Popen call indirectly — but
    # launch_test_app has no cwd param. Use node directly instead.
    node_modules_electron = str(
        PROJECT_ROOT / "test-apps" / "electron" / "node_modules" / ".bin" / "electron"
    )
    main_js = str(PROJECT_ROOT / "test-apps" / "electron" / "main.js")

    # Install node_modules if missing.
    if not Path(node_modules_electron).exists():
        result = subprocess.run(
            [npm, "install", "--no-audit", "--no-fund", "--silent"],
            cwd=electron_dir,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.fail(
                f"Failed to install Electron dependencies:\n{result.stdout}\n{result.stderr}"
            )

    yield from launch_test_app(
        command=[node_modules_electron, main_js, "--force-renderer-accessibility"],
        app_names=["xa11y-electron-test-app", "Electron", "xa11y"],
        content_ready_selector='button[name="OK"]',
    )


def _launch_accesskit():
    if not Path(_ACCESSKIT_BINARY).exists():
        result = subprocess.run(
            ["cargo", "build"],
            cwd=str(PROJECT_ROOT),
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.fail(
                f"Failed to build AccessKit test app:\n{result.stdout}\n{result.stderr}"
            )
    yield from launch_test_app(
        command=[_ACCESSKIT_BINARY],
        app_names=["xa11y-test-app"],
    )


def _launch_egui():
    if not Path(_EGUI_BINARY).exists():
        result = subprocess.run(
            [
                "cargo",
                "build",
                "--manifest-path",
                str(PROJECT_ROOT / "test-apps" / "egui" / "Cargo.toml"),
            ],
            cwd=str(PROJECT_ROOT),
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.fail(
                f"Failed to build egui test app:\n{result.stdout}\n{result.stderr}"
            )
    yield from launch_test_app(
        command=[_EGUI_BINARY],
        app_names=["xa11y-egui-test-app"],
        content_ready_selector='button[name="OK"]',
    )


_LAUNCHERS = {
    "qt": _launch_qt,
    "gtk": _launch_gtk,
    "cocoa": _launch_cocoa,
    "tauri": _launch_tauri,
    "electron": _launch_electron,
    "accesskit": _launch_accesskit,
    "egui": _launch_egui,
}


# ── CLI binary ────────────────────────────────────────────────────────────────


@pytest.fixture(scope="session")
def cli_bin() -> list[str]:
    """Return the command prefix to invoke the xa11y CLI.

    Tries, in order:
    1. An installed ``xa11y`` binary on PATH.
    2. ``python -m xa11y._cli`` if the xa11y Python package is importable.
    Falls back to skipping the entire session if neither is found.
    """
    found = shutil.which("xa11y")
    if found:
        return [found]

    result = subprocess.run(
        [sys.executable, "-c", "import xa11y; print('ok')"],
        capture_output=True,
        text=True,
    )
    if result.returncode == 0:
        return [sys.executable, "-m", "xa11y._cli"]

    pytest.skip("xa11y CLI not found — install xa11y-python first")


# ── run_cli helper ────────────────────────────────────────────────────────────


@pytest.fixture(scope="session")
def run_cli(cli_bin: list[str]):
    """Return a callable that runs the CLI and returns (returncode, stdout, stderr)."""

    def _run(*args: str, **kwargs) -> tuple[int, str, str]:
        result = subprocess.run(
            cli_bin + list(args),
            capture_output=True,
            text=True,
            timeout=30,
            **kwargs,
        )
        return result.returncode, result.stdout, result.stderr

    return _run


# ── App name ─────────────────────────────────────────────────────────────────


@pytest.fixture(scope="session")
def app_name() -> str:
    """The name of the app under test, from XA11Y_TEST_APP (default: tauri)."""
    return os.environ.get("XA11Y_TEST_APP", "tauri")


# ── Test app ──────────────────────────────────────────────────────────────────


@pytest.fixture(scope="session")
def app(app_name: str):
    """Launch (or connect to) the test app and yield an xa11y App handle."""
    import xa11y

    pid_env = os.environ.get("XA11Y_TEST_APP_PID")
    if pid_env:
        pid = int(pid_env)
        app_handle = _connect_to_running_app(xa11y, pid)
        yield app_handle
        return

    launcher = _LAUNCHERS.get(app_name)
    if launcher is None:
        pytest.fail(
            f"Unknown XA11Y_TEST_APP={app_name!r}. "
            f"Known apps: {', '.join(_LAUNCHERS)}"
        )

    yield from launcher()


@pytest.fixture(scope="session")
def app_pid(app) -> int:
    """Return the PID of the running test app."""
    pid = app.pid
    assert pid is not None and pid > 0, "test app has no PID"
    return pid
