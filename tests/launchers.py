"""How each test app is launched, as pytest-xa11y ``AppLauncher`` recipes.

One table, used by both the Python and CLI integration suites. Before this
module the two conftest files carried near-identical copies of nine launch
functions, which had already drifted: the Cocoa and AccessKit apps ran
headless under one suite and windowed under the other, and one copy built the
AccessKit app with a workspace-wide ``cargo build`` where the other used
``-p xa11y-test-app``.

The launch mechanics themselves — polling until the app registers with the
accessibility API, gating on a readiness selector, capturing output, claiming
the macOS frontmost slot, tearing the process down — live in pytest-xa11y.
What stays here is the part that is genuinely about this repository: where
each test app's binary is, how to build it if it is missing, and which
platforms it runs on.

``tests/harness/launch.py`` keeps its own copy of the command table. It is
the outer harness that launches an app once and runs every language suite
against it, so it has to work on a bare interpreter with no pytest and no
plugin installed. Unifying that copy too is worth doing, but it is a change
to how CI starts apps, not to how tests find them.
"""

from __future__ import annotations

import os
import subprocess
import sys
from dataclasses import replace
from pathlib import Path

import pytest
from pytest_xa11y import AppLauncher

PROJECT_ROOT = Path(__file__).resolve().parent.parent

# Overall app-startup / content-readiness deadline, in seconds. Overridable
# for slow machines and loaded CI runners. `None` when unset, so the plugin's
# own default (and `--xa11y-startup-timeout`) applies — an AppLauncher's
# startup_timeout outranks the flag, so passing one unconditionally would make
# the flag inert.
_STARTUP_TIMEOUT_ENV = os.environ.get("XA11Y_TEST_STARTUP_TIMEOUT")
STARTUP_TIMEOUT = float(_STARTUP_TIMEOUT_ENV) if _STARTUP_TIMEOUT_ENV else None

# Apps with a real, activatable macOS window whose tests depend on holding the
# frontmost slot — input simulation delivers CGEvents there, and focus
# assertions read it. Excluded: cocoa (runs headless as an accessory app, so
# it cannot be frontmost) and accesskit (synthesises its own focus events).
# Mirrors _MACOS_FRONTMOST_APPS in tests/harness/launch.py.
FRONTMOST_APPS = {"tauri", "qt", "electron", "egui"}


def _build(what: str, command: list[str], cwd: Path) -> None:
    """Build a test app, failing the session with the compiler's own output."""
    result = subprocess.run(command, cwd=str(cwd), capture_output=True, text=True)
    if result.returncode != 0:
        pytest.fail(f"Failed to build {what} test app:\n{result.stdout}\n{result.stderr}")


def _qt() -> AppLauncher:
    return AppLauncher(
        command=[sys.executable, str(PROJECT_ROOT / "test-apps" / "qt" / "app.py")],
        env={"QT_ACCESSIBILITY": "1"},
        app_names=["xa11y-qt-test-app", "xa11y", "python3", "python", "Python", "app.py"],
    )


def _gtk() -> AppLauncher:
    return AppLauncher(
        command=[sys.executable, str(PROJECT_ROOT / "test-apps" / "gtk" / "app.py")],
        app_names=[
            "xa11y-gtk-test-app",
            "gtk-test-app",
            "python3",
            "python",
            "Python",
            "app.py",
        ],
    )


def _cocoa() -> AppLauncher:
    binary = PROJECT_ROOT / "test-apps" / "cocoa" / "xa11y-cocoa-test-app"
    if not binary.exists():
        if sys.platform != "darwin":
            pytest.skip("Cocoa test app is macOS-only")
        _build("Cocoa", ["make", "build"], binary.parent)
    return AppLauncher(
        command=[str(binary), "--headless"],
        app_names=["xa11y-cocoa-test-app"],
    )


def _tauri() -> AppLauncher:
    binary = PROJECT_ROOT / "test-apps" / "tauri" / "target" / "debug" / "xa11y-tauri-test-app"
    if not binary.exists():
        _build(
            "Tauri",
            [
                "cargo",
                "build",
                "--manifest-path",
                str(PROJECT_ROOT / "test-apps" / "tauri" / "Cargo.toml"),
            ],
            PROJECT_ROOT,
        )
    return AppLauncher(
        command=[str(binary)],
        app_names=["xa11y-tauri-test-app"],
        ready='button[name="OK"]',
    )


