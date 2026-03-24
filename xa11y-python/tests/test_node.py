"""Tests for Node class: fields, state flags, bounds, actions, dunders."""


# ── Core fields ──────────────────────────────────────────────────────────────


def test_node_role_as_string(tree):
    assert tree.root.role == "application"
    buttons = tree.query("button")
    assert buttons[0].role == "button"


def test_node_name(tree):
    root = tree.root
    assert root.name == "TestApp"


def test_node_name_none(tree):
    # All nodes in the test tree have names, but check type
    assert isinstance(tree.root.name, str)


def test_node_value(tree):
    search = tree.query("text_field")[0]
    assert search.value == "hello"


def test_node_value_none(tree):
    assert tree.root.value is None


def test_node_description(tree):
    root = tree.root
    assert root.description == "Test application"
    back = next(b for b in tree.query("button") if b.name == "Back")
    assert back.description == "Go back"


def test_node_description_none(tree):
    fwd = next(b for b in tree.query("button") if b.name == "Forward")
    assert fwd.description is None


def test_node_depth(tree):
    assert tree.root.depth == 0
    window = tree.root.children[0]
    assert window.depth == 1
    buttons = tree.query("button")
    assert all(b.depth == 3 for b in buttons)


def test_node_stable_id(tree):
    assert tree.root.stable_id == "app-root"
    back = next(b for b in tree.query("button") if b.name == "Back")
    assert back.stable_id == "btn-back"


def test_node_stable_id_none(tree):
    fwd = next(b for b in tree.query("button") if b.name == "Forward")
    assert fwd.stable_id is None


# ── Numeric values ───────────────────────────────────────────────────────────


def test_numeric_value(tree):
    slider = tree.query("slider")[0]
    assert slider.numeric_value == 75.0


def test_min_value(tree):
    slider = tree.query("slider")[0]
    assert slider.min_value == 0.0


def test_max_value(tree):
    slider = tree.query("slider")[0]
    assert slider.max_value == 100.0


def test_numeric_value_none(tree):
    assert tree.root.numeric_value is None
    assert tree.root.min_value is None
    assert tree.root.max_value is None


# ── Flattened state flags ────────────────────────────────────────────────────


def test_enabled_default_true(tree):
    assert tree.root.enabled is True


def test_enabled_false(tree):
    fwd = next(b for b in tree.query("button") if b.name == "Forward")
    assert fwd.enabled is False


def test_visible_default_true(tree):
    assert tree.root.visible is True


def test_visible_false(tree):
    status = tree.query("static_text")[0]
    assert status.visible is False


def test_focused(tree):
    window = tree.root.children[0]
    assert window.focused is True
    assert tree.root.focused is False


def test_checked_on(tree):
    cb = tree.query("check_box")[0]
    assert cb.checked == "on"


def test_checked_none(tree):
    # Non-checkable elements have checked=None
    assert tree.root.checked is None
    buttons = tree.query("button")
    assert all(b.checked is None for b in buttons)


def test_selected(tree):
    items = tree.query("list_item")
    item1 = next(i for i in items if i.name == "Item 1")
    item2 = next(i for i in items if i.name == "Item 2")
    assert item1.selected is True
    assert item2.selected is False


def test_expanded(tree):
    lst = tree.query("list")[0]
    assert lst.expanded is True


def test_expanded_none(tree):
    # Non-expandable elements have expanded=None
    assert tree.root.expanded is None


def test_editable(tree):
    search = tree.query("text_field")[0]
    assert search.editable is True
    assert tree.root.editable is False


def test_focusable(tree):
    back = next(b for b in tree.query("button") if b.name == "Back")
    assert back.focusable is True
    assert tree.root.focusable is False


def test_modal(tree):
    assert tree.root.modal is False


def test_required(tree):
    assert tree.root.required is False


def test_busy(tree):
    assert tree.root.busy is False


# ── Actions list ─────────────────────────────────────────────────────────────


def test_actions_button(tree):
    back = next(b for b in tree.query("button") if b.name == "Back")
    assert back.actions == ["press", "focus"]


def test_actions_text_field(tree):
    search = tree.query("text_field")[0]
    assert set(search.actions) == {"focus", "set_value", "type_text"}


def test_actions_slider(tree):
    slider = tree.query("slider")[0]
    assert set(slider.actions) == {"increment", "decrement", "set_value", "focus"}


def test_actions_checkbox(tree):
    cb = tree.query("check_box")[0]
    assert set(cb.actions) == {"toggle", "focus"}


def test_actions_empty(tree):
    assert tree.root.actions == []


# ── Bounds ───────────────────────────────────────────────────────────────────


def test_bounds_present(tree):
    root = tree.root
    b = root.bounds
    assert b is not None
    assert b.x == 0
    assert b.y == 0
    assert b.width == 1920
    assert b.height == 1080


def test_bounds_button(tree):
    back = next(b for b in tree.query("button") if b.name == "Back")
    b = back.bounds
    assert b.x == 110
    assert b.y == 60
    assert b.width == 50
    assert b.height == 30


def test_bounds_none(tree):
    toolbar = tree.query("toolbar")[0]
    assert toolbar.bounds is None


def test_bounds_normalized(tree):
    root = tree.root
    bn = root.bounds_normalized
    assert bn is not None
    assert bn.left == 0.0
    assert bn.top == 0.0
    assert bn.right == 1.0
    assert bn.bottom == 1.0


def test_bounds_normalized_none(tree):
    back = next(b for b in tree.query("button") if b.name == "Back")
    assert back.bounds_normalized is None


# ── Rect / NormalizedRect repr ───────────────────────────────────────────────


def test_rect_repr(tree):
    b = tree.root.bounds
    r = repr(b)
    assert "Rect(" in r
    assert "x=0" in r
    assert "width=1920" in r


def test_rect_eq(tree):
    b1 = tree.root.bounds
    b2 = tree.root.bounds
    assert b1 == b2


def test_normalized_rect_repr(tree):
    bn = tree.root.bounds_normalized
    r = repr(bn)
    assert "NormalizedRect(" in r


# ── Node dunders ─────────────────────────────────────────────────────────────


def test_node_repr_basic(tree):
    r = repr(tree.root)
    assert "role='application'" in r
    assert "name='TestApp'" in r


def test_node_repr_with_value(tree):
    search = tree.query("text_field")[0]
    r = repr(search)
    assert "value='hello'" in r


def test_node_repr_disabled(tree):
    fwd = next(b for b in tree.query("button") if b.name == "Forward")
    r = repr(fwd)
    assert "enabled=False" in r


def test_node_repr_hidden(tree):
    status = tree.query("static_text")[0]
    r = repr(status)
    assert "visible=False" in r


def test_node_repr_focused(tree):
    window = tree.root.children[0]
    r = repr(window)
    assert "focused=True" in r


def test_node_str_is_repr(tree):
    assert str(tree.root) == repr(tree.root)


def test_node_len_children_count(tree):
    root = tree.root
    assert len(root) == 1  # one child (window)
    window = root.children[0]
    assert len(window) == 2  # toolbar + group
    back = next(b for b in tree.query("button") if b.name == "Back")
    assert len(back) == 0  # leaf
