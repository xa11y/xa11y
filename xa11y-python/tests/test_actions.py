"""Tests for action dispatch: Tree.press/focus/etc, Tree.perform, target resolution."""

import pytest
import xa11y

# ── Target resolution: node vs selector string ──────────────────────────────


def test_press_with_node(tree):
    back = next(b for b in tree.query("button") if b.name == "Back")
    tree.press(back)  # should not raise


def test_press_with_selector(tree):
    tree.press('button[name="Back"]')  # should not raise


def test_press_wrong_type(tree):
    with pytest.raises(TypeError, match="Node or a selector string"):
        tree.press(42)


def test_press_selector_no_match(tree):
    with pytest.raises(xa11y.SelectorNotMatchedError):
        tree.press("menu_item")


# ── All convenience action methods ──────────────────────────────────────────


def test_focus(tree):
    tree.focus('button[name="Back"]')


def test_blur(tree):
    tree.blur('button[name="Back"]')


def test_toggle(tree):
    tree.toggle('check_box[name="Agree"]')


def test_expand(tree):
    tree.expand('list[name="Items"]')


def test_collapse(tree):
    tree.collapse('list[name="Items"]')


def test_select(tree):
    tree.select('list_item[name="Item 2"]')


def test_increment(tree):
    tree.increment('slider[name="Volume"]')


def test_decrement(tree):
    tree.decrement('slider[name="Volume"]')


def test_show_menu(tree):
    tree.show_menu('button[name="Back"]')


def test_scroll_into_view(tree):
    tree.scroll_into_view('button[name="Back"]')


# ── Value actions ────────────────────────────────────────────────────────────


def test_set_value(tree):
    tree.set_value('text_field[name="Search"]', "new text")


def test_set_numeric_value(tree):
    tree.set_numeric_value('slider[name="Volume"]', 50.0)


def test_type_text(tree):
    tree.type_text('text_field[name="Search"]', "typed text")


# ── Scroll with direction ───────────────────────────────────────────────────


def test_scroll_down(tree):
    tree.scroll('list[name="Items"]', "down")


def test_scroll_up(tree):
    tree.scroll('list[name="Items"]', "up")


def test_scroll_left(tree):
    tree.scroll('list[name="Items"]', "left")


def test_scroll_right(tree):
    tree.scroll('list[name="Items"]', "right")


def test_scroll_custom_amount(tree):
    tree.scroll('list[name="Items"]', "down", 5.0)


def test_scroll_default_amount(tree):
    tree.scroll('list[name="Items"]', "down")  # default amount=1.0


def test_scroll_invalid_direction(tree):
    with pytest.raises(ValueError, match="Unknown scroll direction"):
        tree.scroll('list[name="Items"]', "diagonal")


# ── Select text ──────────────────────────────────────────────────────────────


def test_select_text(tree):
    tree.select_text('text_field[name="Search"]', 0, 5)


# ── Generic perform ─────────────────────────────────────────────────────────


def test_perform_press(tree):
    tree.perform('button[name="Back"]', "press")


def test_perform_set_value_with_text(tree):
    tree.perform('text_field[name="Search"]', "set_value", value="new")


def test_perform_set_value_with_numeric(tree):
    tree.perform('slider[name="Volume"]', "set_value", numeric_value=42.0)


def test_perform_type_text(tree):
    tree.perform('text_field[name="Search"]', "type_text", value="hi")


def test_perform_scroll(tree):
    tree.perform('list[name="Items"]', "scroll", direction="down", amount=3.0)


def test_perform_scroll_missing_direction(tree):
    with pytest.raises(ValueError, match="scroll requires direction"):
        tree.perform('list[name="Items"]', "scroll")


def test_perform_set_text_selection(tree):
    tree.perform('text_field[name="Search"]', "set_text_selection", start=0, end=3)


def test_perform_set_text_selection_missing_start(tree):
    with pytest.raises(ValueError, match="requires start"):
        tree.perform('text_field[name="Search"]', "set_text_selection", end=3)


def test_perform_set_text_selection_missing_end(tree):
    with pytest.raises(ValueError, match="requires end"):
        tree.perform('text_field[name="Search"]', "set_text_selection", start=0)


def test_perform_unknown_action(tree):
    with pytest.raises(ValueError, match="Unknown action"):
        tree.perform('button[name="Back"]', "fly_away")
