"""Tests for Element class: fields, state flags, bounds, actions, dunders."""


# ── Core fields ──────────────────────────────────────────────────────────────


def test_element_role_as_string(test_app):
    app = test_app.element()
    assert app.role == "application"
    buttons = test_app.descendant("button").elements()
    assert buttons[0].role == "button"


def test_element_name(test_app):
    app = test_app.element()
    assert app.name == "TestApp"


def test_element_name_none(test_app):
    app = test_app.element()
    assert isinstance(app.name, str)


def test_element_value(test_app):
    search = test_app.descendant("text_field").elements()[0]
    assert search.value == "hello"


def test_element_value_none(test_app):
    app = test_app.element()
    assert app.value is None


def test_element_description(test_app):
    app = test_app.element()
    assert app.description == "Test application"
    buttons = test_app.descendant("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    assert back.description == "Go back"


def test_element_description_none(test_app):
    buttons = test_app.descendant("button").elements()
    fwd = next(b for b in buttons if b.name == "Forward")
    assert fwd.description is None


def test_element_stable_id(test_app):
    app = test_app.element()
    assert app.stable_id == "app-root"
    buttons = test_app.descendant("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    assert back.stable_id == "btn-back"


def test_element_stable_id_none(test_app):
    buttons = test_app.descendant("button").elements()
    fwd = next(b for b in buttons if b.name == "Forward")
    assert fwd.stable_id is None


# ── Numeric values ───────────────────────────────────────────────────────────


def test_numeric_value(test_app):
    slider = test_app.descendant("slider").elements()[0]
    assert slider.numeric_value == 75.0


def test_min_value(test_app):
    slider = test_app.descendant("slider").elements()[0]
    assert slider.min_value == 0.0


def test_max_value(test_app):
    slider = test_app.descendant("slider").elements()[0]
    assert slider.max_value == 100.0


def test_numeric_value_none(test_app):
    app = test_app.element()
    assert app.numeric_value is None
    assert app.min_value is None
    assert app.max_value is None


# ── Flattened state flags ────────────────────────────────────────────────────


def test_enabled_default_true(test_app):
    app = test_app.element()
    assert app.enabled is True


def test_enabled_false(test_app):
    buttons = test_app.descendant("button").elements()
    fwd = next(b for b in buttons if b.name == "Forward")
    assert fwd.enabled is False


def test_visible_default_true(test_app):
    app = test_app.element()
    assert app.visible is True


def test_visible_false(test_app):
    status = test_app.descendant("static_text").elements()[0]
    assert status.visible is False


def test_focused(test_app):
    app = test_app.element()
    window = app.children()[0]
    assert window.focused is True
    assert app.focused is False


def test_active_window(test_app):
    # The mock's main window models the foreground/active window; the
    # application root is not a window and reports `active` False.
    app = test_app.element()
    window = app.children()[0]
    assert window.active is True
    assert app.active is False


def test_checked_on(test_app):
    cb = test_app.descendant("check_box").elements()[0]
    assert cb.checked == "on"


def test_checked_none(test_app):
    app = test_app.element()
    assert app.checked is None
    buttons = test_app.descendant("button").elements()
    assert all(b.checked is None for b in buttons)


def test_selected(test_app):
    items = test_app.descendant("list_item").elements()
    item1 = next(i for i in items if i.name == "Item 1")
    item2 = next(i for i in items if i.name == "Item 2")
    assert item1.selected is True
    assert item2.selected is False


def test_expanded(test_app):
    lst = test_app.descendant("list").elements()[0]
    assert lst.expanded is True


def test_expanded_none(test_app):
    app = test_app.element()
    assert app.expanded is None


def test_editable(test_app):
    app = test_app.element()
    search = test_app.descendant("text_field").elements()[0]
    assert search.editable is True
    assert app.editable is False


def test_focusable(test_app):
    app = test_app.element()
    buttons = test_app.descendant("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    assert back.focusable is True
    assert app.focusable is False


def test_modal(test_app):
    app = test_app.element()
    assert app.modal is False


def test_required(test_app):
    app = test_app.element()
    assert app.required is False


def test_busy(test_app):
    app = test_app.element()
    assert app.busy is False


# ── Raw platform data ────────────────────────────────────────────────────────


def test_raw_exposes_provider_metadata(test_app):
    """`Element.raw` exposes the platform-specific raw data map. The shared
    mock sets `{"ax_role": "AXApplication"}` on the application node so we
    have a concrete value to assert against."""
    app = test_app.element()
    assert isinstance(app.raw, dict)
    assert app.raw == {"ax_role": "AXApplication"}


def test_raw_defaults_to_empty(test_app):
    """Elements without raw metadata should expose an empty dict (not None)."""
    buttons = test_app.descendant("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    assert back.raw == {}


# ── Actions list ─────────────────────────────────────────────────────────────


def test_actions_button(test_app):
    buttons = test_app.descendant("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    assert back.actions == ["press", "focus"]


def test_actions_text_field(test_app):
    search = test_app.descendant("text_field").elements()[0]
    assert set(search.actions) == {"focus", "set_value", "type_text"}


def test_actions_slider(test_app):
    slider = test_app.descendant("slider").elements()[0]
    assert set(slider.actions) == {"increment", "decrement", "set_value", "focus"}


def test_actions_checkbox(test_app):
    cb = test_app.descendant("check_box").elements()[0]
    assert set(cb.actions) == {"press", "focus"}


def test_actions_empty(test_app):
    app = test_app.element()
    assert app.actions == []


# ── Bounds ───────────────────────────────────────────────────────────────────


def test_bounds_present(test_app):
    app = test_app.element()
    b = app.bounds
    assert b is not None
    assert b.x == 0
    assert b.y == 0
    assert b.width == 1920
    assert b.height == 1080


def test_bounds_button(test_app):
    buttons = test_app.descendant("button").elements()
    back = next(b for b in buttons if b.name == "Back")
    b = back.bounds
    assert b.x == 110
    assert b.y == 60
    assert b.width == 50
    assert b.height == 30


def test_bounds_none(test_app):
    toolbar = test_app.descendant("toolbar").elements()[0]
    assert toolbar.bounds is None


# ── Rect repr ────────────────────────────────────────────────────────────────


def test_rect_repr(test_app):
    app = test_app.element()
    b = app.bounds
    r = repr(b)
    assert "Rect(" in r
    assert "x=0" in r
    assert "width=1920" in r


def test_rect_eq(test_app):
    app = test_app.element()
    b1 = app.bounds
    b2 = app.bounds
    assert b1 == b2


# ── Element dunders ───────────────────────────────────────────────────────────


def test_element_repr_basic(test_app):
    app = test_app.element()
    r = repr(app)
    assert "role='application'" in r
    assert "name='TestApp'" in r


def test_element_repr_with_value(test_app):
    search = test_app.descendant("text_field").elements()[0]
    r = repr(search)
    assert "value='hello'" in r


def test_element_repr_disabled(test_app):
    buttons = test_app.descendant("button").elements()
    fwd = next(b for b in buttons if b.name == "Forward")
    r = repr(fwd)
    assert "enabled=False" in r


def test_element_repr_hidden(test_app):
    status = test_app.descendant("static_text").elements()[0]
    r = repr(status)
    assert "visible=False" in r


def test_element_repr_focused(test_app):
    app = test_app.element()
    window = app.children()[0]
    r = repr(window)
    assert "focused=True" in r


def test_element_str_is_repr(test_app):
    app = test_app.element()
    assert str(app) == repr(app)
