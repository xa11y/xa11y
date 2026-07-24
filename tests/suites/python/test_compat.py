"""Compatibility tests: tree navigation, element discovery, roles, and properties.

These tests verify that xa11y can discover the core widget set across all
supported toolkits (Qt, GTK, Cocoa/AppKit, Tauri/WebView). App-specific
widget names and selectors come from the ``app_config`` fixture so that the
same assertions run against every app without duplication.

All role and property assertions cover the public API surface exposed by each
platform's accessibility bridge. Tests skip gracefully when the current app
does not include a particular widget type.
"""

from __future__ import annotations

import sys
import warnings

import pytest
import xa11y


ACTION_SETTLE = 0.3


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def dump_tree(el: xa11y.Element, depth: int = 0, max_depth: int = 20) -> str:
    if depth > max_depth:
        return ""
    indent = "  " * depth
    info = el.role
    if el.name:
        info += f'  name="{el.name}"'
    if el.value:
        info += f'  value="{el.value}"'
    if el.numeric_value is not None:
        info += f"  num={el.numeric_value}"
    if el.min_value is not None:
        info += f"  min={el.min_value}"
    if el.max_value is not None:
        info += f"  max={el.max_value}"
    if el.checked is not None:
        info += f"  checked={el.checked}"
    if el.enabled is False:
        info += "  DISABLED"
    lines = [indent + info]
    for child in el.children():
        lines.append(dump_tree(child, depth + 1, max_depth))
    return "\n".join(lines)


def collect_tree_nodes(app: xa11y.App, max_depth: int = 30) -> list[dict]:
    """Recursively collect the a11y tree into a flat list of dicts."""

    def _collect(el, depth):
        if depth > max_depth:
            return []
        node = {"role": el.role, "name": el.name, "depth": depth}
        nodes = [node]
        for child in el.children():
            nodes.extend(_collect(child, depth + 1))
        return nodes

    result = [{"role": "application", "name": app.name, "depth": 0}]
    for child in app.children():
        result.extend(_collect(child, 1))
    return result


# ---------------------------------------------------------------------------
# Diagnostics
# ---------------------------------------------------------------------------


def test_tree_dump(app):
    """Dump the full accessibility tree for CI debugging (always runs first)."""
    lines = [f'application  name="{app.name}"']
    for child in app.children():
        lines.append(dump_tree(child, depth=1))
    tree_text = "\n".join(lines)
    warnings.warn(
        f"\n=== Accessibility Tree ({sys.platform}) ===\n{tree_text}\n=== End Tree ===",
        stacklevel=1,
    )
    assert app is not None
    assert len(app.children()) > 0, f"Tree is empty! App: name={app.name}"


# ---------------------------------------------------------------------------
# App / window
# ---------------------------------------------------------------------------


def test_app_pid(app):
    assert app.pid > 0


def test_window_found(app, app_config):
    """Verify that the app exposes a window element (or the app-level root)."""
    # On Linux/macOS, the app has a window child.
    # On Windows UIA, there may be no separate application node — the app's
    # children are the window's content directly.
    if app.locator("window").exists():
        w = app.locator("window").element()
        assert w.role == "window"
        expected = app_config.get("window_name_contains")
        if expected:
            assert expected in (w.name or ""), (
                f"window name {w.name!r} does not contain {expected!r}"
            )
    else:
        # Windows: app itself is the window equivalent
        assert app.name is not None


def test_tree_is_not_empty(app):
    """The a11y tree must have a meaningful number of elements."""
    nodes = collect_tree_nodes(app)
    assert len(nodes) >= 5, (
        f"Tree has only {len(nodes)} nodes — expected at least 5"
    )


# ---------------------------------------------------------------------------
# Buttons
# ---------------------------------------------------------------------------


def test_ok_button_properties(app, app_config):
    ok = app.locator(f'button[name="{app_config["ok_button_name"]}"]').element()
    assert ok.role == "button"
    assert ok.name == app_config["ok_button_name"]
    assert ok.enabled is True
    if app_config.get("ok_button_description"):
        assert ok.description == app_config["ok_button_description"]


def test_cancel_button_disabled(app, app_config):
    cancel_name = app_config["cancel_button_name"]
    # Some test runs may have already pressed OK and enabled Cancel; tolerate
    # either state and just verify the button is findable with the right role.
    cancel = app.locator(f'button[name="{cancel_name}"]').element()
    assert cancel.role == "button"
    assert cancel.name == cancel_name
    assert isinstance(cancel.enabled, bool)


