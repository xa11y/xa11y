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


class _Unsupported(str):
    """A falsy reason string marking a widget the toolkit genuinely can't expose.

    ``APP_CONFIGS`` fields carry three kinds of value:

    - a real selector/name      → the widget exists; tests use it.
    - ``unsupported("reason")`` → the *toolkit* cannot expose this widget;
      tests skip, and the value documents why.
    - ``None`` (plus a comment) → the test app simply hasn't been instrumented
      for it yet (the toolkit could support it) — a parity gap to fix, not a
      permanent skip.

    Instances are falsy, so the existing ``if not value: pytest.skip(...)``
    checks in the suites treat them exactly like ``None`` — but a bare ``None``
    no longer ambiguously means both "can't" and "didn't bother".
    """

    __slots__ = ()

    def __bool__(self) -> bool:
        return False


def unsupported(reason: str) -> _Unsupported:
    """Mark an APP_CONFIGS field as genuinely unsupported by the toolkit."""
    return _Unsupported(reason)


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
        # Table — QTableWidget. Qt names each cell accessible from its item
        # text on every platform (UIA DataItem+TableItem, AT-SPI table cell,
        # AXCell), so cell names are asserted directly.
        "table_selector": 'table[name="Users Table"]',
        "table_min_cells": 4,
        "table_cell_names": ["Alice", "Admin", "Bob", "User"],
        "table_content_names": None,
        # The app selects cell (0, 0); the "Alice" cell must report
        # selected=true on every platform. On macOS Qt's bridge implements
        # no per-element AXSelected — selection is derived from the table's
        # AXSelectedChildren (see xa11y-macos container-selection probe).
        "table_selected_cell_name": "Alice",
        # Header cells are named on Windows (UIA HeaderItem), Linux (AT-SPI
        # column header), and in webviews — but Qt's Cocoa bridge exposes no
        # header objects at all (synthesized AXRows/AXColumns only, no
        # AXHeader attribute), so header names are absent from the macOS AX
        # tree entirely. Upstream Qt limitation, not an xa11y one — see
        # https://github.com/mrexodia/xa11y-table-repro captures.
        "table_header_names": (None if sys.platform == "darwin" else ["Name", "Role"]),
        # Window
        "window_name_contains": "xa11y-qt-test-app",
        # Dynamic buttons for event tests
        "submit_button_name": "Submit",
        "add_item_button_name": "Add Item",
        "remove_item_button_name": "Remove Item",
    },
    "gtk": {
        # Dialog — a Gtk.Window constructed with the DIALOG accessible role.
        "dialog_button_name": "Open Dialog",
        "dialog_name": "Sample Dialog",
        "ok_button_name": "OK",
        "cancel_button_name": "Cancel",
        # Not asserted: the app sets a tooltip, but GTK4's tooltip→AT-SPI
        # description mapping is not verified for this suite yet.
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
        # Table — Gtk.ColumnView (AT-SPI "tree table"); only one table in the
        # app so a role selector suffices. GTK 4.14 names each "table cell"
        # accessible from its child Gtk.Label (verified against a live
        # AT-SPI session), so cell names are asserted directly.
        "table_selector": "table",
        "table_min_cells": 4,
        "table_cell_names": ["Alice", "Admin", "Bob", "User"],
        "table_content_names": None,
        # The GTK table uses a NoSelection model — nothing is selected.
        "table_selected_cell_name": None,
        # ColumnView's header row is named from its column titles.
        "table_header_names": ["Name", "Role"],
        "window_name_contains": None,  # not asserted for GTK
        # Dynamic buttons for event tests (Dynamic group in app.py).
        "submit_button_name": "Submit",
        "add_item_button_name": "Add Item",
        "remove_item_button_name": "Remove Item",
    },
    "cocoa": {
        # Not instrumented yet: AppKit supports dialogs (NSPanel/NSAlert) but
        # the Cocoa test app has no dialog-opening button.
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
        # Table — multi-column cell-based NSTableView ("Users Table").
        # AppKit exposes AXCell elements without a title; the text is the
        # AXValue of the AXStaticText inside each cell, so content is
        # asserted via descendants, not cell names.
        "table_selector": 'table[name="Users Table"]',
        "table_min_cells": 4,
        "table_cell_names": None,
        "table_content_names": ["Alice", "Admin", "Bob", "User"],
        # Not instrumented yet: NSTableView selects rows, not cells; a
        # row-selection assertion needs named rows to target.
        "table_selected_cell_name": None,
        # AppKit exposes the header as sort-button children under the
        # table's header group, named from the column titles.
        "table_header_names": ["Name", "Role"],
        "window_name_contains": None,  # not asserted for Cocoa
        "submit_button_name": "Submit",
        "add_item_button_name": "Add Item",
        "remove_item_button_name": "Remove Item",
    },
    "tauri": {
        # Not instrumented yet: the webview could expose an ARIA dialog, but
        # the Tauri test page has no dialog-opening button.
        "dialog_button_name": None,
        "dialog_name": None,
        "ok_button_name": "OK",
        "cancel_button_name": "Cancel",
        # Not asserted: the OK button sets `title=`, but the webview bridges'
        # title→description mapping is not verified for this suite yet.
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
        # Spin button — <input type="number" role="spinbutton"> ("Quantity")
        # in test-apps/tauri/frontend/index.html.
        "spinbutton_selector": 'spin_button[name="Quantity"]',
        "progress_bar_selector": 'progress_bar[name="Progress"]',
        "textfield_selector": 'text_field[name="Search"]',
        "textfield_initial_value": "hello world",
        "textarea_selector": 'text_area[name="Notes"]',
        # Table — HTML <table> with a <caption> (WebKit's data-table
        # heuristic needs a caption/headers to expose the table at all, and
        # <th> is out — see the page comment). Only one table in the app,
        # found by role: WebKitGTK doesn't surface aria-label as the
        # table's AT-SPI name (macOS WebKit does). The cross-platform role
        # contract (table + table_cell) is asserted; cell text is NOT
        # name-addressable in either WebKit port — WebKitGTK exposes it
        # via the AT-SPI Text interface, macOS WebKit via text markers —
        # so content assertions for webviews live in the Electron config,
        # where Chromium names the text leaves.
        "table_selector": "table",
        "table_min_cells": 4,
        "table_cell_names": None,
        "table_content_names": None,
        # Plain HTML tables have no selection.
        "table_selected_cell_name": None,
        # No header assertions: the Tauri page carries no <th> cells at all —
        # under WebKitGTK with a window manager present, <th> triggers a
        # continuous accessibility-tree invalidation churn that detaches the
        # whole page from AT-SPI (see the comment in
        # test-apps/tauri/frontend/index.html). Webview header coverage
        # lives in the Electron config instead.
        "table_header_names": None,
        "window_name_contains": None,  # not asserted for Tauri
        "submit_button_name": "Submit",
        "add_item_button_name": "Add Item",
        "remove_item_button_name": "Remove Item",
    },
    "electron": {
        # Not instrumented yet: Chromium could expose an ARIA dialog, but the
        # Electron test page has no dialog-opening button.
        "dialog_button_name": None,
        "dialog_name": None,
        "ok_button_name": "OK",
        "cancel_button_name": "Cancel",
        "ok_button_description": None,  # not asserted for Electron
        "has_checkbox": True,
        "checkbox_unchecked_name": "Agree to terms",
        # Not instrumented yet: the Electron test app has only one checkbox.
        "checkbox_checked_name": None,
        # Not instrumented yet: the Electron test app has no radio buttons.
        "has_radio": False,
        "radio_role": None,
        "radio_a_name": None,
        "radio_b_name": None,
        # Range controls — markup mirrors the Tauri test app.
        "slider_selector": 'slider[name="Volume"]',
        "slider_initial_value": 50.0,
        "slider_min": 0.0,
        "slider_max": 100.0,
        "spinbutton_selector": 'spin_button[name="Quantity"]',
        "progress_bar_selector": 'progress_bar[name="Progress"]',
        "textfield_selector": 'text_field[name="Search"]',
        "textfield_initial_value": "hello world",
        "textarea_selector": 'text_area[name="Notes"]',
        # Table — markup mirrors the Tauri test app; same descendant-based
        # content assertion (Chromium's cell naming varies by platform).
        "table_selector": 'table[name="Users Table"]',
        "table_min_cells": 4,
        "table_cell_names": None,
        "table_content_names": ["Alice", "Admin", "Bob", "User"],
        # Plain HTML tables have no selection.
        "table_selected_cell_name": None,
        # <th> header cells are named from their text content.
        "table_header_names": ["Name", "Role"],
        "window_name_contains": None,  # not asserted for Electron
        # Not instrumented yet: no Dynamic (Submit / Add Item / Remove Item)
        # group in the Electron test app, so event tests skip.
        "submit_button_name": None,
        "add_item_button_name": None,
        "remove_item_button_name": None,
    },
    "accesskit": {
        # The AccessKit test app (test-apps/accesskit/src/main.rs) is the
        # canonical AccessKit-on-AT-SPI target on Linux. Its widget schema
        # differs from the shared toolkit fixtures: buttons are Submit/Cancel
        # (no OK), there is a single checkbox, and there is no native dialog.
        "dialog_button_name": unsupported(
            "the AccessKit/winit test app has no native dialog primitive"
        ),
        "dialog_name": unsupported(
            "the AccessKit/winit test app has no native dialog primitive"
        ),
        # Buttons — the app uses "Submit"/"Cancel" rather than "OK"/"Cancel".
        # The shared button tests treat ``ok_button_name`` as "the primary
        # activation button", so Submit fills that role here.
        "ok_button_name": "Submit",
        "cancel_button_name": "Cancel",
        # Submit advertises no description in the AccessKit tree.
        "ok_button_description": None,
        # In the AccessKit app, pressing Submit does NOT enable Cancel — the
        # checkbox toggle is what flips ``cancel_enabled`` (see handle_action
        # in test-apps/accesskit/src/main.rs). Tell the shared button test not
        # to assert the OK→Cancel-enable coupling that other toolkits provide.
        "ok_press_enables_cancel": False,
        # Checkboxes — a single checkbox labelled "I agree to terms",
        # initially unchecked. There is no pre-checked checkbox.
        "has_checkbox": True,
        "checkbox_unchecked_name": "I agree to terms",
        "checkbox_checked_name": None,
        # Radio buttons — Role::RadioButton, "Option A"/"Option B".
        "has_radio": True,
        "radio_role": "radio_button",
        "radio_a_name": "Option A",
        "radio_b_name": "Option B",
        # Slider — "Volume", numeric range 0..100.
        "slider_selector": 'slider[name="Volume"]',
        "slider_initial_value": 50.0,
        "slider_min": 0.0,
        "slider_max": 100.0,
        # Spin button — "Quantity", numeric range 0..100.
        "spinbutton_selector": 'spin_button[name="Quantity"]',
        # Progress bar — labelled "75%" (ProgressIndicator value 0.75).
        "progress_bar_selector": 'progress_bar[name="75%"]',
        # Text field — "Name". The app sets the value to "John Doe", but
        # accesskit_unix does not surface a Role::TextInput's value through the
        # AT-SPI Text/EditableText interface, so xa11y reads it back as None
        # and set_value() raises TextValueNotSupported. This is the same
        # adapter limitation the Rust integ test `action_set_value_text`
        # tolerates. Leave the initial value unchecked and mark the field
        # non-settable so the action tests skip rather than fail.
        "textfield_selector": 'text_field[name="Name"]',
        "textfield_initial_value": None,
        "textfield_settable": False,
        # The AccessKit app has no multiline text area.
        "textarea_selector": None,
        # Table — Role::Table "Users" with Role::Row / Role::Cell children.
        # AccessKit sets each cell's name from its label on every adapter
        # (on Windows via the structural DataItem disambiguation in
        # xa11y-windows — AccessKit exposes cells as pattern-less DataItems).
        "table_selector": 'table[name="Users"]',
        "table_min_cells": 6,
        "table_cell_names": [
            "Alice",
            "alice@test.com",
            "Admin",
            "Bob",
            "bob@test.com",
            "User",
        ],
        "table_content_names": None,
        # Not instrumented yet: the AccessKit app sets no selection on its
        # table cells.
        "table_selected_cell_name": None,
        # Not instrumented yet: the AccessKit app's table has no header row.
        "table_header_names": None,
        # Window name comes from the winit window title but AT-SPI reports the
        # binary name; leave unchecked.
        "window_name_contains": None,
        "submit_button_name": "Submit",
        "add_item_button_name": "Add Item",
        "remove_item_button_name": "Remove Item",
    },
    "egui": {
        # egui has no native dialog primitive; tests that depend on opening a
        # platform dialog skip.
        "dialog_button_name": unsupported("egui has no native dialog primitive"),
        "dialog_name": unsupported("egui has no native dialog primitive"),
        # Buttons — egui sets the AccessKit name from the visible label.
        "ok_button_name": "OK",
        "cancel_button_name": "Cancel",
        # `Response::on_hover_text` is a tooltip in egui; it does not push
        # through to AccessKit's description, so the description check is
        # skipped.
        "ok_button_description": unsupported(
            "egui tooltips (on_hover_text) do not push through to the "
            "AccessKit description"
        ),
        # Checkboxes
        "has_checkbox": True,
        "checkbox_unchecked_name": "Agree to terms",
        "checkbox_checked_name": "Subscribe",
        # Radio buttons
        "has_radio": True,
        "radio_role": "radio_button",
        "radio_a_name": "Option A",
        "radio_b_name": "Option B",
        # Slider — egui's `Slider::text("Volume")` becomes the AccessKit name.
        "slider_selector": 'slider[name="Volume"]',
        "slider_initial_value": 50.0,
        "slider_min": 0.0,
        "slider_max": 100.0,
        # Spin button — the egui app suppresses the slider's auxiliary
        # DragValue (see `.show_value(false)` in test-apps/egui/src/main.rs),
        # so the only remaining `spin_button` in the tree is the Quantity
        # field. Use a role-only selector — macOS AccessKit doesn't expose
        # AXMaxValue for SpinButton, so attribute-based matching on
        # `max_value` would only work on Linux/Windows.
        "spinbutton_selector": "spin_button",
        # Progress bar — `ProgressBar::text("75%")` becomes the AX name.
        "progress_bar_selector": "progress_bar",
        # Text field — egui's `TextEdit::singleline` does not set a name, so
        # match by role (only one in the app) and verify the initial value.
        "textfield_selector": "text_field",
        "textfield_initial_value": "hello world",
        # Text area — egui uses Role::MultilineTextInput which AccessKit's
        # macOS bridge maps to AXTextArea (xa11y `text_area`) but UIA on
        # Windows collapses to UIA_EditControlTypeId (xa11y `text_field`,
        # no distinct multiline role exists in UIA). Skip on Windows.
        "textarea_selector": None if sys.platform == "win32" else "text_area",
        # Table — egui has no table widget with table accessibility
        # semantics (egui::Grid and egui_extras' TableBuilder are layout
        # only; they emit no AccessKit Table/Row/Cell roles).
        "table_selector": unsupported(
            "egui has no table widget that exposes AccessKit table semantics"
        ),
        "table_min_cells": 0,
        "table_cell_names": None,
        "table_content_names": None,
        "table_selected_cell_name": None,
        "table_header_names": None,
        # Window name comes from `ViewportBuilder::with_title` but the
        # AT-SPI/UIA/AX layer reports the binary name; leave unchecked.
        "window_name_contains": None,
        "submit_button_name": "Submit",
        "add_item_button_name": "Add Item",
        "remove_item_button_name": "Remove Item",
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
        require_frontmost=True,
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
        require_frontmost=True,
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
        require_frontmost=True,
    )


