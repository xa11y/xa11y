"""Tests for the `ShellSurface` binding surface, against the mock provider.

`ShellSurface.list()` / `.by_kind()` resolve the platform singleton provider,
which no CI runner without a desktop session has. `_make_test_shell_surfaces()`
and `_find_test_shell_surface()` run the identical binding code against the
shared mock, whose fixture is two surfaces:

    taskbar  "Taskbar" (pid 4242)  -> button "Show Hidden Icons", button "Volume"
    desktop  "Desktop" (pid 4242)  -> list_item "Trash"

Argument validation is checked through the *public* entry points, since it
happens before the provider is resolved.

Not covered here: the ambiguity refusal (two surfaces of one kind). The mock's
fixture is fixed, so producing it needs a second provider implementation —
`xa11y-core`'s `by_kind_with_refuses_ambiguity_immediately` covers the
behaviour, and the failure it raises is the same `SelectorNotMatched` +
`candidates` shape asserted below for the zero-match case.
"""

import pytest
import xa11y
from xa11y._native import _find_test_shell_surface, _make_test_shell_surfaces

MOCK_SHELL_PID = 4242


@pytest.fixture
def surfaces():
    """The mock provider's shell surfaces."""
    return _make_test_shell_surfaces()


@pytest.fixture
def taskbar():
    """The mock provider's taskbar surface, resolved by kind."""
    return _find_test_shell_surface("taskbar", timeout=0.0)


# ── list ─────────────────────────────────────────────────────────────────────


def test_list_returns_the_fixture_surfaces(surfaces):
    assert [s.kind for s in surfaces] == ["taskbar", "desktop"]
    assert [s.name for s in surfaces] == ["Taskbar", "Desktop"]
    assert [s.pid for s in surfaces] == [MOCK_SHELL_PID, MOCK_SHELL_PID]


def test_kind_is_a_snake_case_string(surfaces):
    # The kind crosses as the identically-spelled snake_case string, like a
    # role — not as an enum object.
    for surface in surfaces:
        assert isinstance(surface.kind, str)
        assert surface.kind == surface.kind.lower()


def test_surface_properties_are_read_only(surfaces):
    with pytest.raises(AttributeError):
        surfaces[0].kind = "dock"


def test_repr_names_kind_name_and_pid(surfaces):
    expected = f"ShellSurface(kind='taskbar', name='Taskbar', pid={MOCK_SHELL_PID})"
    assert repr(surfaces[0]) == expected
    assert str(surfaces[0]) == repr(surfaces[0])


# ── by_kind ──────────────────────────────────────────────────────────────────


def test_by_kind_resolves_a_unique_surface():
    desktop = _find_test_shell_surface("desktop", timeout=0.0)
    assert desktop.kind == "desktop"
    assert desktop.name == "Desktop"
    assert desktop.pid == MOCK_SHELL_PID


def test_by_kind_defaults_to_the_process_default_timeout():
    # `timeout=None` (the default) must resolve rather than raise — the same
    # default-timeout handling `App.by_name` has. A present surface returns
    # immediately, so this does not wait.
    assert _find_test_shell_surface("taskbar").kind == "taskbar"


def test_by_kind_with_no_such_surface_raises_with_candidates():
    # Tenet 6: the failure names what *was* found, as structured attributes.
    with pytest.raises(xa11y.SelectorNotMatchedError) as excinfo:
        _find_test_shell_surface("dock", timeout=0.0)
    err = excinfo.value
    assert err.selector == "shell_surface[kind=dock]"
    assert err.condition == "a dock shell surface"
    assert "no dock surface present" in err.last_observed
    assert err.candidates == [
        f'taskbar "Taskbar" (pid={MOCK_SHELL_PID})',
        f'desktop "Desktop" (pid={MOCK_SHELL_PID})',
    ]
    # The same content is rendered into the message, not only the attributes.
    assert "dock" in str(err)


# ── Argument parsing (before the provider is touched) ────────────────────────


def test_unknown_kind_raises_value_error_without_touching_the_provider():
    # Parse-before-OS-call: the public entry point resolves the platform
    # provider, which a headless runner has none of — a `ValueError` here (and
    # not a PlatformError) proves the parse ran first.
    with pytest.raises(ValueError) as excinfo:
        xa11y.ShellSurface.by_kind("not_a_kind", timeout=0.0)
    message = str(excinfo.value)
    assert "not_a_kind" in message
    for kind in ("menu_bar", "status_items", "taskbar", "panel", "dock", "desktop", "flyout"):
        assert kind in message


def test_unknown_kind_raises_value_error_on_the_mock_path_too():
    with pytest.raises(ValueError, match="unknown shell surface kind"):
        _find_test_shell_surface("Taskbar", timeout=0.0)


def test_by_kind_rejects_negative_timeout():
    with pytest.raises(ValueError, match="non-negative"):
        xa11y.ShellSurface.by_kind("taskbar", timeout=-1.0)


def test_by_kind_rejects_nan_timeout():
    with pytest.raises(ValueError, match="non-negative"):
        xa11y.ShellSurface.by_kind("taskbar", timeout=float("nan"))


def test_by_kind_timeout_is_keyword_only():
    # `#[pyo3(signature = (kind, *, timeout=None))]` mirrors `App.by_name`.
    with pytest.raises(TypeError):
        xa11y.ShellSurface.by_kind("taskbar", 0.0)


def test_unknown_kind_wins_over_a_bad_timeout():
    # Both are parsed before any OS call; the kind is parsed first.
    with pytest.raises(ValueError, match="unknown shell surface kind"):
        xa11y.ShellSurface.by_kind("not_a_kind", timeout=-1.0)


# ── Queries rooted at the surface ────────────────────────────────────────────


def test_locator_finds_the_tray_chevron(taskbar):
    chevron = taskbar.locator("button[name='Show Hidden Icons']").element()
    assert chevron.role == "button"
    assert chevron.name == "Show Hidden Icons"
    assert chevron.pid == MOCK_SHELL_PID


def test_locator_is_scoped_to_the_surface(taskbar):
    # The application tree is not in scope: the surface root is its own subtree.
    with pytest.raises(xa11y.SelectorNotMatchedError):
        taskbar.locator("button[name='Back']").element()


def test_children_are_the_surface_root_children(taskbar):
    assert [c.name for c in taskbar.children()] == ["Show Hidden Icons", "Volume"]


def test_as_element_exposes_the_surface_root(taskbar):
    root = taskbar.as_element()
    assert root.name == "Taskbar"
    assert root.role == "toolbar"


def test_root_carries_the_kind_as_a_raw_attribute(taskbar):
    # Stamped by core in one place, on the surface root only. `as_element()`
    # is where it is read back; it is not a selector, and `tree`/`dump` do not
    # carry raw attributes.
    assert taskbar.as_element().raw["shell_kind"] == "taskbar"


def test_tree_is_rooted_at_the_surface(taskbar):
    tree = taskbar.tree()
    assert tree["name"] == "Taskbar"
    assert [child["name"] for child in tree["children"]] == ["Show Hidden Icons", "Volume"]


def test_tree_respects_max_depth(taskbar):
    assert taskbar.tree(0)["children"] == []


def test_dump_renders_the_surface_subtree(taskbar):
    dump = taskbar.dump()
    assert "Taskbar" in dump
    assert "Show Hidden Icons" in dump


def test_dump_respects_max_depth(taskbar):
    assert "Show Hidden Icons" not in taskbar.dump(0)
