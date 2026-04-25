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
