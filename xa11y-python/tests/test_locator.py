"""Tests for Locator class: chaining, queries, actions, wait operations."""

import pytest
import xa11y

# ── Construction ─────────────────────────────────────────────────────────────


def test_locator_from_tree(tree):
    loc = tree.locator("button")
    assert loc.selector == "button"


def test_locator_from_provider(provider):
    loc = provider.locator("TestApp", selector="button")
    assert loc.selector == "button"


def test_locator_from_provider_by_pid(provider):
    loc = provider.locator(pid=1234, selector="button")
    assert loc.selector == "button"


def test_locator_from_provider_no_target(provider):
    with pytest.raises(ValueError, match="Either name or pid"):
        provider.locator(selector="button")


def test_locator_repr(tree):
    loc = tree.locator('button[name="Back"]')
    assert "button" in repr(loc)
    assert "Back" in repr(loc)


# ── Chaining ─────────────────────────────────────────────────────────────────


def test_nth(tree):
    loc = tree.locator("button").nth(1)
    assert loc.name() == "Forward"


def test_first(tree):
    loc = tree.locator("button").first()
    assert loc.name() == "Back"


def test_child(tree):
    loc = tree.locator("toolbar").child("button")
    assert loc.selector == "toolbar > button"
    assert loc.count() == 2


def test_descendant(tree):
    loc = tree.locator("window").descendant("button")
    assert loc.selector == "window button"
    assert loc.count() == 2


# ── Queries ──────────────────────────────────────────────────────────────────


def test_role(tree):
    loc = tree.locator('button[name="Back"]')
    assert loc.role() == "button"


def test_name(tree):
    loc = tree.locator('button[name="Back"]')
    assert loc.name() == "Back"


def test_value(tree):
    loc = tree.locator("text_field")
    assert loc.value() == "hello"


def test_value_none(tree):
    loc = tree.locator('button[name="Back"]')
    assert loc.value() is None


def test_description(tree):
    loc = tree.locator('button[name="Back"]')
    assert loc.description() == "Go back"


def test_is_visible(tree):
    assert tree.locator('button[name="Back"]').is_visible() is True
    assert tree.locator("static_text").is_visible() is False


def test_is_enabled(tree):
    assert tree.locator('button[name="Back"]').is_enabled() is True
    assert tree.locator('button[name="Forward"]').is_enabled() is False


def test_is_focused(tree):
    assert tree.locator("window").is_focused() is True
    assert tree.locator('button[name="Back"]').is_focused() is False


def test_exists_true(tree):
    assert tree.locator("button").exists() is True


def test_exists_false(tree):
    assert tree.locator("menu_item").exists() is False


def test_count(tree):
    assert tree.locator("button").count() == 2
    assert tree.locator("list_item").count() == 2
    assert tree.locator("menu_item").count() == 0


def test_get_returns_node(tree):
    node = tree.locator('button[name="Back"]').get()
    assert isinstance(node, xa11y.Node)
    assert node.role == "button"
    assert node.name == "Back"


def test_not_matched_raises(tree):
    with pytest.raises(xa11y.SelectorNotMatchedError):
        tree.locator("menu_item").role()


# ── Actions ──────────────────────────────────────────────────────────────────


def test_locator_press(tree):
    tree.locator('button[name="Back"]').press()


def test_locator_focus(tree):
    tree.locator('button[name="Back"]').focus()


def test_locator_blur(tree):
    tree.locator('button[name="Back"]').blur()


def test_locator_toggle(tree):
    tree.locator("check_box").toggle()


def test_locator_expand(tree):
    tree.locator("list").expand()


def test_locator_collapse(tree):
    tree.locator("list").collapse()


def test_locator_select_item(tree):
    tree.locator('list_item[name="Item 1"]').select_item()


def test_locator_show_menu(tree):
    tree.locator('button[name="Back"]').show_menu()


def test_locator_scroll_into_view(tree):
    tree.locator('button[name="Back"]').scroll_into_view()


def test_locator_increment(tree):
    tree.locator("slider").increment()


def test_locator_decrement(tree):
    tree.locator("slider").decrement()


def test_locator_set_value(tree):
    tree.locator("text_field").set_value("new")


def test_locator_set_numeric_value(tree):
    tree.locator("slider").set_numeric_value(42.0)


def test_locator_type_text(tree):
    tree.locator("text_field").type_text("typed")


def test_locator_select_text(tree):
    tree.locator("text_field").select_text(0, 3)


def test_locator_scroll(tree):
    tree.locator("list").scroll("down")


def test_locator_scroll_with_amount(tree):
    tree.locator("list").scroll("up", 5.0)


def test_locator_scroll_invalid_direction(tree):
    with pytest.raises(ValueError, match="scroll direction"):
        tree.locator("list").scroll("sideways")


# ── Wait operations ──────────────────────────────────────────────────────────
# The mock always returns the same tree, so waits resolve immediately
# or timeout. We test the happy paths and the timeout path.


def test_wait_visible_immediate(tree):
    tree.locator('button[name="Back"]').wait_visible(timeout=0.5)


def test_wait_attached_immediate(tree):
    tree.locator('button[name="Back"]').wait_attached(timeout=0.5)


def test_wait_enabled_immediate(tree):
    tree.locator('button[name="Back"]').wait_enabled(timeout=0.5)


def test_wait_hidden_immediate(tree):
    # static_text "Status" is hidden
    tree.locator("static_text").wait_hidden(timeout=0.5)


def test_wait_detached_for_nonexistent(tree):
    # Element doesn't exist → detached immediately
    tree.locator("menu_item").wait_detached(timeout=0.5)


def test_wait_visible_timeout(tree):
    # static_text is hidden — waiting for visible should timeout
    with pytest.raises(xa11y.TimeoutError):
        tree.locator("static_text").wait_visible(timeout=0.3)


def test_wait_enabled_timeout(tree):
    # Forward button is disabled — waiting for enabled should timeout
    with pytest.raises(xa11y.TimeoutError):
        tree.locator('button[name="Forward"]').wait_enabled(timeout=0.3)


def test_wait_detached_timeout(tree):
    # Back button exists — waiting for detached should timeout
    with pytest.raises(xa11y.TimeoutError):
        tree.locator('button[name="Back"]').wait_detached(timeout=0.3)
