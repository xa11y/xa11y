"""Integration tests for xa11y Python bindings.

These tests run against the xa11y-test-app (AccessKit + winit) and verify
that the Python API works end-to-end through the real AT-SPI2 accessibility
stack on Linux.

Run with: ./run_python_integ_tests.sh
All tests are marked with @pytest.mark.integ and skipped by default.
"""

import pytest
import xa11y

# All tests in this file require the live test app + AT-SPI2 infrastructure.
pytestmark = pytest.mark.integ

APP_NAME = "xa11y-test-app"


@pytest.fixture(scope="module")
def provider():
    """Create a real platform provider (AT-SPI2 on Linux)."""
    return xa11y.connect()


@pytest.fixture
def app(provider):
    """Get the test app's accessibility tree."""
    return provider.app(APP_NAME)


# ── Discovery ────────────────────────────────────────────────────────────────


def test_list_apps_includes_test_app(provider):
    apps = provider.list_apps()
    names = [a.name for a in apps]
    assert any("xa11y" in n for n in names), f"Test app not found in: {names}"


def test_app_tree_has_nodes(app):
    assert len(app) > 10, f"Expected many nodes, got {len(app)}"
    assert app.root.role == "window"


# ── Tree queries ─────────────────────────────────────────────────────────────


def test_find_buttons_by_role(app):
    buttons = app.query("button")
    names = {b.name for b in buttons}
    assert "Submit" in names
    assert "Cancel" in names


def test_find_by_name(app):
    results = app.find_by_name("Submit")
    assert len(results) >= 1
    assert results[0].role == "button"


def test_query_text_field(app):
    fields = app.query("text_input")
    assert len(fields) >= 1
    name_field = next(f for f in fields if f.name == "Name")
    assert name_field.value == "John Doe"


def test_query_checkbox(app):
    cbs = app.query("check_box")
    assert len(cbs) >= 1
    cb = cbs[0]
    assert cb.name == "I agree to terms"
    # Initially unchecked
    assert cb.checked in ("off", False, None) or cb.checked != "on"


def test_query_slider(app):
    sliders = app.query("slider")
    assert len(sliders) >= 1
    slider = next(s for s in sliders if s.name == "Volume")
    assert slider.numeric_value == 50.0
    assert slider.min_value == 0.0
    assert slider.max_value == 100.0


def test_query_descendant_combinator(app):
    """toolbar > button finds buttons inside the toolbar."""
    results = app.query("toolbar > button")
    names = {b.name for b in results}
    assert "New" in names


# ── Locator API ──────────────────────────────────────────────────────────────


def test_locator_exists(app):
    assert app.locator("button[name='Submit']").exists()
    assert not app.locator("button[name='DoesNotExist']").exists()


def test_locator_name_and_role(app):
    loc = app.locator("button[name='Submit']")
    assert loc.name() == "Submit"
    assert loc.role() == "button"


def test_locator_is_enabled(app):
    assert app.locator("button[name='Submit']").is_enabled()
    assert not app.locator("button[name='Cancel']").is_enabled()


def test_locator_count(app):
    assert app.locator("button").count() >= 4  # Submit, Cancel, New, OpenTool, ...


def test_locator_nth(app):
    buttons = app.locator("toolbar > button")
    first = buttons.first()
    second = buttons.nth(1)
    assert first.name() != second.name()


# ── Actions: checkbox toggle ─────────────────────────────────────────────────


def test_toggle_checkbox_enables_cancel(provider):
    """Toggle the checkbox and verify Cancel button becomes enabled."""
    tree = provider.app(APP_NAME)

    # Checkbox starts unchecked, Cancel starts disabled
    cancel = tree.locator("button[name='Cancel']")
    assert not cancel.is_enabled(), "Cancel should start disabled"

    # Toggle the checkbox
    tree.press("check_box[name='I agree to terms']")

    # Re-read tree to see updated state
    tree = provider.app(APP_NAME)
    cancel = tree.locator("button[name='Cancel']")
    assert cancel.is_enabled(), "Cancel should be enabled after checking the checkbox"

    # Toggle back to restore initial state
    tree.press("check_box[name='I agree to terms']")


# ── Actions: text field ──────────────────────────────────────────────────────


def test_set_value_on_text_field(provider):
    """Set the Name field's value and verify it changed."""
    tree = provider.app(APP_NAME)
    name_field = tree.locator("text_input[name='Name']")
    original = name_field.value()

    tree.set_value("text_input[name='Name']", "Alice")

    tree = provider.app(APP_NAME)
    assert tree.locator("text_input[name='Name']").value() == "Alice"

    # Restore original value
    tree.set_value("text_input[name='Name']", original or "John Doe")