def _launch_accesskit() -> xa11y.App:
    # Built as part of the Cargo workspace (`cargo build -p xa11y-test-app`);
    # the binary lands in the workspace target dir.
    binary = str(PROJECT_ROOT / "target" / "debug" / "xa11y-test-app")
    if not Path(binary).exists():
        result = subprocess.run(
            ["cargo", "build", "-p", "xa11y-test-app"],
            cwd=PROJECT_ROOT,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.fail(
                f"Failed to build AccessKit test app:\n{result.stdout}\n{result.stderr}"
            )
    yield from launch_test_app(
        command=[binary, "--headless"],
        app_names=["xa11y-test-app", "xa11y Test App"],
    )


def _launch_egui() -> xa11y.App:
    binary = str(
        PROJECT_ROOT / "test-apps" / "egui" / "target" / "debug" / "xa11y-egui-test-app"
    )
    if not Path(binary).exists():
        result = subprocess.run(
            [
                "cargo",
                "build",
                "--manifest-path",
                str(PROJECT_ROOT / "test-apps" / "egui" / "Cargo.toml"),
            ],
            cwd=PROJECT_ROOT,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            pytest.fail(
                f"Failed to build egui test app:\n{result.stdout}\n{result.stderr}"
            )
    yield from launch_test_app(
        command=[binary],
        app_names=["xa11y-egui-test-app"],
        content_ready_selector='button[name="OK"]',
        require_frontmost=True,
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
        name = os.environ.get("XA11Y_TEST_APP_NAME")
        # One waited lookup that matches the PID we were handed *or* the
        # harness-discovered name — `App.find` polls internally, so there's
        # no need for a pid-then-name fallback chain. On some toolkits the app
        # exposes a name to AT-SPI before its pid lookup resolves (or vice
        # versa); matching either signal absorbs both races.
        app_handle = xa11y.App.find(
            lambda a: a.pid == pid or (name is not None and a.name == name),
            timeout=10.0,
        )
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