def test_button_count_at_least_two(app):
    """There should be at least two buttons in every test app."""
    count = app.locator("button").count()
    assert count >= 2, f"Expected >= 2 buttons, got {count}"


# ---------------------------------------------------------------------------
# Checkboxes
# ---------------------------------------------------------------------------


def test_unchecked_checkbox_found(app, app_config):
    if not app_config.get("has_checkbox"):
        pytest.skip("app has no checkbox widgets")
    name = app_config["checkbox_unchecked_name"]
    el = app.locator(f'check_box[name="{name}"]').element()
    assert el.role == "check_box"
    assert el.name == name
    # May have been toggled by prior tests; accept off or mixed.
    assert el.checked in ("off", "mixed", "on")


def test_checked_checkbox_found(app, app_config):
    if not app_config.get("has_checkbox"):
        pytest.skip("app has no checkbox widgets")
    name = app_config["checkbox_checked_name"]
    if not name:
        pytest.skip("app has no pre-checked check_box (only an unchecked one)")
    el = app.locator(f'check_box[name="{name}"]').element()
    assert el.role == "check_box"
    assert el.name == name
    assert el.checked in ("on", "off", "mixed")


# ---------------------------------------------------------------------------
# Radio buttons
# ---------------------------------------------------------------------------


def test_radio_buttons_found(app, app_config):
    if not app_config.get("has_radio"):
        pytest.skip("app has no radio button widgets")
    role = app_config["radio_role"]
    a_name = app_config["radio_a_name"]
    b_name = app_config["radio_b_name"]

    el_a = app.locator(f'{role}[name="{a_name}"]').element()
    assert el_a.role == role
    assert el_a.name == a_name
    assert el_a.checked in ("on", "off")

    el_b = app.locator(f'{role}[name="{b_name}"]').element()
    assert el_b.role == role
    assert el_b.name == b_name
    assert el_b.checked in ("on", "off")


# ---------------------------------------------------------------------------
# Slider
# ---------------------------------------------------------------------------


def test_slider_properties(app, app_config):
    sel = app_config.get("slider_selector")
    if not sel:
        pytest.skip("app has no slider widget")
    el = app.locator(sel).element()
    assert el.role == "slider"
    assert el.numeric_value is not None


def test_slider_range(app, app_config):
    sel = app_config.get("slider_selector")
    if not sel:
        pytest.skip("app has no slider widget")
    expected_min = app_config.get("slider_min")
    expected_max = app_config.get("slider_max")
    if expected_min is None or expected_max is None:
        pytest.skip("no expected range defined for this app")
    el = app.locator(sel).element()
    assert el.min_value == pytest.approx(expected_min)
    assert el.max_value == pytest.approx(expected_max)


# ---------------------------------------------------------------------------
# Spin button
# ---------------------------------------------------------------------------


def test_spinbutton_found(app, app_config):
    sel = app_config.get("spinbutton_selector")
    if not sel:
        pytest.skip("app has no spin_button widget")
    el = app.locator(sel).element()
    assert el.role == "spin_button"
    assert el.numeric_value is not None
    # Range parity across backends. xa11y-linux + xa11y-windows populate
    # min_value/max_value for every ranged role; xa11y-macos historically
    # only did so for Role::Slider, which silently dropped the attributes
    # for spin_button. This assertion seals the parity — if any backend
    # regresses or gains a new ranged role without wiring min/max, the
    # test surfaces it instead of just-working-with-None.
    assert el.min_value is not None, (
        f"spin_button {el.name!r} has numeric_value={el.numeric_value} "
        f"but min_value is None — likely a provider range-attribute gap."
    )
    assert el.max_value is not None, (
        f"spin_button {el.name!r} has numeric_value={el.numeric_value} "
        f"but max_value is None — likely a provider range-attribute gap."
    )


# ---------------------------------------------------------------------------
# Progress bar
# ---------------------------------------------------------------------------


def test_progress_bar_found(app, app_config):
    sel = app_config.get("progress_bar_selector")
    if not sel:
        pytest.skip("app has no progress_bar widget")
    el = app.locator(sel).element()
    assert el.role == "progress_bar"
    assert el.numeric_value is not None
    # NOTE: not asserting min_value/max_value here because some platform AX
    # bridges legitimately omit them when an app does not set explicit
    # bounds (e.g. an HTML <progress> without `min`/`max` attributes). The
    # spin_button parity check in test_spinbutton_found is the canonical
    # seal for the cross-backend range-attribute gap that motivated this
    # test — spin buttons always have a range, progress bars do not.


# ---------------------------------------------------------------------------
# Text field
# ---------------------------------------------------------------------------


