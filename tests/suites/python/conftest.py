"""Shared fixtures for feature-based xa11y integration tests.

Reads XA11Y_TEST_APP to select the target app (default: tauri). The launch
recipes themselves live in ``tests/launchers.py`` as pytest-xa11y
``AppLauncher`` objects, shared with the CLI suite; this file supplies the
part that is specific to the Python suite.

App-specific widget names/values that differ across toolkits live in
APP_CONFIGS and are exposed via the ``app_config`` session-scoped fixture.
"""

from __future__ import annotations

import os
import sys

import pytest
import xa11y

from tests.launchers import launcher_for

# ---------------------------------------------------------------------------
# App-specific configuration
# ---------------------------------------------------------------------------


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
        # Not yet verified unknown-free on all three OSes.
        "expect_no_unknown_roles": False,
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
        # Verified clean against a live AT-SPI session.
        "expect_no_unknown_roles": True,
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
        # Not yet verified unknown-free.
        "expect_no_unknown_roles": False,
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
        # Text area — WebView2 goes through UIA, which has no distinct
        # multiline text role: <textarea> collapses to UIA_EditControlTypeId
        # (xa11y `text_field`). Skip on Windows, same as the egui entry below.
        "textarea_selector": (
            None if sys.platform == "win32" else 'text_area[name="Notes"]'
        ),
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
        # WebKitGTK exposes cell text through the AT-SPI Text interface, which
        # xa11y surfaces as the cell's value. This is a WebKitGTK property, not
        # a webview one: macOS WebKit has no equivalent (text markers only),
        # and the Windows cell renders through WebView2, which is Chromium
        # under UIA. Asserted on Linux only.
        "table_cell_values": (
            ["Alice", "Admin", "Bob", "User"] if sys.platform == "linux" else None
        ),
        # Plain HTML tables have no selection.
        "table_selected_cell_name": None,
        # No header assertions: the Tauri page carries no <th> cells at all —
        # under WebKitGTK with a window manager present, <th> triggers a
        # continuous accessibility-tree invalidation churn that detaches the
        # whole page from AT-SPI (see the comment in
        # test-apps/tauri/frontend/index.html). Webview header coverage
        # lives in the Electron config instead.
        "table_header_names": None,
        # WebKitGTK exposes two hidden spin-button arrow internals with the
        # (deliberately unmapped) AT-SPI "arrow" role, so the tree is not
        # unknown-free.
        "expect_no_unknown_roles": False,
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
        # Not yet verified unknown-free.
        "expect_no_unknown_roles": False,
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
        # Verified clean against a live AT-SPI session.
        "expect_no_unknown_roles": True,
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
        # Not yet verified unknown-free.
        "expect_no_unknown_roles": False,
        # Window name comes from `ViewportBuilder::with_title` but the
        # AT-SPI/UIA/AX layer reports the binary name; leave unchecked.
        "window_name_contains": None,
        "submit_button_name": "Submit",
        "add_item_button_name": "Add Item",
        "remove_item_button_name": "Remove Item",
    },
    "winforms": {
        # The first Microsoft UI framework in the matrix (issue #324). Every
        # other Windows cell reaches UIA through a third-party bridge; this one
        # exercises the WinForms provider, whose control types come from each
        # AccessibleObject's Role via AccessibleRoleControlTypeMap.
        #
        # Windows-only: tests/harness/launch.py rejects it elsewhere.
        #
        # WinForms never sets UIA_IsDialogPropertyId, so a WinForms dialog
        # reaches xa11y as a plain `window` — there is no signal to key the
        # `dialog` role off, whatever the app does.
        "dialog_button_name": unsupported(
            "WinForms Forms do not set UIA_IsDialogPropertyId, so a dialog is "
            "indistinguishable from any other top-level window"
        ),
        "dialog_name": unsupported(
            "WinForms Forms do not set UIA_IsDialogPropertyId"
        ),
        "ok_button_name": "OK",
        "cancel_button_name": "Cancel",
        # Not asserted: the app sets AccessibleDescription on OK, but WinForms
        # routes it to MSAA accDescription rather than UIA HelpText /
        # FullDescription — the two properties xa11y reads as `description`.
        "ok_button_description": None,
        "has_checkbox": True,
        "checkbox_unchecked_name": "Agree to terms",
        "checkbox_checked_name": "Subscribe",
        "has_radio": True,
        "radio_role": "radio_button",
        "radio_a_name": "Option A",
        "radio_b_name": "Option B",
        # TrackBar reports ControlType.Slider but its accessible object
        # advertises only Value + LegacyIAccessible (see
        # TrackBar.TrackBarAccessibleObject.IsPatternSupported upstream), never
        # RangeValue — so xa11y reads no numeric_value/min_value/max_value and
        # the whole slider group (compat, actions, events, errors) skips.
        "slider_selector": unsupported(
            "WinForms TrackBar exposes no UIA RangeValue pattern, so it has no "
            "numeric value or range to assert"
        ),
        "slider_initial_value": None,
        "slider_min": None,
        "slider_max": None,
        # Same story as the slider: UpDownBase.UpDownBaseAccessibleObject
        # reports ControlType.Spinner but inherits the base pattern set, which
        # is Invoke-only — no RangeValue, so numeric_value is None and the
        # min/max parity assertion in test_spinbutton_found cannot hold.
        "spinbutton_selector": unsupported(
            "WinForms NumericUpDown exposes no UIA RangeValue pattern"
        ),
        # ProgressBar does implement RangeValue (Minimum/Maximum/RangeValue on
        # ProgressBar.ProgressBarAccessibleObject), so this one is asserted.
        "progress_bar_selector": 'progress_bar[name="Progress"]',
        "textfield_selector": 'text_field[name="Search"]',
        "textfield_initial_value": "hello world",
        # UIA has no multiline-edit control type: a WinForms multiline TextBox
        # is ControlType.Edit like any other, which xa11y maps to `text_field`.
        # (Same UIA limitation the egui config notes on Windows.)
        "textarea_selector": unsupported(
            "UIA has no distinct multiline edit control type, so a multiline "
            "WinForms TextBox is indistinguishable from a single-line one"
        ),
        # Table — DataGridView. The grid is ControlType.DataGrid and its cells
        # are ControlType.DataItem + the TableItem pattern, which is the
        # is_table_item branch of map_uia_role in xa11y-windows/src/uia.rs.
        "table_selector": 'table[name="Users Table"]',
        "table_min_cells": 4,
        # Not asserted: DataGridViewCellAccessibleObject.Name is a synthesized
        # "<column header> Row <n>" string (SR.DataGridView_AccDataGridViewCellName,
        # plus sort status for sortable columns) — a framework-generated,
        # localizable label, not the cell text.
        "table_cell_names": None,
        # No child accessibles under a grid cell, so nothing to reach by name.
        "table_content_names": None,
        # The cell text is the ValuePattern value (the cell's FormattedValue),
        # which is how xa11y surfaces it — same shape as WebKitGTK's cells.
        "table_cell_values": ["Alice", "Admin", "Bob", "User"],
        # The app selects cell (0, 0). WinForms grid cells advertise no
        # SelectionItem pattern (Legacy/Invoke/Value/TableItem/GridItem only)
        # and publish selection solely as the MSAA STATE_SYSTEM_SELECTED bit,
        # which is what the LegacyIAccessible.State read in xa11y-windows's
        # `parse_states` picks up. Pinned by value, not name, because the
        # synthesized cell names are not addressable (see table_cell_names).
        "table_selected_cell_name": None,
        "table_selected_cell_value": "Alice",
        # Column headers are ControlType.Header named from the column's
        # HeaderText; test_table_headers_exposed matches on name, not role.
        "table_header_names": ["Name", "Role"],
        # The named unknowns this app first surfaced — the grid's "Top Row" /
        # "Row 1" / "Row 2", which publish no UIA control type — now resolve
        # through the MSAA role (see map_msaa_role in xa11y-windows). The
        # nameless remainder of the tree is not verified clean yet, so the
        # opt-in whole-tree guard stays off.
        "expect_no_unknown_roles": False,
        "window_name_contains": "xa11y-winforms-test-app",
        "submit_button_name": "Submit",
        "add_item_button_name": "Add Item",
        "remove_item_button_name": "Remove Item",
    },
    "wpf": {
        # The second Microsoft UI framework in the matrix (step 2 of issue
        # #324), and the one that produces ControlType.Custom + TableItem
        # cells — the branch of map_uia_role added in #323, which no other app
        # in the matrix exercises. WinForms covers the DataItem cell shape.
        #
        # Windows-only: tests/harness/launch.py rejects it elsewhere.
        #
        # Unlike WinForms, WPF has AutomationProperties.IsDialog, which is
        # exactly UIA_IsDialogPropertyId — so the native-dialog role test runs.
        "dialog_button_name": "Open Dialog",
        "dialog_name": "Sample Dialog",
        "ok_button_name": "OK",
        "cancel_button_name": "Cancel",
        # WPF routes AutomationProperties.HelpText to UIA HelpText, one of the
        # two properties xa11y reads as `description` (WinForms routes its
        # AccessibleDescription to MSAA accDescription instead, which is why
        # the WinForms config leaves this unasserted).
        "ok_button_description": "Confirm the dialog",
        "has_checkbox": True,
        "checkbox_unchecked_name": "Agree to terms",
        "checkbox_checked_name": "Subscribe",
        "has_radio": True,
        "radio_role": "radio_button",
        "radio_a_name": "Option A",
        "radio_b_name": "Option B",
        # SliderAutomationPeer implements IRangeValueProvider, so unlike the
        # WinForms TrackBar the value and the range are both readable and the
        # whole slider group (compat, actions, events, errors) runs.
        "slider_selector": 'slider[name="Volume"]',
        "slider_initial_value": 50.0,
        "slider_min": 0.0,
        "slider_max": 100.0,
        # WPF ships no spin-button control — there is no NumericUpDown or any
        # other in-box control whose peer reports ControlType.Spinner, so the
        # app has no widget to point at.
        "spinbutton_selector": unsupported(
            "WPF ships no spin-button control; no in-box control reports "
            "ControlType.Spinner"
        ),
        # ProgressBarAutomationPeer also implements IRangeValueProvider.
        "progress_bar_selector": 'progress_bar[name="Progress"]',
        "textfield_selector": 'text_field[name="Search"]',
        "textfield_initial_value": "hello world",
        # Same UIA limitation the WinForms and egui configs note: there is no
        # multiline-edit control type, so a WPF TextBox with AcceptsReturn is
        # ControlType.Edit like any other.
        "textarea_selector": unsupported(
            "UIA has no distinct multiline edit control type, so a multiline "
            "WPF TextBox is indistinguishable from a single-line one"
        ),
        # Table — DataGrid. The grid is ControlType.DataGrid, its rows are
        # DataItem (DataGridItemAutomationPeer) and its cells are
        # ControlType.Custom + the TableItem pattern
        # (DataGridCellItemAutomationPeer), which is the Custom branch of
        # map_uia_role in xa11y-windows/src/uia.rs. This is the only app in the
        # matrix that produces that shape.
        "table_selector": 'table[name="Users Table"]',
        "table_min_cells": 4,
        # Not asserted: DataGridCellItemAutomationPeer.GetNameCore synthesizes
        # a localizable "<item> <column display index>" string from the row
        # object and the column position, not the cell text.
        "table_cell_names": None,
        # No child accessibles under a grid cell, so nothing to reach by name.
        "table_content_names": None,
        # The cell text is the ValuePattern value
        # (DataGridCellItemAutomationPeer implements IValueProvider over the
        # column's clipboard content), which is how xa11y surfaces it.
        "table_cell_values": ["Alice", "Admin", "Bob", "User"],
        # The app selects cell (0, 0) with SelectionUnit=Cell. Unlike WinForms
        # grid cells, WPF's implement ISelectionItemProvider, so this is read
        # from SelectionItem.IsSelected rather than the MSAA state bit. Pinned
        # by value, not name, because the synthesized cell names are not
        # addressable (see table_cell_names).
        "table_selected_cell_name": None,
        "table_selected_cell_value": "Alice",
        # Column headers are ControlType.HeaderItem
        # (DataGridColumnHeaderAutomationPeer) named from the column's Header;
        # test_table_headers_exposed matches on name, not role.
        "table_header_names": ["Name", "Role"],
        # Not yet verified unknown-free across the whole tree.
        "expect_no_unknown_roles": False,
        "window_name_contains": "xa11y-wpf-test-app",
        "submit_button_name": "Submit",
        "add_item_button_name": "Add Item",
        "remove_item_button_name": "Remove Item",
    },
}