# ── Actions: slider ──────────────────────────────────────────────────────────


def test_increment_slider(provider):
    """Increment the slider and verify the value increased."""
    tree = provider.app(APP_NAME)
    slider = tree.locator("slider[name='Volume']")
    before = slider.get().numeric_value

    tree.increment("slider[name='Volume']")

    tree = provider.app(APP_NAME)
    after = tree.locator("slider[name='Volume']").get().numeric_value
    assert after == before + 1.0, f"Expected {before + 1.0}, got {after}"


def test_decrement_slider(provider):
    """Decrement the slider and verify the value decreased."""
    tree = provider.app(APP_NAME)
    slider = tree.locator("slider[name='Volume']")
    before = slider.get().numeric_value

    tree.decrement("slider[name='Volume']")

    tree = provider.app(APP_NAME)
    after = tree.locator("slider[name='Volume']").get().numeric_value
    assert after == before - 1.0, f"Expected {before - 1.0}, got {after}"


# ── Actions: list item selection ─────────────────────────────────────────────


def test_select_list_item(provider):
    """Click a list item and verify it becomes selected."""
    tree = provider.app(APP_NAME)

    # Initially no fruit is selected
    apple = tree.locator("list_item[name='Apple']")
    assert not apple.get().selected, "Apple should not be selected initially"

    # Click Apple
    apple.press()

    tree = provider.app(APP_NAME)
    apple = tree.locator("list_item[name='Apple']")
    assert apple.get().selected, "Apple should be selected after clicking"


# ── Actions: expand / collapse ───────────────────────────────────────────────


def test_expand_and_collapse(provider):
    """Expand the expander group and verify, then collapse it back."""
    tree = provider.app(APP_NAME)
    expander = tree.locator("group[name='Expander']")

    # Initially collapsed
    assert not expander.get().expanded, "Expander should start collapsed"

    # Expand
    tree.expand("group[name='Expander']")

    tree = provider.app(APP_NAME)
    expander = tree.locator("group[name='Expander']")
    assert expander.get().expanded, "Expander should be expanded"

    # Collapse
    tree.collapse("group[name='Expander']")

    tree = provider.app(APP_NAME)
    expander = tree.locator("group[name='Expander']")
    assert not expander.get().expanded, "Expander should be collapsed again"


# ── Actions: submit workflow ─────────────────────────────────────────────────


def test_submit_without_checkbox_shows_warning(provider):
    """Clicking Submit without checking the box shows a warning status."""
    # Make sure checkbox is unchecked first
    tree = provider.app(APP_NAME)
    cb = tree.query("check_box[name='I agree to terms']")
    if cb and cb[0].checked == "on":
        tree.press("check_box[name='I agree to terms']")
        tree = provider.app(APP_NAME)

    tree.press("button[name='Submit']")

    tree = provider.app(APP_NAME)
    status = tree.find_by_name("Please agree")
    assert len(status) >= 1, "Should show 'Please agree to terms' message"


def test_full_submit_workflow(provider):
    """Check the box, submit, and verify the status changes to 'Submitted'."""
    tree = provider.app(APP_NAME)

    # Ensure checkbox is checked
    cb = tree.query("check_box[name='I agree to terms']")
    if cb and cb[0].checked != "on":
        tree.press("check_box[name='I agree to terms']")
        tree = provider.app(APP_NAME)

    # Click Submit
    tree.press("button[name='Submit']")

    tree = provider.app(APP_NAME)
    status = tree.find_by_name("Submitted")
    assert len(status) >= 1, "Should show 'Submitted' status after checking box + submit"


# ── Navigation ───────────────────────────────────────────────────────────────


def test_parent_child_navigation(app):
    """Navigate the tree structure via parent/children properties."""
    root = app.root
    assert len(root.children) > 0

    # Find the toolbar and verify we can navigate back up
    toolbars = app.query("toolbar")
    assert len(toolbars) >= 1
    toolbar = toolbars[0]
    assert toolbar.parent is not None
    assert toolbar.parent.role == "window"

    # Toolbar should have button children
    btn_children = [c for c in toolbar.children if c.role == "button"]
    assert len(btn_children) >= 2


# ── Tree printing / repr ─────────────────────────────────────────────────────


def test_tree_str_contains_structure(app):
    """str(tree) should produce a readable dump of the tree."""
    dump = str(app)
    assert "window" in dump
    assert "button" in dump
    assert "Submit" in dump


def test_tree_repr(app):
    r = repr(app)
    assert "xa11y" in r.lower() or "Tree" in r
