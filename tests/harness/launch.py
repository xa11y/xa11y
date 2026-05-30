"""Shared test session harness for xa11y integration tests.

Launches the test app once and runs all requested language suites against
it in sequence, so that the app process is not repeatedly spawned/torn down
between language suites.

CLI usage:
    python tests/harness/launch.py <app> [suite ...]

    <app>     one of: qt, gtk, cocoa, tauri, electron, accesskit, egui
    [suite]   optional subset: python js cli  (default: all applicable)

Programmatic usage:
    from tests.harness.launch import run
    exit_code = run("qt", suites=["python", "js"])
"""

from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Sequence

# ---------------------------------------------------------------------------
# Repository layout helpers
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent

STARTUP_TIMEOUT = 30  # seconds


# ---------------------------------------------------------------------------
# App definitions
# ---------------------------------------------------------------------------

def _app_command(app: str) -> tuple[list[str], dict[str, str], list[str], str | None]:
    """Return (command, env_overrides, candidate_app_names, content_ready_selector) for the given app.

    Raises ValueError for unknown app names or platform-unsupported apps.
    """
    if app == "qt":
        script = str(PROJECT_ROOT / "test-apps" / "qt" / "app.py")
        return (
            [sys.executable, script],
            {"QT_ACCESSIBILITY": "1"},
            ["xa11y-qt-test-app", "xa11y", "python3", "python", "Python", "app.py"],
            None,
        )

    if app == "gtk":
        if sys.platform == "darwin":
            raise ValueError("gtk app is not supported on macOS")
        if sys.platform == "win32":
            raise ValueError("gtk app is not supported on Windows")
        script = str(PROJECT_ROOT / "test-apps" / "gtk" / "app.py")
        return (
            [sys.executable, script],
            {},
            ["xa11y-gtk-test-app", "gtk-test-app", "python3", "python", "Python", "app.py"],
            None,
        )

    if app == "cocoa":
        if sys.platform != "darwin":
            raise ValueError("cocoa app is only supported on macOS")
        binary = str(PROJECT_ROOT / "test-apps" / "cocoa" / "xa11y-cocoa-test-app")
        return (
            [binary, "--headless"],
            {},
            ["xa11y-cocoa-test-app"],
            None,
        )

    if app == "tauri":
        binary = str(
            PROJECT_ROOT / "test-apps" / "tauri" / "target" / "debug" / "xa11y-tauri-test-app"
        )
        return (
            [binary],
            {},
            ["xa11y-tauri-test-app"],
            'button[name="OK"]',
        )

    if app == "electron":
        electron_bin = str(
            PROJECT_ROOT / "test-apps" / "electron" / "node_modules" / ".bin" / "electron"
        )
        main_js = str(PROJECT_ROOT / "test-apps" / "electron" / "main.js")
        return (
            [electron_bin, main_js, "--no-sandbox", "--force-renderer-accessibility"],
            {},
            ["xa11y-electron-test-app", "electron"],
            'button[name="OK"]',
        )

    if app == "accesskit":
        # The AccessKit test app is part of the Cargo workspace and is built
        # by `cargo build -p xa11y-test-app`. The binary lands in the workspace
        # target directory. We do NOT build it here — the caller is responsible
        # for ensuring it exists (mirrors scripts/run_js_tests.sh).
        binary = str(PROJECT_ROOT / "target" / "debug" / "xa11y-test-app")
        return (
            [binary, "--headless"],
            {},
            ["xa11y-test-app", "xa11y Test App"],
            None,
        )

    if app == "egui":
        # The egui test app sits outside the Cargo workspace (its eframe
        # dependency tree is heavy and slows workspace-wide builds). Build
        # it explicitly via `cargo build --manifest-path test-apps/egui/Cargo.toml`.
        binary = str(
            PROJECT_ROOT / "test-apps" / "egui" / "target" / "debug" / "xa11y-egui-test-app"
        )
        return (
            [binary],
            {},
            ["xa11y-egui-test-app"],
            'button[name="OK"]',
        )

    raise ValueError(
        f"Unknown app: {app!r}. Supported: qt, gtk, cocoa, tauri, electron, accesskit, egui"
    )


