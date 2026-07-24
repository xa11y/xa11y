"""Shared test session harness for xa11y integration tests.

Launches the test app once and runs all requested language suites against
it in sequence, so that the app process is not repeatedly spawned/torn down
between language suites.

CLI usage:
    python tests/harness/launch.py <app> [suite ...]

    <app>     one of: qt, gtk, cocoa, tauri, electron, accesskit, egui, winforms
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
import tempfile
import time
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Sequence

# ---------------------------------------------------------------------------
# Repository layout helpers
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent

# Overall app-startup / content-readiness deadline, in seconds. Overridable
# for slow machines and loaded CI runners (matches tests/helpers.py).
STARTUP_TIMEOUT = float(os.environ.get("XA11Y_TEST_STARTUP_TIMEOUT", "30"))

# Apps with a real, activatable macOS window whose tests depend on holding the
# frontmost slot — input_sim delivers CGEvents to the frontmost app, and focus
# assertions read OS focus state. We actively claim the front before tests run
# (see tests.helpers.ensure_macos_frontmost) instead of reactively killing a
# hardcoded list of focus-stealing onboarding processes (issue #230). Excluded:
# cocoa (--headless, accessory app — can't be frontmost) and accesskit
# (synthesises its own focus events; its macOS coverage is the Rust integ
# suite, and the Python harness skips all its suites on macOS anyway).
_MACOS_FRONTMOST_APPS = {"tauri", "qt", "electron", "egui"}


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

    if app == "winforms":
        if sys.platform != "win32":
            raise ValueError("winforms app is only supported on Windows")
        # Built by `dotnet build test-apps/winforms` (the caller's job — CI has
        # a build step, scripts/run_app_suite.sh builds it locally). The
        # `net8.0-windows` path segment must track TargetFramework in
        # test-apps/winforms/xa11y-winforms-test-app.csproj.
        binary = str(
            PROJECT_ROOT
            / "test-apps"
            / "winforms"
            / "bin"
            / "Debug"
            / "net8.0-windows"
            / "xa11y-winforms-test-app.exe"
        )
        return (
            [binary],
            {},
            ["xa11y-winforms-test-app"],
            'button[name="OK"]',
        )

    raise ValueError(
        f"Unknown app: {app!r}. "
        f"Supported: qt, gtk, cocoa, tauri, electron, accesskit, egui, winforms"
    )


# ---------------------------------------------------------------------------
# Linux accessibility setup
# ---------------------------------------------------------------------------

def _setup_linux_a11y() -> None:
    """Spawn AT-SPI2 infrastructure and enable accessibility on Linux.

    Delegates to scripts/setup_linux_a11y.sh — the single source of truth for
    AT-SPI bring-up, shared with the standalone shell harnesses and the
    setup-a11y CI action. The daemons it backgrounds survive past the script's
    own exit, so they stay up for the suites we launch afterwards.

    Only called when a D-Bus session exists and the environment hasn't already
    been prepared (XA11Y_A11Y_READY). When CI's setup-a11y action — or a
    wrapping `source scripts/setup_linux_a11y.sh` — already brought AT-SPI up,
    we skip this so we don't start a second copy of the daemons.
    """
    # These three flags must live in *our* process env (and our children's), so
    # set them here rather than relying on the sourced script's exports, which
    # wouldn't propagate back across the subprocess boundary.
    os.environ["NO_AT_BRIDGE"] = "0"
    os.environ["AT_SPI_CLIENT"] = "true"
    os.environ["ACCESSIBILITY_ENABLED"] = "1"

    script = PROJECT_ROOT / "scripts" / "setup_linux_a11y.sh"
    subprocess.run(["bash", str(script)], check=False)


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

    deadline = time.monotonic() + STARTUP_TIMEOUT

    # Discovery is a single waited call: `App.find` polls the accessibility
    # API internally, so the harness no longer hand-rolls a retry loop. Match
    # on the PID we just launched *or* any of the platform's candidate names
    # (the process name on Linux/macOS, the window title on Windows) — matching
    # either absorbs the per-platform registration races in one place.
    def _is_test_app(candidate: "xa11y.App") -> bool:
        return candidate.pid == proc.pid or candidate.name in app_names

    try:
        app = xa11y.App.find(_is_test_app, timeout=STARTUP_TIMEOUT)
    except (xa11y.SelectorNotMatchedError, xa11y.PlatformError) as exc:
        # Distinguish "app crashed on launch" from "app never registered".
        if proc.poll() is not None:
            out = proc.stdout.read().decode() if proc.stdout else ""
            err = proc.stderr.read().decode() if proc.stderr else ""
            raise RuntimeError(
                f"Test app (pid={proc.pid}) exited early (code {proc.returncode}).\n"
                f"stdout: {out}\nstderr: {err}"
            ) from exc
        try:
            app_list = [(a.name, a.pid) for a in xa11y.App.list()]
        except Exception:
            app_list = "<failed to list>"
        _kill_app(proc)
        out = proc.stdout.read().decode() if proc.stdout else ""
        err = proc.stderr.read().decode() if proc.stderr else ""
        raise RuntimeError(
            f"Test app (pid={proc.pid}) not found after {STARTUP_TIMEOUT}s.\n"
            f"Last error: {exc}\n"
            f"Available apps: {app_list}\n"
            f"stdout: {out}\nstderr: {err}"
        ) from exc

    discovered_name = app.name or app_names[0]
    print(f"Test app visible: {discovered_name!r} (pid={proc.pid})")

    # Wait for content to be ready if a selector was specified — again one
    # library call (`wait_attached`) rather than a manual poll loop.
    if content_ready_selector is not None:
        print(f"Waiting for content: {content_ready_selector!r}")
        remaining = max(deadline - time.monotonic(), 1.0)
        try:
            app.locator(content_ready_selector).wait_attached(timeout=remaining)
        except (xa11y.SelectorNotMatchedError, xa11y.TimeoutError, xa11y.PlatformError):
            print(
                f"WARNING: content selector {content_ready_selector!r} not ready "
                f"after timeout; proceeding anyway"
            )

    # macOS: claim the frontmost slot before handing the app to the suites, so
    # input_sim/focus tests aren't silently misdirected to whatever onboarding
    # process the runner image booted with (issue #230).
    if sys.platform == "darwin" and app in _MACOS_FRONTMOST_APPS:
        if str(PROJECT_ROOT) not in sys.path:
            sys.path.insert(0, str(PROJECT_ROOT))
        from tests.helpers import ensure_macos_frontmost

        print(f"Ensuring test app (pid={proc.pid}) is frontmost (macOS)...")
        ok, detail = ensure_macos_frontmost(proc.pid)
        if not ok:
            _kill_app(proc)
            raise RuntimeError(detail)
        print("Test app is frontmost.")

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


def _check_pytest_ran_tests(suite: str, junit_path: Path) -> int:
    """Guard against a matrix cell silently running zero tests.

    pytest exits 5 when it *collects* nothing, which already fails the cell.
    The subtler gap: a run where every collected test was skipped exits 0 and
    looks green while covering nothing. Parse the junit report and require at
    least one executed (non-skipped) test.

    Returns 0 when the suite really executed tests, 1 otherwise.
    """
    try:
        root = ET.parse(junit_path).getroot()
    except (ET.ParseError, OSError) as exc:
        print(
            f"ERROR: {suite} suite produced no readable junit report "
            f"({exc}); cannot verify that any tests ran."
        )
        return 1

    total = skipped = 0
    # The root is <testsuites> (or a bare <testsuite>); iter() covers both.
    for ts in root.iter("testsuite"):
        total += int(ts.get("tests", 0))
        skipped += int(ts.get("skipped", 0))

    if total == 0:
        print(f"ERROR: {suite} suite reported zero collected tests.")
        return 1
    if skipped >= total:
        print(
            f"ERROR: {suite} suite skipped all {total} collected tests — "
            f"this matrix cell exercised nothing."
        )
        return 1
    print(f"{suite} suite executed {total - skipped} tests ({skipped} skipped).")
    return 0


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

        # pytest-based suites get a junit report so we can verify the cell
        # actually executed tests (see _check_pytest_ran_tests).
        junit_path: Path | None = None
        if suite in ("python", "cli"):
            fd, junit_name = tempfile.mkstemp(prefix=f"xa11y-{suite}-junit-", suffix=".xml")
            os.close(fd)
            junit_path = Path(junit_name)
            cmd = [*cmd, f"--junitxml={junit_path}"]

        print(f"\n=== Running {suite} suite against {app} ===\n")
        result = subprocess.run(cmd, env=suite_env, cwd=str(PROJECT_ROOT))
        rc = result.returncode
        if junit_path is not None:
            if rc == 0:
                rc = _check_pytest_ran_tests(suite, junit_path)
            junit_path.unlink(missing_ok=True)
        if rc != 0:
            print(f"\n--- {suite} suite exited with code {rc} ---")
        if rc > worst_rc:
            worst_rc = rc

    return worst_rc


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

VALID_APPS = ("qt", "gtk", "cocoa", "tauri", "electron", "accesskit", "egui", "winforms")
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

    if (
        sys.platform == "linux"
        and os.environ.get("DBUS_SESSION_BUS_ADDRESS")
        and not os.environ.get("XA11Y_A11Y_READY")
    ):
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
