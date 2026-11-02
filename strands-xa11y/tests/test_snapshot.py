"""The snapshot is what the model reads every turn; its shape is a contract."""

from __future__ import annotations

import re

import fake_xa11y
import pytest

from strands_xa11y._actions import run
from strands_xa11y._refs import REFS
from strands_xa11y._snapshot import _Budget, collect_rich, describe_element, properties
from strands_xa11y.models import SnapshotAction


def snapshot(**fields) -> str:
    result = run(SnapshotAction(type="snapshot", **fields))
    assert result["status"] == "success", result
    return result["content"][0]["text"]


class Vanished(fake_xa11y.Element):
    """An element destroyed between the parent's children() call and its own read."""

    _BLOCKED = frozenset({"role", "name", "value", "stable_id", "enabled", "visible", "focused", "checked"})

    def __getattribute__(self, attribute):
        if attribute in Vanished._BLOCKED:
            raise fake_xa11y.PlatformError("element was destroyed mid-walk")
        return super().__getattribute__(attribute)


def test_snapshot_renders_refs_names_values_and_states(editor):
    text = snapshot(app="TextEdit")
    assert "TextEdit (pid 4242)" in text
    assert re.search(r"e\d+ button \"Bold\"", text)
    assert re.search(r"e\d+ button \"Italic\" \[disabled\]", text)
    assert re.search(r"e\d+ text_field \"File name\" value=\"untitled\"", text)
    assert 'check_box "Wrap lines" [unchecked]' in text


def test_snapshot_indentation_tracks_tree_depth(editor):
    indents = {}
    for line in snapshot(app="TextEdit").splitlines():
        node = re.match(r"^(\s*)e\d+ (\w+)", line)
        if node:
            indents[node.group(2)] = len(node.group(1))
    assert indents["application"] == 0
    assert indents["window"] == 2
    assert indents["toolbar"] == 4
    assert indents["button"] == 6


def test_interactive_only_drops_decoration_but_keeps_containers(editor):
    text = snapshot(app="TextEdit")
    assert "separator" not in text
    assert " image" not in text
    # The unnamed group carries no meaning itself, but its children do.
    assert "group" in text


def test_everything_is_shown_when_the_filter_is_off(editor):
    text = snapshot(app="TextEdit", interactive_only=False)
    assert "separator" in text
    assert "image" in text


def test_truncation_is_reported_never_silent(editor):
    text = snapshot(app="TextEdit", max_nodes=3)
    assert "TRUNCATED" in text
    assert "max_nodes" in text


def test_depth_limit_stops_the_walk(editor):
    text = snapshot(app="TextEdit", max_depth=1)
    assert "window" in text
    assert "toolbar" not in text


def test_basic_detail_skips_state_reads(editor):
    """'basic' trades per-node state for a single bulk call — the Linux escape hatch."""
    text = snapshot(app="TextEdit", detail="basic")
    assert "button" in text
    assert "[disabled]" not in text


def test_bounds_are_opt_in(editor):
    assert "@10,20 300x400" not in snapshot(app="TextEdit")
    assert "@10,20 300x400" in snapshot(app="TextEdit", include_bounds=True)


def test_scoping_to_a_selector_narrows_the_tree(editor):
    text = snapshot(app="TextEdit", selector="toolbar")
    assert "button" in text
    assert "text_area" not in text


def test_scoping_to_a_ref_narrows_the_tree_to_that_subtree(editor):
    toolbar_ref = next(
        line.strip().split(" ", 1)[0] for line in snapshot(app="TextEdit").splitlines() if " toolbar" in line
    )
    text = snapshot(ref=toolbar_ref)
    assert "scope:" in text
    assert 'button "Bold"' in text
    assert "text_area" not in text


def test_a_snapshot_can_only_be_scoped_one_way():
    with pytest.raises(Exception, match="not both"):
        SnapshotAction(type="snapshot", selector="toolbar", ref="e1")


def test_long_values_are_truncated_rather_than_spent_in_full(editor):
    editor.as_element().children()[0]._children[2].value = "word " * 200
    line = next(line for line in snapshot(app="TextEdit").splitlines() if "text_area" in line)
    assert "…" in line
    assert len(line) < 250


def test_whitespace_in_values_is_collapsed_to_one_line(editor):
    editor.as_element().children()[0]._children[2].value = "first\n\nsecond   third"
    assert 'value="first second third"' in snapshot(app="TextEdit")


def test_a_value_that_is_only_whitespace_is_treated_as_no_value(editor):
    editor.as_element().children()[0]._children[2].value = "   \n\t "
    assert "value=" not in next(line for line in snapshot(app="TextEdit").splitlines() if "text_area" in line)


def test_a_basic_snapshot_reports_truncation_the_same_way(editor):
    text = snapshot(app="TextEdit", detail="basic", max_nodes=3)
    assert "TRUNCATED" in text


def test_the_depth_limit_applies_to_basic_snapshots_too(editor):
    text = snapshot(app="TextEdit", detail="basic", max_depth=1)
    assert "window" in text
    assert "toolbar" not in text


def test_a_scope_with_no_expressible_path_still_issues_usable_refs(editor):
    """Refs under such a scope fall back to stable_id or a captured handle rather than a bad path."""
    from strands_xa11y._session import app_key

    element = editor.as_element().children()[0]._children[1]
    entry = REFS.issue(app_key(editor), "group", element=element)  # no path
    assert entry.path is None

    text = snapshot(ref=entry.ref)
    field_ref = next(line.strip().split(" ", 1)[0] for line in text.splitlines() if "text_field" in line)
    assert REFS.get(field_ref).path is None
    assert REFS.get(field_ref).selectors() == ["[stable_id='tf-1']"]


