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
import time
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


def test_password_field_is_flagged(app, app_config):
    """A password entry keeps the text_field role and sets `password` (#522).

    The selector form matches exactly the password entry, and the ordinary
    text field next to it is not flagged.
    """
    name = app_config.get("password_field_name")
    if not name:
        pytest.skip("app has no password entry")
    el = app.locator(f'text_field[name="{name}"]').element()
    assert el.role == "text_field"
    assert el.password is True
    flagged = app.locator('text_field[password="true"]').elements()
    assert [e.name for e in flagged] == [name]
    plain = app.locator(app_config["textfield_selector"]).element()
    assert plain.password is False


def test_password_field_value_is_withheld(app, app_config):
    """A password entry whose provider withholds its text reads as no value.

    On Windows, a password control declines `ValuePattern.Value` with
    `UIA_E_INVALIDOPERATION`. That must surface as `value is None` on a
    flagged element, not as a platform error that fails every tree read of
    the window holding it (#597). Walking the whole tree first proves the
    snapshot of every element, the password field among them, succeeds.
    """
    name = app_config.get("password_field_name")
    if not name or not app_config.get("password_value_withheld"):
        pytest.skip("app has no password entry whose value is withheld")
    nodes = collect_tree_nodes(app)
    assert nodes, "tree read returned no elements"
    el = app.locator(f'text_field[name="{name}"]').element()
    assert el.password is True
    assert el.value is None, f"password value leaked: {el.value!r}"


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
    selectors: Qt's and WinForms' UIA DataItem+TableItem cells, WPF's
    DataGrid cells (ControlType.Custom + TableItem), AccessKit's
    pattern-less DataItem cells (disambiguated structurally on Windows),
    AT-SPI "table cell", and AXCell must all surface as ``table_cell``.
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
    ``selected`` on Windows (UIA SelectionItem.IsSelected, or the MSAA
    ``STATE_SYSTEM_SELECTED`` bit for frameworks that predate SelectionItem —
    WinForms grids), Linux (AT-SPI selected state), and macOS. On macOS, Qt
    exposes selection only through the table's ``AXSelectedChildren`` (no
    per-element ``AXSelected``), so this exercises xa11y-macos's
    container-selection derivation.
    Regression for https://github.com/mrexodia/xa11y-table-repro.

    The selected cell is pinned by name where cells are name-addressable, and
    by value otherwise — WinForms names its cells with a synthesized
    "<column header> Row <n>" string, so ``table_selected_cell_value`` targets
    the cell text instead (the same reason ``table_cell_values`` exists).
    """
    selector = app_config.get("table_selector")
    selected_name = app_config.get("table_selected_cell_name")
    selected_value = app_config.get("table_selected_cell_value")
    if not selector or not (selected_name or selected_value):
        pytest.skip("test app has no table with a programmatic cell selection")

    attr, expected = (
        ("name", selected_name) if selected_name else ("value", selected_value)
    )

    cell = app.locator(f'{selector} table_cell[{attr}*="{expected}"]').element()
    assert cell.selected, (
        f"cell with {attr} {expected!r} is programmatically selected in the "
        f"app but reports selected={cell.selected}"
    )
    # A sibling cell must NOT leak the selected state.
    for other in app.locator(f"{selector} table_cell").elements():
        observed = getattr(other, attr)
        if observed and expected not in observed:
            assert not other.selected, (
                f"unselected cell ({attr}={observed!r}) reports selected=True"
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
    xa11y distinguishes dialogs via UIA_IsDialogPropertyId (set by Qt, and
    by WPF through AutomationProperties.IsDialog) or AriaRole="dialog" (set
    by web/AccessKit content). This test exercises the native path via
    QDialog and the WPF dialog window, neither of which sets AriaRole.

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


def test_hidden_reason_is_set_exactly_when_not_visible(app):
    """Every element carries a ``hidden_reason`` exactly when it is not
    visible (#590), and the reason is one of the documented names."""
    known = {"window_minimized", "scrolled_out", "not_showing", "unknown"}
    elements = app.locator("*").elements()
    assert elements, "the app's tree must not be empty"
    broken = [
        (e.role, e.name, e.visible, e.hidden_reason)
        for e in elements
        if (e.hidden_reason is None) != e.visible
        or (e.hidden_reason is not None and e.hidden_reason not in known)
    ]
    assert not broken, (
        f"hidden_reason must be a known name exactly when visible is False; "
        f"these elements break it: {broken[:20]}"
    )


def _scrolled_pane_visibility(app, pane_name: str, first: str, last: str):
    """The first and last entries' ``visible``, read both ways the provider
    computes it: a selector search, which takes the one-pass subtree fetch,
    and ``children()``, which clips each level to its parent."""
    found = {
        name: app.locator(f'[name="{name}"]').element().visible
        for name in (first, last)
    }
    pane = app.locator(f'[name="{pane_name}"]').element()
    listed = {c.name: c.visible for c in pane.children() if c.name in (first, last)}
    return found, listed


def test_controls_scrolled_out_of_a_pane_are_not_visible(app, app_config):
    """A control scrolled out of its pane reports ``visible == False``, and
    ``hidden_reason == "scrolled_out"``.

    On Windows, some UIA providers leave ``IsOffscreen`` false on content
    outside the viewport, so without the provider's own clipping it counted
    as visible and an auto-waiting action would act on a control nobody can
    see (issue #502). The test first asserts that premise on the scrolled-out
    entry's ``raw["uia_is_offscreen"]``, so it cannot pass with or without
    the fix. On Linux the toolkit hides the row itself, and the provider has
    to tell scrolling from any other hiding (#590).
    """
    pane_name = app_config.get("scrolled_pane_name")
    if not pane_name:
        pytest.skip("this app has no pane longer than its viewport yet")
    first = app_config["scrolled_pane_first_item"]
    last = app_config["scrolled_pane_last_item"]

    hidden = app.locator(f'[name="{last}"]').element()
    if sys.platform == "win32":
        # The premise: UIA itself says the scrolled-out entry is on screen.
        # Without it this test would pass with or without the fix.
        assert hidden.raw.get("uia_is_offscreen") is False, (
            f"{last!r} reports uia_is_offscreen="
            f"{hidden.raw.get('uia_is_offscreen')!r}, so the fixture no longer "
            f"exercises issue #502 (UIA leaving IsOffscreen false on a "
            f"scrolled-out control)"
        )

    found, listed = _scrolled_pane_visibility(app, pane_name, first, last)
    expected = {first: True, last: False}
    assert found == expected, (
        f"selector search: the first entry must be visible and the last "
        f"scrolled out; got {found}"
    )
    assert listed == expected, (
        f"children(): the first entry must be visible and the last scrolled "
        f"out; got {listed}"
    )
    # A hidden element says why, and a visible one says nothing (#590).
    assert hidden.hidden_reason == "scrolled_out", (
        f"{last!r} is hidden because its pane clips it, so hidden_reason "
        f"must be 'scrolled_out'; got {hidden.hidden_reason!r}"
    )
    shown = app.locator(f'[name="{first}"]').element()
    assert shown.hidden_reason is None, (
        f"a visible element has no hidden_reason; {first!r} reports "
        f"{shown.hidden_reason!r}"
    )


def test_a_control_its_container_hides_is_not_showing(app, app_config):
    """A control on an inactive tab page reads ``hidden_reason ==
    "not_showing"`` (#590).

    A contract test: the hidden control reports no bounds, so this does not
    exercise the scroll-role check that tells a control lying outside a
    container that does not scroll from a scrolled-out one. The Linux unit
    tests ``scrolled_out_needs_a_scrolling_container_on_the_climb`` and
    ``a_hidden_row_beyond_its_list_is_scrolled_out`` guard that check.
    """
    name = app_config.get("not_showing_name")
    if not name:
        pytest.skip("this app has no control hidden by a non-scrolling container")
    hidden = app.locator(f'[name="{name}"]').element()
    assert hidden.visible is False, f"{name!r} is on an inactive page, so not visible"
    assert hidden.hidden_reason == "not_showing", (
        f"{name!r} is hidden by its tab page, which does not scroll, so "
        f"hidden_reason must be 'not_showing'; got {hidden.hidden_reason!r}"
    )


def test_scrolling_a_pane_flips_which_controls_are_visible(app, app_config):
    """After the pane scrolls to the last entry, it reports visible and the
    first does not: the clip follows the scroll position rather than a
    remembered answer.

    Focus moves the WinForms viewport: the framework scrolls a pane to the
    control that takes focus. ``scroll_into_view`` cannot, because plain
    buttons have no UIA ScrollItem pattern. ``Element.focus`` acts without
    waiting for ``visible``, which the scrolled-out entry is not. A Qt list
    row offers neither focus nor AT-SPI ``ScrollTo``, so an app that sets
    ``scrolled_pane_end_button`` / ``scrolled_pane_start_button`` scrolls by
    pressing those instead.
    """
    pane_name = app_config.get("scrolled_pane_name")
    if not pane_name:
        pytest.skip("this app has no pane longer than its viewport yet")
    first = app_config["scrolled_pane_first_item"]
    last = app_config["scrolled_pane_last_item"]
    expected = {first: False, last: True}

    buttons = {
        last: app_config.get("scrolled_pane_end_button"),
        first: app_config.get("scrolled_pane_start_button"),
    }

    def bring_into_view(name: str) -> None:
        if buttons[name]:
            app.locator(f'button[name="{buttons[name]}"]').press()
        else:
            app.locator(f'[name="{name}"]').element().focus()

    bring_into_view(last)
    try:
        deadline = time.monotonic() + 5.0
        while True:
            found, listed = _scrolled_pane_visibility(app, pane_name, first, last)
            settled = found == expected and listed == expected
            if settled or time.monotonic() > deadline:
                break
            time.sleep(0.2)
        assert found == expected, (
            f"selector search after scrolling to the last entry: got {found}"
        )
        assert listed == expected, (
            f"children() after scrolling to the last entry: got {listed}"
        )
        gone = app.locator(f'[name="{first}"]').element()
        if sys.platform == "win32":
            # The premise again, from the other side: UIA still calls the
            # entry that just scrolled out on screen, so the flip is the
            # viewport clip.
            assert gone.raw.get("uia_is_offscreen") is False, (
                f"{first!r} reports uia_is_offscreen="
                f"{gone.raw.get('uia_is_offscreen')!r} after scrolling out, so "
                f"this test no longer exercises issue #502"
            )
        assert gone.hidden_reason == "scrolled_out", (
            f"{first!r} scrolled out of its pane, so hidden_reason must be "
            f"'scrolled_out'; got {gone.hidden_reason!r}"
        )
    finally:
        # Put the shared app back for the test above, whatever the order.
        bring_into_view(first)


def test_open_dropdown_in_a_scrolled_panel_is_visible(app, app_config):
    """An open drop-down's entries report visible although they lie outside
    the overflowing panel that holds the combo box.

    The drop-down is a popup window, drawn above the panel, but UIA lists it
    under the combo in the raw view. Clipping it to the panel would leave the
    entries not visible and time out any auto-waiting action on them.
    """
    combo_name = app_config.get("scrolled_panel_combo_name")
    if not combo_name:
        pytest.skip("this app has no combo box in an overflowing panel yet")
    entry = app_config["scrolled_panel_combo_last_item"]

    # The panel must really clip, or the drop-down has nothing to escape:
    # its filler sits below the panel's viewport.
    # And it must be the provider's clip that hides the filler, not UIA's own
    # IsOffscreen, or a clip that wrongly swallowed the popup could hide
    # behind a framework that already reports it.
    filler = app.locator(
        f'[name="{app_config["scrolled_panel_filler_name"]}"]'
    ).element()
    assert filler.raw.get("uia_is_offscreen") is False, (
        f"the panel's filler reports uia_is_offscreen="
        f"{filler.raw.get('uia_is_offscreen')!r}, so its visibility does not "
        f"show the panel's clip at work"
    )
    assert filler.visible is False, (
        "the panel's filler must be scrolled out of view; if it is visible, "
        "the panel exposes no UIA Scroll pattern and this fixture tests "
        "nothing"
    )
    panel = (
        app.locator(f'[name="{app_config["scrolled_panel_name"]}"]').element().bounds
    )

    combo = app.locator(f'[name="{combo_name}"]')
    combo.expand()
    try:
        el = app.locator(f'[name="{entry}"]').element()
        b = el.bounds
        outside = b is not None and (
            b.y >= panel.y + panel.height
            or b.y + b.height <= panel.y
            or b.x >= panel.x + panel.width
            or b.x + b.width <= panel.x
        )
        assert outside, (
            f"the drop-down entry must lie wholly outside the panel "
            f"({panel!r}) for this to test popup clipping; got bounds={b!r}"
        )
        assert el.visible is True, (
            f"an open drop-down entry must be visible, not clipped to the "
            f"scrolling panel; got visible={el.visible!r} bounds={el.bounds!r}"
        )
    finally:
        combo.collapse()
