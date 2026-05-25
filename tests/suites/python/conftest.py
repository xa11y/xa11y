"""Shared fixtures for feature-based xa11y integration tests.

Reads XA11Y_TEST_APP to select the target app (default: tauri). If
XA11Y_TEST_APP_PID is set, connects to an already-running process; otherwise
launches the app using the same launch logic as the original per-app conftest
files.

App-specific widget names/values that differ across toolkits live in
APP_CONFIGS and are exposed via the ``app_config`` session-scoped fixture.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest
import xa11y

from tests.helpers import launch_test_app

# ---------------------------------------------------------------------------
# App-specific configuration
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent.parent

# Per-app configuration dict. Each entry describes how widget names and
# selectors differ for that toolkit. Tests use ``app_config`` to adapt.
APP_CONFIGS: dict[str, dict] = {
    "qt": {
        # Dialog
        "dialog_button_name": "Open Dialog",
        "dialog_name": "Sample Dialog",
        # Buttons
        "ok_button_name": "OK",
        "cancel_button_name": "Cancel",
        "ok_button_description": "Confirm the dialog",
        # Checkboxes
        "has_checkbox": True,
        "checkbox_unchecked_name": "Agree to terms",
        "checkbox_checked_name": "Subscribe",
        # Radio buttons
        "has_radio": True,
        "radio_role": "radio_button",
        "radio_a_name": "Option A",
        "radio_b_name": "Option B",
        # Slider — found by name
        "slider_selector": 'slider[name="Volume"]',
        "slider_initial_value": 50.0,
        "slider_min": 0.0,
        "slider_max": 100.0,
        # Spin button — found by name
        "spinbutton_selector": 'spin_button[name="Quantity"]',
        # Progress bar — found by name
        "progress_bar_selector": 'progress_bar[name="Progress"]',
        # Text field — found by name
        "textfield_selector": 'text_field[name="Search"]',
        "textfield_initial_value": None,  # Qt does not guarantee specific text
        # Text area
        "textarea_selector": '[name="Notes"]',
        # Window
        "window_name_contains": "xa11y-qt-test-app",
        # Dynamic buttons for event tests
        "submit_button_name": "Submit",
        "add_item_button_name": "Add Item",
        "remove_item_button_name": "Remove Item",
    },
    "gtk": {
        "dialog_button_name": None,
        "dialog_name": None,
        "ok_button_name": "OK",
        "cancel_button_name": "Cancel",
        "ok_button_description": None,
        "has_checkbox": True,
        "checkbox_unchecked_name": "Agree to terms",
        "checkbox_checked_name": "Subscribe",
        # GTK4 radio buttons use check_box role
        "has_radio": True,
        "radio_role": "check_box",
        "radio_a_name": "Option A",
        "radio_b_name": "Option B",
        "slider_selector": "slider",  # GTK doesn't reliably expose AX labels
        "slider_initial_value": 50.0,
        "slider_min": 0.0,
        "slider_max": 100.0,
        "spinbutton_selector": "spin_button",
        "progress_bar_selector": "progress_bar",
        "textfield_selector": "text_field",
        "textfield_initial_value": "hello world",
        "textarea_selector": "text_area",
        "window_name_contains": None,
        "submit_button_name": None,   # GTK test app has no Submit button
        "add_item_button_name": None,
        "remove_item_button_name": None,
    },
    "cocoa": {
        "dialog_button_name": None,
        "dialog_name": None,
        "ok_button_name": "OK",
        "cancel_button_name": "Cancel",
        "ok_button_description": "Confirm the dialog",
        "has_checkbox": True,
        "checkbox_unchecked_name": "Agree to terms",
        "checkbox_checked_name": "Subscribe",
        "has_radio": True,
        "radio_role": "radio_button",
        "radio_a_name": "Option A",
        "radio_b_name": "Option B",
        "slider_selector": 'slider[name="Volume"]',
        "slider_initial_value": 50.0,
        "slider_min": 0.0,
        "slider_max": 100.0,
        "spinbutton_selector": 'spin_button[name="Quantity"]',
        "progress_bar_selector": 'progress_bar[name="Progress"]',
        "textfield_selector": 'text_field[name="Search"]',
        "textfield_initial_value": "hello world",
        "textarea_selector": 'text_area[name="Notes"]',
        "window_name_contains": None,
        "submit_button_name": "Submit",
        "add_item_button_name": "Add Item",
        "remove_item_button_name": "Remove Item",
    },
    "tauri": {
        "dialog_button_name": None,
        "dialog_name": None,
        "ok_button_name": "OK",
        "cancel_button_name": "Cancel",
        "ok_button_description": None,
        "has_checkbox": True,
        "checkbox_unchecked_name": "Agree to terms",
        "checkbox_checked_name": "Subscribe",
        "has_radio": True,
        "radio_role": "radio_button",
        "radio_a_name": "Option A",
        "radio_b_name": "Option B",
        "slider_selector": 'slider[name="Volume"]',
        "slider_initial_value": 50.0,
        "slider_min": 0.0,
        "slider_max": 100.0,
        "spinbutton_selector": None,  # Tauri test app has no spin_button
        "progress_bar_selector": 'progress_bar[name="Progress"]',
        "textfield_selector": 'text_field[name="Search"]',
        "textfield_initial_value": "hello world",
        "textarea_selector": 'text_area[name="Notes"]',
        "window_name_contains": None,
        "submit_button_name": "Submit",
        "add_item_button_name": "Add Item",
        "remove_item_button_name": "Remove Item",
    },
    "electron": {
        "dialog_button_name": None,
        "dialog_name": None,
        "ok_button_name": "OK",
        "cancel_button_name": "Cancel",
        "ok_button_description": None,
        "has_checkbox": True,
        "checkbox_unchecked_name": "Agree to terms",
        "checkbox_checked_name": None,  # Electron test app has only one checkbox
        "has_radio": False,
        "radio_role": None,
        "radio_a_name": None,
        "radio_b_name": None,
        "slider_selector": None,       # Electron test app has no slider
        "slider_initial_value": None,
        "slider_min": None,
        "slider_max": None,
        "spinbutton_selector": None,   # Electron test app has no spin_button
        "progress_bar_selector": None, # Electron test app has no progress_bar
        "textfield_selector": 'text_field[name="Search"]',
        "textfield_initial_value": "hello world",
        "textarea_selector": None,     # Electron test app has no text_area
        "window_name_contains": None,
        "submit_button_name": None,    # Electron test app has no Submit button
        "add_item_button_name": None,
        "remove_item_button_name": None,
    },
}

# ---------------------------------------------------------------------------
# Launch helpers per app type
# ---------------------------------------------------------------------------


def _launch_qt() -> xa11y.App:
    script = str(PROJECT_ROOT / "test-apps" / "qt" / "app.py")
    pid_file = tempfile.mktemp(suffix=".pid")
    yield from launch_test_app(
        command=[sys.executable, script, "--pid-file", pid_file],
        app_names=["xa11y-qt-test-app", "xa11y", "python3", "python", "Python", "app.py"],
        env_overrides={"QT_ACCESSIBILITY": "1"},
    )


def _launch_gtk() -> xa11y.App:
    script = str(PROJECT_ROOT / "test-apps" / "gtk" / "app.py")
    pid_file = tempfile.mktemp(suffix=".pid")
    yield from launch_test_app(
        command=[sys.executable, script, "--pid-file", pid_file],
        app_names=["xa11y-gtk-test-app", "gtk-test-app", "python3", "python", "Python", "app.py"],
    )


def _launch_cocoa() -> xa11y.App:
    binary = str(PROJECT_ROOT / "test-apps" / "cocoa" / "xa11y-cocoa-test-app")
    binary_path = Path(binary)
    if not binary_path.exists():
        makefile_dir = binary_path.parent
        result = subprocess.run(
            ["make", "build"],
            cwd=makefile_dir,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.fail(
                f"Failed to build Cocoa test app:\n{result.stdout}\n{result.stderr}"
            )
    pid_file = tempfile.mktemp(suffix=".pid")
    yield from launch_test_app(
        command=[binary, "--headless", "--pid-file", pid_file],
        app_names=["xa11y-cocoa-test-app"],
    )


def _launch_tauri() -> xa11y.App:
    binary = str(
        PROJECT_ROOT / "test-apps" / "tauri" / "target" / "debug" / "xa11y-tauri-test-app"
    )
    if not Path(binary).exists():
        result = subprocess.run(
            [
                "cargo",
                "build",
                "--manifest-path",
                str(PROJECT_ROOT / "test-apps" / "tauri" / "Cargo.toml"),
            ],
            cwd=PROJECT_ROOT,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.fail(
                f"Failed to build Tauri test app:\n{result.stdout}\n{result.stderr}"
            )
    yield from launch_test_app(
        command=[binary],
        app_names=["xa11y-tauri-test-app"],
        content_ready_selector='button[name="OK"]',
    )


def _launch_electron() -> xa11y.App:
    electron_dir = PROJECT_ROOT / "test-apps" / "electron"
    npm = "npm.cmd" if sys.platform == "win32" else "npm"
    node_modules_electron = electron_dir / "node_modules" / ".bin" / "electron"
    main_js = str(electron_dir / "main.js")

    # Install node_modules if missing.
    if not node_modules_electron.exists():
        result = subprocess.run(
            [npm, "install", "--no-audit", "--no-fund", "--silent"],
            cwd=str(electron_dir),
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.fail(
                f"Failed to install Electron dependencies:\n{result.stdout}\n{result.stderr}"
            )

    yield from launch_test_app(
        command=[str(node_modules_electron), main_js, "--force-renderer-accessibility"],
        app_names=["xa11y-electron-test-app", "Electron", "xa11y"],
        content_ready_selector='button[name="OK"]',
    )


_LAUNCHERS = {
    "qt": _launch_qt,
    "gtk": _launch_gtk,
    "cocoa": _launch_cocoa,
    "tauri": _launch_tauri,
    "electron": _launch_electron,
}


# ---------------------------------------------------------------------------
# Diagnostic capture for macOS input_sim flakes
# ---------------------------------------------------------------------------
#
# When CGEvents fail to reach the Tauri WKWebView, every test in
# test_input_sim.py fails identically with `assert 'mousedown' in ''`, which
# tells us nothing about why. The hook below snapshots runtime state on the
# first such failure (which macOS app is frontmost, hit-target bounds, event
# log contents, plus a re-probe) and attaches it to the failure report — so
# the next round of failures arrives with breadcrumbs in the CI log.
#
# This hook is what surfaced the Setup-Assistant-stealing-focus cause on
# Actions runner images. Worth keeping for the next class of flake on the
# same path.

def _macos_frontmost_app() -> str:
    """Return the name of the macOS frontmost application (or an error tag)."""
    import subprocess
    try:
        result = subprocess.run(
            ["osascript", "-e",
             'tell application "System Events" to '
             'get name of first application process whose frontmost is true'],
            capture_output=True, text=True, timeout=5,
        )
        if result.returncode == 0:
            return result.stdout.strip() or "<empty>"
        return f"<osascript rc={result.returncode}: {result.stderr.strip()}>"
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError) as exc:
        return f"<error: {exc!r}>"


def _macos_visible_processes() -> str:
    """List names of all foreground-capable macOS processes."""
    import subprocess
    try:
        result = subprocess.run(
            ["osascript", "-e",
             'tell application "System Events" to '
             'get name of (every application process whose visible is true)'],
            capture_output=True, text=True, timeout=5,
        )
        if result.returncode == 0:
            return result.stdout.strip() or "<empty>"
        return f"<osascript rc={result.returncode}: {result.stderr.strip()}>"
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError) as exc:
        return f"<error: {exc!r}>"


def _collect_macos_input_sim_diagnostics(app) -> str:
    """Snapshot state useful for diagnosing CGEvent → WKWebView delivery flakes."""
    import time
    lines = ["macOS input_sim failure diagnostics:"]
    lines.append(f"  frontmost: {_macos_frontmost_app()}")
    lines.append(f"  visible processes: {_macos_visible_processes()}")

    try:
        lines.append(f"  app.pid: {app.pid}")
        lines.append(f"  app.name: {app.name!r}")
    except Exception as exc:  # noqa: BLE001
        lines.append(f"  app.{{pid,name}}: <error {exc!r}>")

    try:
        wb = app.locator('window').element().bounds
        lines.append(
            f"  window bounds: x={wb.x} y={wb.y} w={wb.width} h={wb.height}"
            if wb else "  window bounds: <None>"
        )
    except Exception as exc:  # noqa: BLE001
        lines.append(f"  window bounds: <error {exc!r}>")

    try:
        hb = app.locator('button[name="Hit target"]').element().bounds
        if hb:
            lines.append(
                f"  hit_target bounds: x={hb.x} y={hb.y} w={hb.width} h={hb.height}"
            )
            lines.append(
                f"  hit_target center: "
                f"({hb.x + hb.width // 2}, {hb.y + hb.height // 2})"
            )
        else:
            lines.append("  hit_target bounds: <None>")
    except Exception as exc:  # noqa: BLE001
        lines.append(f"  hit_target: <error {exc!r}>")

    try:
        log_val = app.locator('text_area[name="Event log"]').element().value
        lines.append(f"  event_log (len={len(log_val or '')}): {(log_val or '')!r}")
    except Exception as exc:  # noqa: BLE001
        lines.append(f"  event_log: <error {exc!r}>")

    # Re-probe: post one fresh click and see if it lands now. Disambiguates
    # "the test click happened to lose a race" from "the macOS session can't
    # deliver CGEvents to this WKWebView at all".
    try:
        sim = xa11y.input_sim()
        hb = app.locator('button[name="Hit target"]').element().bounds
        if hb is None:
            lines.append("  reprobe: <no bounds>")
        else:
            app.locator('button[name="Clear log"]').press()
            time.sleep(0.2)
            sim.click((hb.x + hb.width // 2, hb.y + hb.height // 2))
            time.sleep(0.5)
            log_after = app.locator('text_area[name="Event log"]').element().value or ""
            lines.append(
                f"  reprobe click → log (len={len(log_after)}): {log_after!r}"
            )
    except Exception as exc:  # noqa: BLE001
        lines.append(f"  reprobe: <error {exc!r}>")

    return "\n".join(lines)


@pytest.hookimpl(hookwrapper=True, tryfirst=True)
def pytest_runtest_makereport(item, call):
    outcome = yield
    rep = outcome.get_result()
    if (rep.when != "call" or not rep.failed
            or sys.platform != "darwin"
            or "test_input_sim" not in item.nodeid):
        return
    # Diagnostics are the same for every test in the module; emit once per
    # session so the failure section doesn't repeat the snapshot N times.
    session = item.session
    if getattr(session, "_macos_input_sim_diag_emitted", False):
        return
    session._macos_input_sim_diag_emitted = True
    try:
        app_obj = item.funcargs.get("tauri_input_app") or item.funcargs.get("app")
        if app_obj is None:
            return
        rep.sections.append(
            ("macOS input_sim diagnostics",
             _collect_macos_input_sim_diagnostics(app_obj))
        )
    except Exception as exc:  # noqa: BLE001
        rep.sections.append(
            ("macOS input_sim diagnostics",
             f"<diagnostics collection raised: {exc!r}>")
        )


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def app_name() -> str:
    """The name of the app under test, from XA11Y_TEST_APP (default: tauri)."""
    return os.environ.get("XA11Y_TEST_APP", "tauri")


@pytest.fixture(scope="session")
def app(app_name: str) -> xa11y.App:
    """Launch (or connect to) the test app and return an xa11y App handle."""
    pid_env = os.environ.get("XA11Y_TEST_APP_PID")
    if pid_env:
        pid = int(pid_env)
        try:
            app_handle = xa11y.App.by_pid(pid, timeout=10.0)
        except (xa11y.SelectorNotMatchedError, xa11y.PlatformError):
            name = os.environ.get("XA11Y_TEST_APP_NAME")
            if not name:
                raise
            app_handle = xa11y.App.by_name(name, timeout=10.0)
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
def app_config(app_name: str) -> dict:
    """App-specific widget names and selectors for the current test app."""
    cfg = APP_CONFIGS.get(app_name)
    if cfg is None:
        pytest.fail(
            f"No APP_CONFIG entry for XA11Y_TEST_APP={app_name!r}. "
            f"Known apps: {', '.join(APP_CONFIGS)}"
        )
    return cfg


@pytest.fixture(scope="module")
def tauri_input_app(app_name, app):
    """Navigate the Tauri app to the input-events page.

    Module-scoped so the event log starts empty and focus state doesn't bleed
    in from widget tests. Skips automatically on non-Tauri apps.

    Navigates back to the home page on teardown so that subsequent suites
    (js, cli) and other test modules can rely on the OK / Submit buttons
    being present.
    """
    if app_name != "tauri":
        pytest.skip("tauri_input_app fixture is only available for the Tauri app")

    app.locator('button[name="Open input events page"]').press()
    try:
        app.locator('button[name="Hit target"]').wait_attached(timeout=5.0)
    except xa11y.TimeoutError:
        pytest.fail("input-events page did not load within 5s")

    try:
        yield app
    finally:
        try:
            app.locator('button[name="Back to widgets"]').press()
            app.locator('button[name="OK"]').wait_attached(timeout=5.0)
        except Exception:
            # Best-effort restoration — never fail the run on teardown.
            pass