def _electron() -> AppLauncher:
    electron_dir = PROJECT_ROOT / "test-apps" / "electron"
    electron_bin = electron_dir / "node_modules" / ".bin" / "electron"
    if not electron_bin.exists():
        npm = "npm.cmd" if sys.platform == "win32" else "npm"
        _build("Electron", [npm, "install", "--no-audit", "--no-fund", "--silent"], electron_dir)
    return AppLauncher(
        command=[
            str(electron_bin),
            str(electron_dir / "main.js"),
            "--force-renderer-accessibility",
        ],
        cwd=electron_dir,
        app_names=["xa11y-electron-test-app", "Electron", "xa11y"],
        ready='button[name="OK"]',
    )


def _accesskit() -> AppLauncher:
    # Part of the Cargo workspace; the binary lands in the workspace target dir.
    binary = PROJECT_ROOT / "target" / "debug" / "xa11y-test-app"
    if not binary.exists():
        _build("AccessKit", ["cargo", "build", "-p", "xa11y-test-app"], PROJECT_ROOT)
    return AppLauncher(
        command=[str(binary), "--headless"],
        app_names=["xa11y-test-app", "xa11y Test App"],
    )


def _egui() -> AppLauncher:
    # Outside the workspace: eframe's dependency tree is heavy enough to slow
    # every workspace-wide build.
    binary = PROJECT_ROOT / "test-apps" / "egui" / "target" / "debug" / "xa11y-egui-test-app"
    if not binary.exists():
        _build(
            "egui",
            [
                "cargo",
                "build",
                "--manifest-path",
                str(PROJECT_ROOT / "test-apps" / "egui" / "Cargo.toml"),
            ],
            PROJECT_ROOT,
        )
    return AppLauncher(
        command=[str(binary)],
        app_names=["xa11y-egui-test-app"],
        ready='button[name="OK"]',
    )


def _dotnet(app: str) -> AppLauncher:
    # The `net8.0-windows` path segment must track TargetFramework in
    # test-apps/<app>/xa11y-<app>-test-app.csproj.
    project_dir = PROJECT_ROOT / "test-apps" / app
    binary = project_dir / "bin" / "Debug" / "net8.0-windows" / f"xa11y-{app}-test-app.exe"
    if not binary.exists():
        if sys.platform != "win32":
            pytest.skip(f"{app} test app is Windows-only")
        _build(app, ["dotnet", "build", str(project_dir)], PROJECT_ROOT)
    return AppLauncher(
        command=[str(binary)],
        app_names=[f"xa11y-{app}-test-app"],
        ready='button[name="OK"]',
    )


_BUILDERS = {
    "qt": _qt,
    "gtk": _gtk,
    "cocoa": _cocoa,
    "tauri": _tauri,
    "electron": _electron,
    "accesskit": _accesskit,
    "egui": _egui,
    "winforms": lambda: _dotnet("winforms"),
    "wpf": lambda: _dotnet("wpf"),
}

KNOWN_APPS = tuple(_BUILDERS)


def launcher_for(app_name: str) -> AppLauncher:
    """The launch recipe for ``XA11Y_TEST_APP``.

    When ``XA11Y_TEST_APP_PID`` is set the harness has already launched the
    app for every language suite to share, so this attaches instead of
    launching — and never terminates a process it did not start.
    """
    pid_env = os.environ.get("XA11Y_TEST_APP_PID")
    if pid_env:
        harness_name = os.environ.get("XA11Y_TEST_APP_NAME")
        return AppLauncher(
            attach_pid=int(pid_env),
            # Match the harness-discovered name as well as the pid: some
            # toolkits expose a name to the accessibility API before their pid
            # lookup resolves, and others the reverse. Matching either signal
            # absorbs both races.
            #
            # This is a substring match, where the conftests it replaced used
            # exact equality. Looser, and worth knowing: App.find returns the
            # first candidate the platform enumerates that satisfies the
            # predicate, so a name match can win over the correct pid. The
            # names the harness discovers here are specific
            # ("xa11y-qt-test-app"), so nothing generic can collide; a suite
            # that needs precision uses AppLauncher(app_name_prefix=...).
            app_names=[harness_name] if harness_name else [],
            label=app_name,
            startup_timeout=STARTUP_TIMEOUT,
        )

    builder = _BUILDERS.get(app_name)
    if builder is None:
        pytest.fail(
            f"Unknown XA11Y_TEST_APP={app_name!r}. Known apps: {', '.join(KNOWN_APPS)}"
        )

    # `replace` rather than rebuilding field by field: the latter silently
    # drops any field not listed, which is a bug waiting for the first builder
    # that sets `reset` or `app_name_prefix`.
    return replace(
        builder(),
        startup_timeout=STARTUP_TIMEOUT,
        frontmost=app_name in FRONTMOST_APPS,
        label=app_name,
    )