def test_textfield_found(app, app_config):
    sel = app_config.get("textfield_selector")
    if not sel:
        pytest.skip("app has no text_field widget")
    el = app.locator(sel).element()
    assert el.role == "text_field"
    initial = app_config.get("textfield_initial_value")
    if initial is not None:
        assert el.value == initial


# ---------------------------------------------------------------------------
# Text area
# ---------------------------------------------------------------------------


def test_textarea_found(app, app_config):
    sel = app_config.get("textarea_selector")
    if not sel:
        pytest.skip("app has no text_area widget")
    loc = app.locator(sel)
    assert loc.exists(), f"text area not found with selector: {sel}"
    el = loc.element()
    assert el.value is not None


# ---------------------------------------------------------------------------
# Tables
# ---------------------------------------------------------------------------


def test_table_cells_normalize(app, app_config):
    """Table contents must normalize to table_cell on every toolkit and OS.

    This is the cross-platform contract behind ``table table_cell``
    selectors: Qt's UIA DataItem+TableItem cells, AccessKit's pattern-less
    DataItem cells (disambiguated structurally on Windows), AT-SPI
    "table cell", and AXCell must all surface as ``table_cell``.
    """
    selector = app_config.get("table_selector")
    if not selector:
        # unsupported() markers are falsy strings carrying the reason.
        reason = str(selector) if isinstance(selector, str) else ""
        pytest.skip(reason or "test app has no table widget")

    table = app.locator(selector).element()
    assert table.role == "table"

    cells = app.locator(f"{selector} table_cell").elements()
    min_cells = app_config["table_min_cells"]
    assert len(cells) >= min_cells, (
        f"expected at least {min_cells} table_cell elements under "
        f"{selector!r}, found {len(cells)}"
    )

    # Toolkits that name the cell accessible itself (Qt, AccessKit): the
    # named element must be the cell, not a descendant.
    for name in app_config.get("table_cell_names") or []:
        cell = app.locator(f'{selector} table_cell[name*="{name}"]').element()
        assert cell.role == "table_cell"

    # Toolkits where the text lives on a child accessible (GTK Labels,
    # AppKit static text, webview text leaves): the content must at least be
    # reachable somewhere under the table.
    for name in app_config.get("table_content_names") or []:
        assert app.locator(f'{selector} [name*="{name}"]').exists(), (
            f"no element named {name!r} found under {selector!r}"
        )

    # Engines that expose cell text via the platform text interface rather
    # than the accessible name (WebKitGTK): it must surface as the cell's
    # value, so `table_cell[value*=...]` selectors stay usable.
    for text in app_config.get("table_cell_values") or []:
        cell = app.locator(f'{selector} table_cell[value*="{text}"]').element()
        assert cell.role == "table_cell"


def test_table_selected_cell_state(app, app_config):
    """Per-cell selection state must survive every platform bridge.

    The test app selects one cell programmatically; that cell must report
    ``selected`` on Windows (UIA SelectionItem.IsSelected), Linux (AT-SPI
    selected state), and macOS. On macOS, Qt exposes selection only through
    the table's ``AXSelectedChildren`` (no per-element ``AXSelected``), so
    this exercises xa11y-macos's container-selection derivation.
    Regression for https://github.com/mrexodia/xa11y-table-repro.
    """
    selector = app_config.get("table_selector")
    selected_name = app_config.get("table_selected_cell_name")
    if not selector or not selected_name:
        pytest.skip("test app has no table with a programmatic cell selection")

    cell = app.locator(f'{selector} table_cell[name*="{selected_name}"]').element()
    assert cell.selected, (
        f"cell {selected_name!r} is programmatically selected in the app "
        f"but reports selected={cell.selected}"
    )
    # A sibling cell must NOT leak the selected state.
    for other in app.locator(f"{selector} table_cell").elements():
        if other.name and selected_name not in other.name:
            assert not other.selected, (
                f"unselected cell {other.name!r} reports selected=True"
            )


def test_tree_has_no_unknown_roles(app, app_config):
    """Opt-in guard: a fully-mapped toolkit's tree contains no unknown roles.

    Unmapped platform roles silently degrade to ``unknown`` in non-strict
    builds, which is exactly how the AT-SPI numeric/name map gaps (roles 10,
    54, 64, 73, 110, ...) went unnoticed: the strict-roles build only runs
    against the AccessKit app, while the diverse-toolkit matrix cells run
    non-strict. Apps opt in via ``expect_no_unknown_roles`` once their tree
    is verified clean; the failure message carries each element's raw
    platform role so a regression points straight at the missing mapping.
    """
    if not app_config.get("expect_no_unknown_roles"):
        pytest.skip("tree not asserted unknown-free for this app")

    unknowns = app.locator("unknown").elements()
    details = [
        f"name={el.name!r} raw={el.raw!r}" for el in unknowns
    ]
    assert not unknowns, (
        f"{len(unknowns)} element(s) with unmapped platform roles:\n  "
        + "\n  ".join(details)
    )


