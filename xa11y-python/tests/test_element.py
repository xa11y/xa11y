"""Tests for Element class: fields, state flags, bounds, actions, dunders."""


# ── Core fields ──────────────────────────────────────────────────────────────


def test_element_role_as_string(test_app):
    tree = test_app.elements()
    assert tree.role == "application"
    buttons = test_app.locator("button").elements()
    assert buttons[0].role == "button"


def test_element_name(test_app):
    tree = test_app.elements()
    assert tree.name == "TestApp"


def test_element_name_none(test_app):
    tree = test_app.elements()
    # All elements in the test tree have names, but check type
    assert isinstance(tree.name, str)


def test_element_value(test_app):
    search = test_app.locator("text_field").elements()[0]
    assert search.value == "hello"


def test_element_value_none(test_app):
    tree = test_app.elements()
    assert tree.value is None


def test_element_description(test_app):
    tree = test_app.elements()
    assert tree.description == "Test application"
    buttons = test_app.locator("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    assert back.description == "Go back"


def test_element_description_none(test_app):
    buttons = test_app.locator("button").elements()
    fwd = next(b for b in buttons if b.name == "Forward")
    assert fwd.description is None


def test_element_stable_id(test_app):
    tree = test_app.elements()
    assert tree.stable_id == "app-root"
    buttons = test_app.locator("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    assert back.stable_id == "btn-back"


def test_element_stable_id_none(test_app):
    buttons = test_app.locator("button").elements()
    fwd = next(b for b in buttons if b.name == "Forward")
    assert fwd.stable_id is None


# ── Numeric values ───────────────────────────────────────────────────────────


def test_numeric_value(test_app):
    slider = test_app.locator("slider").elements()[0]
    assert slider.numeric_value == 75.0


def test_min_value(test_app):
    slider = test_app.locator("slider").elements()[0]
    assert slider.min_value == 0.0


def test_max_value(test_app):
    slider = test_app.locator("slider").elements()[0]
    assert slider.max_value == 100.0


def test_numeric_value_none(test_app):
    tree = test_app.elements()
    assert tree.numeric_value is None
    assert tree.min_value is None
    assert tree.max_value is None


# ── Flattened state flags ────────────────────────────────────────────────────


def test_enabled_default_true(test_app):
    tree = test_app.elements()
    assert tree.enabled is True


def test_enabled_false(test_app):
    buttons = test_app.locator("button").elements()
    fwd = next(b for b in buttons if b.name == "Forward")
    assert fwd.enabled is False


def test_visible_default_true(test_app):
    tree = test_app.elements()
    assert tree.visible is True


def test_visible_false(test_app):
    status = test_app.locator("static_text").elements()[0]
    assert status.visible is False


def test_focused(test_app):
    tree = test_app.elements()
    window = tree.children[0]
    assert window.focused is True
    assert tree.focused is False


def test_checked_on(test_app):
    cb = test_app.locator("check_box").elements()[0]
    assert cb.checked == "on"


def test_checked_none(test_app):
    tree = test_app.elements()
    assert tree.checked is None
    buttons = test_app.locator("button").elements()
    assert all(b.checked is None for b in buttons)


def test_selected(test_app):
    items = test_app.locator("list_item").elements()
    item1 = next(i for i in items if i.name == "Item 1")
    item2 = next(i for i in items if i.name == "Item 2")
    assert item1.selected is True
    assert item2.selected is False


def test_expanded(test_app):
    lst = test_app.locator("list").elements()[0]
    assert lst.expanded is True


def test_expanded_none(test_app):
    tree = test_app.elements()
    # Non-expandable elements have expanded=None
    assert tree.expanded is None


def test_editable(test_app):
    tree = test_app.elements()
    search = test_app.locator("text_field").elements()[0]
    assert search.editable is True
    assert tree.editable is False


def test_focusable(test_app):
    tree = test_app.elements()
    buttons = test_app.locator("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    assert back.focusable is True
    assert tree.focusable is False


def test_modal(test_app):
    tree = test_app.elements()
    assert tree.modal is False


def test_required(test_app):
    tree = test_app.elements()
    assert tree.required is False


def test_busy(test_app):
    tree = test_app.elements()
    assert tree.busy is False


# ── Actions list ─────────────────────────────────────────────────────────────


def test_actions_button(test_app):
    buttons = test_app.locator("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    assert back.actions == ["press", "focus"]


def test_actions_text_field(test_app):
    search = test_app.locator("text_field").elements()[0]
    assert set(search.actions) == {"focus", "set_value", "type_text"}


def test_actions_slider(test_app):
    slider = test_app.locator("slider").elements()[0]
    assert set(slider.actions) == {"increment", "decrement", "set_value", "focus"}


def test_actions_checkbox(test_app):
    cb = test_app.locator("check_box").elements()[0]
    assert set(cb.actions) == {"toggle", "focus"}


def test_actions_empty(test_app):
    tree = test_app.elements()
    assert tree.actions == []


# ── Bounds ───────────────────────────────────────────────────────────────────


def test_bounds_present(test_app):
    tree = test_app.elements()
    b = tree.bounds
    assert b is not None
    assert b.x == 0
    assert b.y == 0
    assert b.width == 1920
    assert b.height == 1080


def test_bounds_button(test_app):
    buttons = test_app.locator("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    b = back.bounds
    assert b.x == 110
    assert b.y == 60
    assert b.width == 50
    assert b.height == 30


def test_bounds_none(test_app):
    toolbar = test_app.locator("toolbar").elements()[0]
    assert toolbar.bounds is None


# ── Rect repr ────────────────────────────────────────────────────────────────


def test_rect_repr(test_app):
    tree = test_app.elements()
    b = tree.bounds
    r = repr(b)
    assert "Rect(" in r
    assert "x=0" in r
    assert "width=1920" in r


def test_rect_eq(test_app):
    tree = test_app.elements()
    b1 = tree.bounds
    b2 = tree.bounds
    assert b1 == b2


# ── Element dunders ───────────────────────────────────────────────────────────


def test_element_repr_basic(test_app):
    tree = test_app.elements()
    r = repr(tree)
    assert "role='application'" in r
    assert "name='TestApp'" in r


def test_element_repr_with_value(test_app):
    search = test_app.locator("text_field").elements()[0]
    r = repr(search)
    assert "value='hello'" in r


def test_element_repr_disabled(test_app):
    buttons = test_app.locator("button").elements()
    fwd = next(b for b in buttons if b.name == "Forward")
    r = repr(fwd)
    assert "enabled=False" in r


def test_element_repr_hidden(test_app):
    status = test_app.locator("static_text").elements()[0]
    r = repr(status)
    assert "visible=False" in r


def test_element_repr_focused(test_app):
    tree = test_app.elements()
    window = tree.children[0]
    r = repr(window)
    assert "focused=True" in r


def test_element_str_is_repr(test_app):
    tree = test_app.elements()
    assert str(tree) == repr(tree)


def test_element_len_children_count(test_app):
    tree = test_app.elements()
    assert len(tree) == 1  # one child (window)
    window = tree.children[0]
    assert len(window) == 2  # toolbar + group
    buttons = test_app.locator("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    assert len(back) == 0  # leaf