# ---------------------------------------------------------------------------
# Linux accessibility setup
# ---------------------------------------------------------------------------

def _find_atspi_binary(*candidates: str) -> str | None:
    """Return the first candidate that exists, or None."""
    import shutil
    for candidate in candidates:
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
        found = shutil.which(candidate)
        if found:
            return found
    return None


def _setup_linux_a11y() -> None:
    """Spawn AT-SPI2 infrastructure and enable accessibility on Linux.

    Mirrors the setup done in scripts/run_qt_tests.sh.
    Only called when DBUS_SESSION_BUS_ADDRESS is set (i.e. a D-Bus session
    is available).
    """
    os.environ["NO_AT_BRIDGE"] = "0"
    os.environ["AT_SPI_CLIENT"] = "true"
    os.environ["ACCESSIBILITY_ENABLED"] = "1"

    launcher = _find_atspi_binary(
        "/usr/libexec/at-spi-bus-launcher", "at-spi-bus-launcher"
    )
    if launcher:
        subprocess.Popen(
            [launcher, "--launch-immediately"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    else:
        print("WARNING: at-spi-bus-launcher not found, AT-SPI2 may not work")
    time.sleep(1)

    registryd = _find_atspi_binary(
        "/usr/libexec/at-spi2-registryd", "at-spi2-registryd"
    )
    if registryd:
        subprocess.Popen(
            [registryd],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    else:
        print("WARNING: at-spi2-registryd not found")
    time.sleep(1)

    subprocess.run(
        [
            "dbus-send", "--session", "--print-reply",
            "--dest=org.a11y.Bus", "/org/a11y/bus",
            "org.freedesktop.DBus.Properties.Set",
            "string:org.a11y.Status", "string:IsEnabled", "variant:boolean:true",
        ],
        check=False,
    )
    subprocess.run(
        [
            "dbus-send", "--session", "--print-reply",
            "--dest=org.a11y.Bus", "/org/a11y/bus",
            "org.freedesktop.DBus.Properties.Set",
            "string:org.a11y.Status", "string:ScreenReaderEnabled", "variant:boolean:true",
        ],
        check=False,
    )


# ---------------------------------------------------------------------------
# App launch + accessibility discovery
# ---------------------------------------------------------------------------

def _launch_app(
    app: str,
) -> tuple[subprocess.Popen[bytes], str]:
    """Start the test app subprocess and wait for it to become visible.

    Returns (proc, discovered_app_name).

    Raises RuntimeError if the app does not appear within STARTUP_TIMEOUT.
    """
    command, env_overrides, app_names, content_ready_selector = _app_command(app)

    env = os.environ.copy()
    env.update(env_overrides)

    print(f"Launching {app} test app: {' '.join(command)}")
    proc = subprocess.Popen(
        command,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    import xa11y  # imported late so the module is optional for callers that don't need it

    discovered_name: str | None = None
    deadline = time.monotonic() + STARTUP_TIMEOUT
    last_err = None

    while time.monotonic() < deadline:
        if proc.poll() is not None:
            out = proc.stdout.read().decode() if proc.stdout else ""
            err = proc.stderr.read().decode() if proc.stderr else ""
            raise RuntimeError(
                f"Test app (pid={proc.pid}) exited early (code {proc.returncode}).\n"
                f"stdout: {out}\nstderr: {err}"
            )

        # Try exact match first (fast path on Linux/macOS).
        for name in app_names:
            try:
                xa11y.App.by_name(name)
                discovered_name = name
                break
            except (xa11y.SelectorNotMatchedError, xa11y.PlatformError):
                pass

        # Fall back to PID-based lookup to avoid false matches against the
        # harness's own Python process when substring matching.
        if discovered_name is None:
            try:
                xa11y.App.by_pid(proc.pid)
                discovered_name = app_names[0]
            except (xa11y.SelectorNotMatchedError, xa11y.PlatformError) as exc:
                last_err = exc

        if discovered_name is not None:
            break

        time.sleep(0.5)

    if discovered_name is None:
        try:
            all_apps = xa11y.App.list()
            app_list = [(a.name, a.pid) for a in all_apps]
        except Exception:
            app_list = "<failed to list>"
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        out = proc.stdout.read().decode() if proc.stdout else ""
        err = proc.stderr.read().decode() if proc.stderr else ""
        raise RuntimeError(
            f"Test app (pid={proc.pid}) not found after {STARTUP_TIMEOUT}s.\n"
            f"Last error: {last_err}\n"
            f"Available apps: {app_list}\n"
            f"stdout: {out}\nstderr: {err}"
        )

    print(f"Test app visible: {discovered_name!r} (pid={proc.pid})")

    # Wait for content to be ready if a selector was specified.
    if content_ready_selector is not None:
        print(f"Waiting for content: {content_ready_selector!r}")
        content_ready = False
        while time.monotonic() < deadline:
            try:
                xa11y.App.by_pid(proc.pid).locator(content_ready_selector).element()
                content_ready = True
                break
            except (xa11y.SelectorNotMatchedError, xa11y.PlatformError):
                time.sleep(0.5)
        if not content_ready:
            print(f"WARNING: content selector {content_ready_selector!r} not ready after timeout; proceeding anyway")

    return proc, discovered_name


def _kill_app(proc: subprocess.Popen[bytes]) -> None:
    """Terminate the app process gracefully, escalating to SIGKILL if needed."""
    try:
        if sys.platform == "win32":
            proc.terminate()
        else:
            proc.send_signal(signal.SIGTERM)
        proc.wait(timeout=5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        proc.kill()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass


# ---------------------------------------------------------------------------
# CLI binary discovery
# ---------------------------------------------------------------------------

def _find_cli_binary() -> str | None:
    """Return the path to the xa11y CLI binary, or None if not found."""
    # Prefer the workspace debug build.
    debug_bin = PROJECT_ROOT / "target" / "debug" / "xa11y"
    if debug_bin.exists():
        return str(debug_bin)
    release_bin = PROJECT_ROOT / "target" / "release" / "xa11y"
    if release_bin.exists():
        return str(release_bin)
    # Fall back to whatever is on PATH.
    import shutil
    return shutil.which("xa11y")


# ---------------------------------------------------------------------------
# Suite runner
# ---------------------------------------------------------------------------

def _suite_command(suite: str) -> list[str]:
    """Build the command to run a suite. JS test files are listed explicitly
    because Node's ``--test`` flag does not auto-discover files in a directory
    on all supported versions.
    """
    if suite == "python":
        return [sys.executable, "-m", "pytest", "tests/suites/python/", "-v"]
    if suite == "cli":
        return [sys.executable, "-m", "pytest", "tests/suites/cli/", "-v"]
    if suite == "js":
        js_files = sorted(
            str(p.relative_to(PROJECT_ROOT))
            for p in (PROJECT_ROOT / "tests" / "suites" / "js").glob("*.test.js")
        )
        return ["node", "--test", *js_files]
    raise ValueError(f"Unknown suite: {suite!r}")


def _run_suites(
    app: str,
    suites: list[str],
    proc: subprocess.Popen[bytes],
    discovered_name: str,
) -> int:
    """Run each requested suite serially and return the worst exit code."""
    env = os.environ.copy()
    env["XA11Y_TEST_APP"] = app
    env["XA11Y_TEST_APP_PID"] = str(proc.pid)
    env["XA11Y_TEST_APP_NAME"] = discovered_name

    worst_rc = 0

    # Per-app suite skips. The AccessKit app's widget schema differs from the
    # shared OK-button fixtures, so the CLI and JS suites (which have their own
    # AccessKit coverage) stay skipped for it.
    #
    # The Python suite IS wired up for AccessKit (full APP_CONFIGS entry:
    # Submit/Cancel schema, single checkbox, no dialog), but only on Linux.
    # Linux is the one platform where AccessKit's AT-SPI bridge
    # (accesskit_unix) is exercised, and where its hardcoded
    # "click"-not-"toggle" action naming makes the toggle()-via-press fallback
    # in xa11y-linux/src/atspi.rs load-bearing — exactly the gap that went
    # uncaught before (see tests/matrix.yaml accesskit_python_compat_on_linux).
    # On macOS/Windows the Rust integ suite remains the canonical AccessKit
    # coverage, so the Python suite is skipped there.
    accesskit_skips = {"cli", "js"}
    if sys.platform != "linux":
        accesskit_skips.add("python")
    suite_skips_by_app = {
        "accesskit": accesskit_skips,
    }

    for suite in suites:
        if suite in suite_skips_by_app.get(app, set()):
            print(
                f"\nSkipping {suite} suite for {app} "
                f"(see suite_skips_by_app in tests/harness/launch.py)"
            )
            continue

        if suite == "cli":
            cli_bin = _find_cli_binary()
            if cli_bin is None:
                print(
                    f"WARNING: xa11y CLI binary not found; skipping 'cli' suite. "
                    f"Build it with: cargo build -p xa11y"
                )
                continue
            suite_env = {**env, "XA11Y_CLI": cli_bin}
        else:
            suite_env = env

        cmd = _suite_command(suite)
        print(f"\n=== Running {suite} suite against {app} ===\n")
        result = subprocess.run(cmd, env=suite_env, cwd=str(PROJECT_ROOT))
        rc = result.returncode
        if rc != 0:
            print(f"\n--- {suite} suite exited with code {rc} ---")
        if rc > worst_rc:
            worst_rc = rc

    return worst_rc


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

VALID_APPS = ("qt", "gtk", "cocoa", "tauri", "electron", "accesskit", "egui")
VALID_SUITES = ("python", "js", "cli")
DEFAULT_SUITES = list(VALID_SUITES)


def run(app_name: str, suites: Sequence[str] | None = None) -> int:
    """Launch app_name, run suites against it, then stop the app.

    Args:
        app_name: One of the supported app identifiers.
        suites:   Ordered list of suite names to run. Defaults to all three.

    Returns:
        The worst (highest) exit code from all suite runs, or 0 if all passed.
    """
    if app_name not in VALID_APPS:
        raise ValueError(f"Unknown app {app_name!r}. Valid apps: {', '.join(VALID_APPS)}")

    suite_list = list(suites) if suites is not None else DEFAULT_SUITES
    for s in suite_list:
        if s not in VALID_SUITES:
            raise ValueError(f"Unknown suite {s!r}. Valid suites: {', '.join(VALID_SUITES)}")

    if sys.platform == "linux" and os.environ.get("DBUS_SESSION_BUS_ADDRESS"):
        _setup_linux_a11y()

    proc: subprocess.Popen[bytes] | None = None
    try:
        proc, discovered_name = _launch_app(app_name)
        return _run_suites(app_name, suite_list, proc, discovered_name)
    except KeyboardInterrupt:
        print("\nInterrupted by user.")
        return 130
    finally:
        if proc is not None:
            _kill_app(proc)
            print(f"Test app (pid={proc.pid}) stopped.")


# ---------------------------------------------------------------------------
# CLI entry point
# ---------------------------------------------------------------------------

def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python tests/harness/launch.py",
        description=(
            "Launch a test app once and run all requested language suites "
            "against it, then shut the app down."
        ),
    )
    parser.add_argument(
        "app",
        choices=VALID_APPS,
        help="Which test app to launch.",
    )
    parser.add_argument(
        "suites",
        nargs="*",
        metavar="suite",
        help=(
            f"Suites to run: {', '.join(VALID_SUITES)}. "
            "Defaults to all three if omitted."
        ),
    )
    return parser


def main(argv: list[str] | None = None) -> None:
    parser = _build_parser()
    args = parser.parse_args(argv)

    requested_suites: list[str] = args.suites or list(DEFAULT_SUITES)

    for s in requested_suites:
        if s not in VALID_SUITES:
            parser.error(f"Invalid suite {s!r}. Choose from: {', '.join(VALID_SUITES)}")

    try:
        exit_code = run(args.app, requested_suites)
    except ValueError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        sys.exit(1)

    sys.exit(exit_code)


if __name__ == "__main__":
    main()