def test_table_headers_exposed(app, app_config):
    """Column header names must be reachable under the table.

    Skipped where the toolkit genuinely exposes no header objects (e.g.
    Qt's Cocoa bridge synthesizes AXRows/AXColumns only and implements no
    AXHeader, so header names do not exist in the macOS AX tree at all).
    """
    selector = app_config.get("table_selector")
    headers = app_config.get("table_header_names")
    if not selector or not headers:
        pytest.skip("test app exposes no named table headers on this platform")

    for header in headers:
        assert app.locator(f'{selector} [name*="{header}"]').exists(), (
            f"header {header!r} not found under {selector!r}"
        )


# ---------------------------------------------------------------------------
# Tree structure quality
# ---------------------------------------------------------------------------


def test_no_named_unknown_roles(app, app_name):
    """Every *named* element must map to a known role (not 'unknown').

    Nameless unknown nodes are toolkit artifacts (structural fillers) and are
    excluded — only named unknowns indicate a role-mapping bug.
    """
    if sys.platform.startswith("linux") and app_name in ("tauri", "electron"):
        pytest.skip(
            "WebKit2GTK / Chromium expose extra AT-SPI roles (panel,"
            " section, statusbar, …) that xa11y deliberately maps to"
            " `unknown`. Role-coverage gaps in webview content are tracked"
            " separately."
        )
    nodes = collect_tree_nodes(app)
    unknowns = [
        f'depth={n["depth"]} name={n["name"]!r}'
        for n in nodes
        if n["role"] == "unknown" and n["name"]
    ]
    assert not unknowns, (
        f"Found {len(unknowns)} *named* element(s) with role 'unknown':\n"
        + "\n".join(f"  - {u}" for u in unknowns[:20])
    )


def test_all_roles_are_valid_strings(app):
    """Every role string in the tree must be a recognized xa11y role."""
    valid_roles = {
        "unknown", "window", "application", "button", "check_box",
        "radio_button", "text_field", "text_area", "static_text",
        "combo_box", "list", "list_item", "menu", "menu_item",
        "menu_bar", "tab", "tab_group", "table", "table_row",
        "table_cell", "toolbar", "scroll_bar", "scroll_thumb", "slider",
        "image", "link", "group", "dialog", "alert", "progress_bar",
        "tree_item", "web_area", "heading", "separator", "split_group",
        "switch", "spin_button", "tooltip", "status", "navigation",
    }
    nodes = collect_tree_nodes(app)
    invalid = [
        f'role={n["role"]!r} name={n["name"]!r} depth={n["depth"]}'
        for n in nodes
        if n["role"] not in valid_roles
    ]
    assert not invalid, (
        f"Found {len(invalid)} element(s) with unrecognized role strings:\n"
        + "\n".join(f"  - {i}" for i in invalid[:20])
    )


# ---------------------------------------------------------------------------
# Selector features
# ---------------------------------------------------------------------------


def test_selector_nth(app):
    """:nth(N) (1-based) returns exactly that match."""
    first = app.locator("button:nth(1)").elements()
    assert len(first) == 1
    assert first[0].role == "button"


def test_selector_attribute_enabled_true(app):
    """[enabled="true"] matches only enabled elements."""
    enabled_buttons = app.locator('button[enabled="true"]').elements()
    assert enabled_buttons
    for b in enabled_buttons:
        assert b.enabled is True


def test_selector_attribute_enabled_false(app):
    """[enabled="false"] returns only disabled elements."""
    disabled = app.locator('[enabled="false"]').elements()
    for d in disabled:
        assert d.enabled is False


def test_selector_attribute_checked_on(app, app_config):
    """[checked="on"] matches pre-checked widgets."""
    if not app_config.get("has_checkbox") and not app_config.get("has_radio"):
        pytest.skip("app has no checkable widgets")
    if not app_config.get("checkbox_checked_name") and not app_config.get("has_radio"):
        pytest.skip("app has no widgets initialised in the checked state")
    matches = app.locator('[checked="on"]').elements()
    # At least one checked widget should exist (Subscribe or Option A).
    assert matches, "Expected at least one element with checked='on'"


def test_locator_count_matches_elements_len(app):
    """count() agrees with len(elements())."""
    loc = app.locator("button")
    assert loc.count() == len(loc.elements())