# ── Degraded trees ───────────────────────────────────────────────────────────


def test_an_app_whose_root_cannot_be_read_says_so(editor):
    """The whole tree being unreadable is reported as such, not as an empty-but-fine app."""
    fake_xa11y.APPS.append(fake_xa11y.App("Opaque", pid=1, root=Vanished("application", "Opaque")))
    assert "(no accessible content)" in snapshot(app="Opaque")


def test_a_tree_of_pure_decoration_explains_how_to_see_it(editor):
    root = fake_xa11y.Element("application", "Decor", children=[fake_xa11y.Element("separator")])
    fake_xa11y.APPS.append(fake_xa11y.App("Decor", pid=2, root=root))
    assert "interactive_only=false" in snapshot(app="Decor")


def test_one_element_vanishing_mid_walk_does_not_take_its_siblings_with_it(editor):
    """A partial tree that looks complete is the failure mode the budget notes exist to avoid."""
    toolbar = editor.as_element().children()[0]._children[0]
    toolbar._children.insert(1, Vanished("button", "Doomed"))

    text = snapshot(app="TextEdit")
    assert 'button "Bold"' in text
    assert 'button "Italic"' in text  # the sibling after the vanished node
    assert "Doomed" not in text
    assert "TRUNCATED" not in text  # nothing was truncated; one node was unreadable


def test_a_container_that_cannot_list_its_children_still_reports_itself(editor):
    toolbar = editor.as_element().children()[0]._children[0]
    toolbar.children = lambda: (_ for _ in ()).throw(fake_xa11y.PlatformError("gone"))

    text = snapshot(app="TextEdit")
    assert "toolbar" in text
    assert 'button "Bold"' not in text
    assert 'text_field "File name"' in text  # the rest of the window survived


def test_the_node_budget_stops_the_walk_and_is_reported_not_confused_with_a_bad_read(editor):
    budget = _Budget(2)
    collect_rich(editor.as_element(), 10, budget, "", False)
    assert budget.truncated is True


# ── States ───────────────────────────────────────────────────────────────────


@pytest.mark.parametrize(
    ("states", "expected"),
    [
        ({"visible": False}, "hidden"),
        ({"focused": True}, "focused"),
        ({"checked": "mixed"}, "mixed"),
        ({"selected": True}, "selected"),
        ({"expanded": True}, "expanded"),
        ({"expanded": False}, "collapsed"),
        ({"required": True}, "required"),
        ({"busy": True}, "busy"),
        ({"modal": True}, "modal"),
        ({"active": True}, "active"),
        ({"password": True}, "password"),
    ],
)
def test_each_state_that_changes_what_to_do_next_is_rendered(editor, states, expected):
    editor.as_element().children()[0]._children[0]._children[0] = fake_xa11y.Element("button", "Bold", **states)
    line = next(line for line in snapshot(app="TextEdit").splitlines() if '"Bold"' in line)
    assert f"[{expected}" in line or f" {expected}]" in line


def test_states_a_reader_would_assume_are_left_out(editor):
    """Every element being 'enabled visible' would be noise on every line."""
    line = next(line for line in snapshot(app="TextEdit").splitlines() if '"Bold"' in line)
    assert "[" not in line


# ── Element rendering helpers ────────────────────────────────────────────────


def test_describe_element_reads_like_a_snapshot_line(editor):
    field = editor.as_element().children()[0]._children[1]._children[0]
    assert describe_element(field) == 'text_field "File name" value="untitled"'


def test_describe_element_omits_what_an_element_does_not_have(editor):
    assert describe_element(fake_xa11y.Element("group")) == "group"


def test_describe_element_carries_state_into_find_results(editor):
    disabled = editor.as_element().children()[0]._children[0]._children[1]
    assert describe_element(disabled) == 'button "Italic" [disabled]'


def test_properties_reports_a_failed_read_instead_of_losing_the_whole_element(editor):
    element = editor.as_element().children()[0]._children[2]
    type(element).description = property(lambda self: (_ for _ in ()).throw(fake_xa11y.PlatformError("nope")))
    try:
        read = properties(element)
    finally:
        del type(element).description

    assert "<unavailable" in read["description"]
    assert read["role"] == "text_area"  # every other property still came back


def test_properties_renders_bounds_as_plain_numbers(editor):
    element = editor.as_element().children()[0]._children[2]
    assert properties(element)["bounds"] == {"x": 10, "y": 20, "width": 300, "height": 400}


def test_properties_reports_bounds_that_could_not_be_measured(editor):
    element = editor.as_element().children()[0]._children[2]
    type(element).bounds = property(lambda self: (_ for _ in ()).throw(fake_xa11y.PlatformError("offscreen")))
    try:
        assert "<unavailable" in properties(element)["bounds"]
    finally:
        del type(element).bounds


def test_properties_distinguishes_no_bounds_from_unreadable_bounds(editor):
    element = editor.as_element().children()[0]._children[1]._children[0]
    assert properties(element)["bounds"] is None


def test_every_rendered_line_gets_a_ref_that_resolves_back(editor):
    text = snapshot(app="TextEdit")
    refs = re.findall(r"^\s*(e\d+) ", text, re.M)
    assert len(refs) == len(text.strip().splitlines()) - 2  # header + the interactive_only note
    assert all(REFS.get(ref).ref == ref for ref in refs)
