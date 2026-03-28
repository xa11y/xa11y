"""Tests for Node class: fields, state flags, bounds, actions, dunders."""


# ── Core fields ──────────────────────────────────────────────────────────────


def test_node_role_as_string(test_app):
    tree = test_app.nodes()
    assert tree.role == "application"
    buttons = test_app.locator("button").nodes()
    assert buttons[0].role == "button"


def test_node_name(tree):
    assert tree.name == "TestApp"


def test_node_name_none(tree):
    # All nodes in the test tree have names, but check type
    assert isinstance(tree.name, str)


def test_node_value(test_app):
    search = test_app.locator("text_field").nodes()[0]
    assert search.value == "hello"


def test_node_value_none(tree):
    assert tree.value is None


def test_node_description(test_app):
    tree = test_app.nodes()
    assert tree.description == "Test application"
    buttons = test_app.locator("button").nodes()
    back = next(b for b in buttons if b.name == "Back")
    assert back.description == "Go back"


def test_node_description_none(test_app):
    buttons = test_app.locator("button").nodes()
    fwd = next(b for b in buttons if b.name == "Forward")
    assert fwd.description is None


def test_node_stable_id(test_app):
    tree = test_app.nodes()
    assert tree.stable_id == "app-root"
    buttons = test_app.locator("button").nodes()
    back = next(b for b in buttons if b.name == "Back")
    assert back.stable_id == "btn-back"


def test_node_stable_id_none(test_app):
    buttons = test_app.locator("button").nodes()
    fwd = next(b for b in buttons if b.name == "Forward")
    assert fwd.stable_id is None


# ── Numeric values ───────────────────────────────────────────────────────────


def test_numeric_value(test_app):
    slider = test_app.locator("slider").nodes()[0]
    assert slider.numeric_value == 75.0


def test_min_value(test_app):
    slider = test_app.locator("slider").nodes()[0]
    assert slider.min_value == 0.0


def test_max_value(test_app):
    slider = test_app.locator("slider").nodes()[0]
    assert slider.max_value == 100.0


def test_numeric_value_none(tree):
    assert tree.numeric_value is None
    assert tree.min_value is None
    assert tree.max_value is None


# ── Flattened state flags ────────────────────────────────────────────────────


def test_enabled_default_true(tree):
    assert tree.enabled is True


def test_enabled_false(test_app):
    buttons = test_app.locator("button").nodes()
    fwd = next(b for b in buttons if b.name == "Forward")
    assert fwd.enabled is False


def test_visible_default_true(tree):
    assert tree.visible is True


def test_visible_false(test_app):
    status = test_app.locator("static_text").nodes()[0]
    assert status.visible is False


def test_focused(tree):
    window = tree.children[0]
    assert window.focused is True
    assert tree.focused is False


def test_checked_on(test_app):
    cb = test_app.locator("check_box").nodes()[0]
    assert cb.checked == "on"


def test_checked_none(test_app):
    tree = test_app.nodes()
    assert tree.checked is None
    buttons = test_app.locator("button").nodes()
    assert all(b.checked is None for b in buttons)


def test_selected(test_app):
    items = test_app.locator("list_item").nodes()
    item1 = next(i for i in items if i.name == "Item 1")
    item2 = next(i for i in items if i.name == "Item 2")
    assert item1.selected is True
    assert item2.selected is False


def test_expanded(test_app):
    lst = test_app.locator("list").nodes()[0]
    assert lst.expanded is True


def test_expanded_none(tree):
    # Non-expandable elements have expanded=None
    assert tree.expanded is None


def test_editable(test_app):
    tree = test_app.nodes()
    search = test_app.locator("text_field").nodes()[0]
    assert search.editable is True
    assert tree.editable is False


def test_focusable(test_app):
    tree = test_app.nodes()
    buttons = test_app.locator("button").nodes()
    back = next(b for b in buttons if b.name == "Back")
    assert back.focusable is True
    assert tree.focusable is False


def test_modal(tree):
    assert tree.modal is False


def test_required(tree):
    assert tree.required is False


def test_busy(tree):
    assert tree.busy is False


# ── Actions list ─────────────────────────────────────────────────────────────


def test_actions_button(test_app):
    buttons = test_app.locator("button").nodes()
    back = next(b for b in buttons if b.name == "Back")
    assert back.actions == ["press", "focus"]


def test_actions_text_field(test_app):
    search = test_app.locator("text_field").nodes()[0]
    assert set(search.actions) == {"focus", "set_value", "type_text"}


def test_actions_slider(test_app):
    slider = test_app.locator("slider").nodes()[0]
    assert set(slider.actions) == {"increment", "decrement", "set_value", "focus"}


def test_actions_checkbox(test_app):
    cb = test_app.locator("check_box").nodes()[0]
    assert set(cb.actions) == {"toggle", "focus"}


def test_actions_empty(tree):
    assert tree.actions == []


# ── Bounds ───────────────────────────────────────────────────────────────────


def test_bounds_present(tree):
    b = tree.bounds
    assert b is not None
    assert b.x == 0
    assert b.y == 0
    assert b.width == 1920
    assert b.height == 1080


def test_bounds_button(test_app):
    buttons = test_app.locator("button").nodes()
    back = next(b for b in buttons if b.name == "Back")
    b = back.bounds
    assert b.x == 110
    assert b.y == 60
    assert b.width == 50
    assert b.height == 30


def test_bounds_none(test_app):
    toolbar = test_app.locator("toolbar").nodes()[0]
    assert toolbar.bounds is None


# ── Rect repr ────────────────────────────────────────────────────────────────


def test_rect_repr(tree):
    b = tree.bounds
    r = repr(b)
    assert "Rect(" in r
    assert "x=0" in r
    assert "width=1920" in r


def test_rect_eq(tree):
    b1 = tree.bounds
    b2 = tree.bounds
    assert b1 == b2


# ── Node dunders ─────────────────────────────────────────────────────────────


def test_node_repr_basic(tree):
    r = repr(tree)
    assert "role='application'" in r
    assert "name='TestApp'" in r


def test_node_repr_with_value(test_app):
    search = test_app.locator("text_field").nodes()[0]
    r = repr(search)
    assert "value='hello'" in r


def test_node_repr_disabled(test_app):
    buttons = test_app.locator("button").nodes()
    fwd = next(b for b in buttons if b.name == "Forward")
    r = repr(fwd)
    assert "enabled=False" in r


def test_node_repr_hidden(test_app):
    status = test_app.locator("static_text").nodes()[0]
    r = repr(status)
    assert "visible=False" in r


def test_node_repr_focused(tree):
    window = tree.children[0]
    r = repr(window)
    assert "focused=True" in r


def test_node_str_is_repr(tree):
    assert str(tree) == repr(tree)


def test_node_len_children_count(test_app):
    tree = test_app.nodes()
    assert len(tree) == 1  # one child (window)
    window = tree.children[0]
    assert len(window) == 2  # toolbar + group
    buttons = test_app.locator("button").nodes()
    back = next(b for b in buttons if b.name == "Back")
    assert len(back) == 0  # leaf