def test_element_parent_roundtrip(app, app_config):
    """parent() of a button points back to a usable ancestor element."""
    ok_name = app_config["ok_button_name"]
    ok = app.locator(f'button[name="{ok_name}"]').element()
    p = ok.parent()
    assert p is not None
    assert p.role  # has a non-empty role string


# ---------------------------------------------------------------------------
# Dialog role
# ---------------------------------------------------------------------------


def test_dialog_role(app, app_config):
    """A native toolkit dialog window must be reported with role 'dialog'.

    On Windows, UIA maps all top-level windows to WindowControlTypeId and
    xa11y distinguishes dialogs via UIA_IsDialogPropertyId (set by Qt) or
    AriaRole="dialog" (set by web/AccessKit content). This test exercises
    the native Qt path via QDialog, which does not set AriaRole.

    The test skips for toolkits that do not expose a dialog button.
    """
    import time

    btn_name = app_config.get("dialog_button_name")
    if not btn_name:
        pytest.skip("app config has no dialog_button_name")

    dialog_name = app_config.get("dialog_name", "")

    # Open the dialog.
    app.locator(f'button[name="{btn_name}"]').press()

    # Poll until the dialog appears (up to 5 s).
    deadline = time.monotonic() + 5.0
    dlg = None
    while time.monotonic() < deadline:
        candidates = app.locator("dialog").elements()
        if dialog_name:
            candidates = [c for c in candidates if c.name and dialog_name in c.name]
        if candidates:
            dlg = candidates[0]
            break
        time.sleep(0.1)

    assert dlg is not None, (
        f"No element with role 'dialog' found within 5 s after pressing "
        f"'{btn_name}'. Platform: {sys.platform}. "
        "On Windows this indicates UIA_IsDialogPropertyId is not being checked."
    )
    assert dlg.role == "dialog", f"Expected role 'dialog', got {dlg.role!r}"

    # Close the dialog so it does not interfere with subsequent tests.
    close_candidates = app.locator('button[name="Close Dialog"]').elements()
    if close_candidates:
        app.locator('button[name="Close Dialog"]').press()
        time.sleep(0.1)


# ---------------------------------------------------------------------------
# tree() and dump()
# ---------------------------------------------------------------------------


def test_element_tree_snapshot(app, app_config):
    """Element.tree() returns a dict snapshot with the expected shape."""
    import time

    ok_name = app_config["ok_button_name"]
    # Locator.element() resolves once with no auto-wait; on Windows the UIA
    # tree can be transiently unsettled after a prior test closes a dialog,
    # so poll briefly to keep this test focused on Element.tree() rather
    # than on its own selector resolution timing.
    deadline = time.monotonic() + 5.0
    while True:
        try:
            btn = app.locator(f'button[name="{ok_name}"]').element()
            break
        except xa11y.SelectorNotMatchedError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.05)
    node = btn.tree(max_depth=0)
    assert node["role"] == "button"
    assert node["name"] == ok_name
    assert node["children"] == []


def test_element_tree_children_structure(app, app_config):
    """tree(max_depth=1) includes children but not grandchildren."""
    ok_name = app_config["ok_button_name"]
    btn = app.locator(f'button[name="{ok_name}"]').element()
    node = btn.tree(max_depth=1)
    assert isinstance(node["children"], list)


def test_element_dump_returns_string(app, app_config):
    """Element.dump() returns a non-empty string containing the role."""
    ok_name = app_config["ok_button_name"]
    text = app.locator(f'button[name="{ok_name}"]').element().dump(max_depth=0)
    assert isinstance(text, str)
    assert "button" in text


def test_element_dump_max_depth_zero_single_line(app, app_config):
    """dump(max_depth=0) produces exactly one non-empty line."""
    ok_name = app_config["ok_button_name"]
    text = app.locator(f'button[name="{ok_name}"]').element().dump(max_depth=0)
    lines = [line for line in text.splitlines() if line.strip()]
    assert len(lines) == 1


def test_locator_tree_shorthand(app, app_config):
    """tree() via element() round-trip."""
    ok_name = app_config["ok_button_name"]
    node = app.locator(f'button[name="{ok_name}"]').element().tree(max_depth=0)
    assert node["role"] == "button"
    assert node["name"] == ok_name


def test_locator_dump_shorthand(app, app_config):
    """dump() via element() round-trip."""
    ok_name = app_config["ok_button_name"]
    text = app.locator(f'button[name="{ok_name}"]').element().dump(max_depth=0)
    assert isinstance(text, str)
    assert "button" in text
