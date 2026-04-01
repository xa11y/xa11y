"""Tests for the xa11y CLI helpers (_cli module)."""

from xa11y._cli import _format_element, _parse_opts, _print_tree

# ── Argument parsing ─────────────────────────────────────────────────────────


def test_parse_opts_app_flag():
    opts, pos = _parse_opts(["--app", "Safari"])
    assert opts["app"] == "Safari"
    assert opts["pid"] is None
    assert pos == []


def test_parse_opts_pid_flag():
    opts, pos = _parse_opts(["--pid", "1234"])
    assert opts["pid"] == "1234"
    assert opts["app"] is None
    assert pos == []


def test_parse_opts_positional_and_flags():
    opts, pos = _parse_opts(["button[name='OK']", "--app", "MyApp"])
    assert opts["app"] == "MyApp"
    assert pos == ["button[name='OK']"]


def test_parse_opts_multiple_positional():
    opts, pos = _parse_opts(["press", "button", "--app", "Test"])
    assert opts["app"] == "Test"
    assert pos == ["press", "button"]


def test_parse_opts_empty():
    opts, pos = _parse_opts([])
    assert opts["app"] is None
    assert opts["pid"] is None
    assert pos == []


def test_parse_opts_value_flag():
    opts, pos = _parse_opts(["--app", "Foo", "--value", "hello"])
    assert opts["value"] == "hello"
    assert opts["app"] == "Foo"
    assert pos == []


# ── Format element ───────────────────────────────────────────────────────────


def test_format_element_basic(test_app):
    app = test_app.element()
    out = _format_element(app)
    assert out.startswith("application")
    assert '"TestApp"' in out
    assert "enabled" in out
    assert "visible" in out


def test_format_element_button(test_app):
    buttons = test_app.descendant("button").elements()
    out = _format_element(buttons[0])
    assert out.startswith("button")
    assert "actions=" in out


def test_format_element_with_value(test_app):
    search = test_app.descendant("text_field").elements()[0]
    out = _format_element(search)
    assert 'value="hello"' in out
    assert "editable" in out


def test_format_element_checked(test_app):
    cb = test_app.descendant("check_box").elements()[0]
    out = _format_element(cb)
    assert "checked=" in out


def test_format_element_expanded(test_app):
    lst = test_app.descendant("list").elements()[0]
    out = _format_element(lst)
    assert "expanded" in out


def test_format_element_numeric_value(test_app):
    slider = test_app.descendant("slider").elements()[0]
    out = _format_element(slider)
    assert "numeric_value=" in out
    assert "min=" in out
    assert "max=" in out


def test_format_element_hidden(test_app):
    hidden = test_app.descendant("static_text").elements()[0]
    out = _format_element(hidden)
    assert "hidden" in out


def test_format_element_stable_id(test_app):
    app = test_app.element()
    out = _format_element(app)
    assert 'id="app-root"' in out


def test_format_element_bounds(test_app):
    app = test_app.element()
    out = _format_element(app)
    assert "bounds=" in out


# ── Tree printing ────────────────────────────────────────────────────────────


def test_print_tree_no_crash(test_app, capsys):
    """Printing the full tree should not crash and should produce output."""
    root = test_app.element()
    _print_tree(root)
    captured = capsys.readouterr()
    assert "application" in captured.out
    assert "window" in captured.out
    assert "button" in captured.out


def test_print_tree_has_connectors(test_app, capsys):
    """Tree output should contain tree-drawing characters."""
    root = test_app.element()
    _print_tree(root)
    captured = capsys.readouterr()
    assert "├── " in captured.out or "└── " in captured.out


def test_print_tree_shows_all_children(test_app, capsys):
    """Tree should include leaf elements deep in the tree."""
    root = test_app.element()
    _print_tree(root)
    captured = capsys.readouterr()
    # These are deep children in the test tree
    assert "slider" in captured.out.lower() or "Slider" in captured.out
    assert "check_box" in captured.out
