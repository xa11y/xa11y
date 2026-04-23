"""Tests for Locator class: chaining, queries, actions, wait operations."""

import pytest
import xa11y

# ── Construction ─────────────────────────────────────────────────────────────


def test_locator_selector(test_app):
    loc = test_app.descendant("button")
    assert "button" in loc.selector


def test_locator_repr(test_app):
    loc = test_app.descendant('button[name="Back"]')
    assert "button" in repr(loc)
    assert "Back" in repr(loc)


# ── Chaining ─────────────────────────────────────────────────────────────────


def test_nth(test_app):
    loc = test_app.descendant("button").nth(2)  # 1-based indexing
    assert loc.element().name == "Forward"


def test_first(test_app):
    loc = test_app.descendant("button").first()
    assert loc.element().name == "Back"


def test_child(test_app):
    loc = test_app.descendant("toolbar").child("button")
    assert "toolbar > button" in loc.selector
    assert loc.count() == 2


def test_descendant_chain(test_app):
    loc = test_app.descendant("window").descendant("button")
    assert "window button" in loc.selector
    assert loc.count() == 2


# ── Queries ──────────────────────────────────────────────────────────────────


def test_exists_true(test_app):
    assert test_app.descendant("button").exists() is True


def test_exists_false(test_app):
    assert test_app.descendant("menu_item").exists() is False


def test_count(test_app):
    assert test_app.descendant("button").count() == 2
    assert test_app.descendant("list_item").count() == 2
    assert test_app.descendant("menu_item").count() == 0


def test_element_returns_element(test_app):
    element = test_app.descendant('button[name="Back"]').element()
    assert isinstance(element, xa11y.Element)
    assert element.role == "button"
    assert element.name == "Back"


def test_elements_returns_list(test_app):
    elements = test_app.descendant("button").elements()
    assert isinstance(elements, list)
    assert len(elements) == 2
    assert all(isinstance(n, xa11y.Element) for n in elements)


def test_elements_empty_for_no_match(test_app):
    elements = test_app.descendant("menu_item").elements()
    assert elements == []


def test_not_matched_raises(test_app):
    with pytest.raises(xa11y.SelectorNotMatchedError):
        test_app.descendant("menu_item").element()


# ── Actions ──────────────────────────────────────────────────────────────────


def test_locator_press(test_app):
    test_app.descendant('button[name="Back"]').press()


def test_locator_focus(test_app):
    test_app.descendant('button[name="Back"]').focus()


def test_locator_blur(test_app):
    test_app.descendant('button[name="Back"]').blur()


def test_locator_press_checkbox(test_app):
    test_app.descendant("check_box").press()


def test_locator_expand(test_app):
    test_app.descendant("list").expand()


def test_locator_collapse(test_app):
    test_app.descendant("list").collapse()


def test_locator_select(test_app):
    test_app.descendant('list_item[name="Item 1"]').select()


def test_locator_show_menu(test_app):
    test_app.descendant('button[name="Back"]').show_menu()


def test_locator_scroll_into_view(test_app):
    test_app.descendant('button[name="Back"]').scroll_into_view()


def test_locator_increment(test_app):
    test_app.descendant("slider").increment()


def test_locator_decrement(test_app):
    test_app.descendant("slider").decrement()


def test_locator_set_value(test_app):
    test_app.descendant("text_field").set_value("new")


def test_locator_set_numeric_value(test_app):
    test_app.descendant("slider").set_numeric_value(42.0)


def test_locator_type_text(test_app):
    test_app.descendant("text_field").type_text("typed")


def test_locator_select_text(test_app):
    test_app.descendant("text_field").select_text(0, 3)


# ── Wait operations ──────────────────────────────────────────────────────────


def test_wait_visible_immediate(test_app):
    test_app.descendant('button[name="Back"]').wait_visible(timeout=0.5)


def test_wait_attached_immediate(test_app):
    test_app.descendant('button[name="Back"]').wait_attached(timeout=0.5)


def test_wait_enabled_immediate(test_app):
    test_app.descendant('button[name="Back"]').wait_enabled(timeout=0.5)


def test_wait_hidden_immediate(test_app):
    test_app.descendant("static_text").wait_hidden(timeout=0.5)


def test_wait_detached_for_nonexistent(test_app):
    test_app.descendant("menu_item").wait_detached(timeout=0.5)


def test_wait_visible_timeout(test_app):
    with pytest.raises(xa11y.TimeoutError):
        test_app.descendant("static_text").wait_visible(timeout=0.3)


def test_wait_enabled_timeout(test_app):
    with pytest.raises(xa11y.TimeoutError):
        test_app.descendant('button[name="Forward"]').wait_enabled(timeout=0.3)


def test_wait_detached_timeout(test_app):
    with pytest.raises(xa11y.TimeoutError):
        test_app.descendant('button[name="Back"]').wait_detached(timeout=0.3)