# ---------------------------------------------------------------------------
# macOS input-simulation diagnostics
# ---------------------------------------------------------------------------
#
# pytest-xa11y already attaches the frontmost app, the visible process list,
# the app's identity and its accessibility tree to every failure. What it
# cannot know is this app's own event log, or whether a *fresh* click lands —
# and that re-probe is what distinguishes "the test click lost a race" from
# "this macOS session cannot deliver CGEvents to this WKWebView at all".
#
# It stays a local hook rather than a registered collector because it is
# specific to one module and it *mutates* the app: it posts a real click.
# Running that on every failure in the suite would corrupt the state of
# whatever failed next. Gated exactly as before — macOS, test_input_sim, once
# per session.

_INPUT_SIM_DIAGNOSTICS_EMITTED = False


def _tauri_input_sim_reprobe(app) -> str:
    """Post one fresh click and report whether it reached the webview."""
    import time

    lines = []
    try:
        hit = app.locator('button[name="Hit target"]').element().bounds
        if hit:
            lines.append(
                f"  hit_target bounds: x={hit.x} y={hit.y} w={hit.width} h={hit.height}"
            )
            lines.append(
                f"  hit_target center: ({hit.x + hit.width // 2}, {hit.y + hit.height // 2})"
            )
        else:
            lines.append("  hit_target bounds: <None>")
    except Exception as exc:  # noqa: BLE001 - diagnostics must not mask the failure
        lines.append(f"  hit_target: <error {exc!r}>")
        hit = None

    try:
        log_value = app.locator('text_area[name="Event log"]').element().value or ""
        lines.append(f"  event_log (len={len(log_value)}): {log_value!r}")
    except Exception as exc:  # noqa: BLE001
        lines.append(f"  event_log: <error {exc!r}>")

    if hit is None:
        lines.append("  reprobe: <no bounds>")
        return "\n".join(lines)

    try:
        sim = xa11y.input_sim()
        app.locator('button[name="Clear log"]').press()
        time.sleep(0.2)
        sim.click((hit.x + hit.width // 2, hit.y + hit.height // 2))
        time.sleep(0.5)
        after = app.locator('text_area[name="Event log"]').element().value or ""
        lines.append(f"  reprobe click -> log (len={len(after)}): {after!r}")
    except Exception as exc:  # noqa: BLE001
        lines.append(f"  reprobe: <error {exc!r}>")
    return "\n".join(lines)


@pytest.hookimpl(hookwrapper=True, tryfirst=True)
def pytest_runtest_makereport(item, call):
    outcome = yield
    rep = outcome.get_result()
    global _INPUT_SIM_DIAGNOSTICS_EMITTED
    if (
        rep.when != "call"
        or not rep.failed
        or sys.platform != "darwin"
        or "test_input_sim" not in item.nodeid
        or _INPUT_SIM_DIAGNOSTICS_EMITTED
    ):
        return
    _INPUT_SIM_DIAGNOSTICS_EMITTED = True
    app_obj = item.funcargs.get("tauri_input_app") or item.funcargs.get("app")
    if app_obj is None:
        return
    try:
        rep.sections.append(
            ("macOS input_sim reprobe", _tauri_input_sim_reprobe(app_obj))
        )
    except Exception as exc:  # noqa: BLE001
        rep.sections.append(("macOS input_sim reprobe", f"<reprobe raised: {exc!r}>"))


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def app_name() -> str:
    """The name of the app under test, from XA11Y_TEST_APP (default: tauri)."""
    return os.environ.get("XA11Y_TEST_APP", "tauri")


@pytest.fixture(scope="session")
def xa11y_launcher(app_name: str):
    """Tell pytest-xa11y how to launch (or attach to) the app under test."""
    return launcher_for(app_name)


@pytest.fixture(scope="session")
def app(xa11y_app: xa11y.App) -> xa11y.App:
    """The running test app.

    An alias for the plugin's ``xa11y_app``, kept because every test in this
    suite names it ``app``.
    """
    return xa11y_app


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
        except Exception:  # noqa: BLE001
            # Best-effort restoration — never fail the run on teardown.
            pass
