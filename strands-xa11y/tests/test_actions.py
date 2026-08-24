"""Behaviour of each action, with the accessibility/input split as the thing under test."""

from __future__ import annotations

import re

import fake_xa11y
import pytest

from strands_xa11y import _actions
from strands_xa11y._actions import run
from strands_xa11y.models import (
    ActAction,
    ClickAction,
    ElementTarget,
    FindAction,
    KeyAction,
    ListAppsAction,
    PointerTarget,
    ReadAction,
    ScreenshotAction,
    ScrollAction,
    SnapshotAction,
    TypeAction,
    WaitAction,
)


def text_of(result) -> str:
    return result["content"][0]["text"]


def ref_for(editor, pattern: str) -> str:
    """Take a snapshot and pull out the ref of the first line matching a pattern."""
    snapshot = text_of(run(SnapshotAction(type="snapshot", app="TextEdit")))
    for line in snapshot.splitlines():
        if re.search(pattern, line):
            return line.strip().split(" ", 1)[0]
    raise AssertionError(f"no snapshot line matched {pattern!r}:\n{snapshot}")


def called(calls, name: str) -> list:
    return [entry for entry in calls if entry[0] == name]


# ── Perceive ─────────────────────────────────────────────────────────────────


def test_list_apps_marks_the_foreground(editor):
    result = text_of(run(ListAppsAction(type="list_apps")))
    assert "TextEdit (pid 4242) [foreground]" in result


def test_list_apps_with_nothing_running_blames_the_bridge_not_the_desktop():
    """An empty list almost always means a missing permission, so say that rather than '0 apps'."""
    result = text_of(run(ListAppsAction(type="list_apps")))
    assert "AT-SPI2" in result
    assert "Accessibility permission" in result


def test_find_assigns_a_ref_per_match_and_reports_the_total(editor):
    result = text_of(run(FindAction(type="find", app="TextEdit", selector="button")))
    assert "3 match(es)" in result
    assert len(re.findall(r"^e\d+ button", result, re.M)) == 3


def test_find_respects_its_limit_and_says_so(editor):
    result = text_of(run(FindAction(type="find", app="TextEdit", selector="button", limit=1)))
    assert "showing the first 1" in result


def test_find_on_no_match_is_a_success_with_nothing_in_it(editor):
    result = run(FindAction(type="find", app="TextEdit", selector="slider"))
    assert result["status"] == "success"
    assert "No element matches" in text_of(result)


def test_read_returns_the_property_set(editor):
    result = text_of(run(ReadAction(type="read", target=ElementTarget(app="TextEdit", selector="text_field"))))
    assert "role: 'text_field'" in result
    assert "value: 'untitled'" in result


def test_read_names_the_element_it_read(editor):
    result = text_of(run(ReadAction(type="read", target=ElementTarget(app="TextEdit", selector="check_box"))))
    assert result.startswith("selector 'check_box':")


def test_read_leaves_out_properties_the_element_does_not_have(editor):
    """Every empty property listed is tokens spent saying nothing."""
    result = text_of(run(ReadAction(type="read", target=ElementTarget(app="TextEdit", selector="text_field"))))
    assert "description:" not in result
    assert "min_value:" not in result


@pytest.mark.parametrize(
    "condition",
    ["visible", "hidden", "attached", "detached", "enabled", "disabled", "focused", "unfocused"],
)
def test_every_wait_condition_maps_onto_its_own_poll(editor, calls, condition):
    result = run(
        WaitAction(
            type="wait", target=ElementTarget(app="TextEdit", selector="button[name='Bold']"), condition=condition
        )
    )
    assert result["status"] == "success"
    assert called(calls, f"wait_{condition}")


def test_wait_passes_its_timeout_down(editor, calls):
    run(WaitAction(type="wait", target=ElementTarget(app="TextEdit", selector="text_area"), timeout=0.25))
    assert called(calls, "wait_visible")[0][1][1] == 0.25


def test_waiting_on_a_captured_handle_explains_why_it_cannot(editor):
    """A handle is a snapshot of the past; polling it would never observe a change."""
    from strands_xa11y._refs import REFS
    from strands_xa11y._session import app_key

    entry = REFS.issue(app_key(editor), "text_area", element=editor.as_element().children()[0]._children[2])
    result = run(WaitAction(type="wait", target=ElementTarget(ref=entry.ref)))
    assert result["status"] == "error"
    assert "Wait on a selector instead" in text_of(result)


# ── Acting through the accessibility layer ───────────────────────────────────


def test_plain_click_uses_the_accessibility_press(editor, calls):
    run(ClickAction(type="click", target=PointerTarget(app="TextEdit", selector="button[name='Bold']")))
    assert called(calls, "press")
    assert not called(calls, "input.click")


def test_double_click_falls_through_to_synthesised_input(editor, calls):
    """There is no accessibility action for 'double click', so this is tier three by necessity."""
    run(ClickAction(type="click", target=PointerTarget(app="TextEdit", selector="text_area"), count=2))
    assert called(calls, "input.click")
    assert not called(calls, "press")


def test_right_click_prefers_show_menu(editor, calls):
    run(ClickAction(type="click", target=PointerTarget(app="TextEdit", selector="text_area"), button="right"))
    assert called(calls, "show_menu")
    assert not called(calls, "input.click")


def test_right_click_synthesises_when_show_menu_is_unsupported(editor, calls):
    editor.as_element().children()[0]._children[2]._unsupported = ("show_menu",)
    run(ClickAction(type="click", target=PointerTarget(app="TextEdit", selector="text_area"), button="right"))
    assert called(calls, "input.click")


def test_click_on_a_point_never_touches_the_accessibility_layer(editor, calls):
    run(ClickAction(type="click", target=PointerTarget(point=(40, 90))))
    name, args, kwargs = called(calls, "input.click")[0]
    assert args[0] == (40, 90)


def test_type_inserts_and_replace_overwrites(editor, calls):
    target = ElementTarget(app="TextEdit", selector="text_field")
    run(TypeAction(type="type", target=target, text="-draft"))
    assert called(calls, "type_text")
    run(TypeAction(type="type", target=target, text="final", replace=True))
    assert called(calls, "set_value")


def test_type_without_a_target_goes_to_whatever_has_focus(editor, calls):
    run(TypeAction(type="type", text="hello", press_enter=True))
    assert called(calls, "input.type_text")
    assert called(calls, "input.press")


def test_type_still_types_into_a_control_that_will_not_take_focus(editor, calls):
    """Some controls accept text without ever accepting focus; refusing focus is not a failure."""
    editor.as_element().children()[0]._children[1]._children[0]._unsupported = ("focus",)

    result = run(TypeAction(type="type", target=ElementTarget(app="TextEdit", selector="text_field"), text="x"))
    assert result["status"] == "success"
    assert called(calls, "type_text")


def test_check_is_a_no_op_when_already_in_the_wanted_state(editor, calls):
    target = ElementTarget(app="TextEdit", selector="check_box")
    result = run(ActAction(type="act", target=target, verb="uncheck"))
    assert "already off" in text_of(result)
    assert not called(calls, "toggle")

    run(ActAction(type="act", target=target, verb="check"))
    assert called(calls, "toggle")


def test_check_on_something_that_has_no_checked_state_explains_itself(editor):
    result = run(ActAction(type="act", target=ElementTarget(app="TextEdit", selector="text_area"), verb="check"))
    assert result["status"] == "error"
    assert "not a checkbox" in text_of(result)


def test_repeat_applies_to_increment(editor, calls):
    run(ActAction(type="act", target=ElementTarget(app="TextEdit", selector="text_area"), verb="increment", repeat=3))
    assert len(called(calls, "increment")) == 3


def test_raw_verb_passes_a_platform_action_through(editor, calls):
    run(
        ActAction(
            type="act",
            target=ElementTarget(app="TextEdit", selector="text_area"),
            verb="raw",
            action_name="AXShowAlternateUI",
        )
    )
    assert called(calls, "perform_action")[0][1][1] == "AXShowAlternateUI"


def test_set_number_forwards_the_value(editor, calls):
    run(
        ActAction(
            type="act", target=ElementTarget(app="TextEdit", selector="text_area"), verb="set_number", number=0.75
        )
    )
    assert called(calls, "set_numeric_value")[0][1][1] == 0.75


def test_select_text_forwards_both_offsets(editor, calls):
    run(
        ActAction(
            type="act",
            target=ElementTarget(app="TextEdit", selector="text_area"),
            verb="select_text",
            start=2,
            end=7,
        )
    )
    assert called(calls, "select_text")[0][1][1:] == (2, 7)


@pytest.mark.parametrize("verb", ["toggle", "expand", "collapse", "select", "blur", "show_menu"])
def test_the_plain_verbs_go_straight_through(editor, calls, verb):
    result = run(ActAction(type="act", target=ElementTarget(app="TextEdit", selector="text_area"), verb=verb))
    assert result["status"] == "success"
    assert called(calls, verb)


def test_scroll_into_view_admits_it_did_nothing_on_macos(editor, monkeypatch):
    monkeypatch.setattr(_actions.platform, "system", lambda: "Darwin")
    result = text_of(
        run(ActAction(type="act", target=ElementTarget(app="TextEdit", selector="text_area"), verb="scroll_into_view"))
    )
    assert "no-op" in result
    assert "use 'scroll' instead" in result


def test_scroll_into_view_says_nothing_extra_where_it_works(editor, monkeypatch):
    monkeypatch.setattr(_actions.platform, "system", lambda: "Linux")
    result = text_of(
        run(ActAction(type="act", target=ElementTarget(app="TextEdit", selector="text_area"), verb="scroll_into_view"))
    )
    assert "no-op" not in result


def test_check_toggles_a_mixed_checkbox_towards_the_state_asked_for(editor, calls):
    editor.as_element().children()[0]._children[1]._children[1].checked = "mixed"
    result = run(ActAction(type="act", target=ElementTarget(app="TextEdit", selector="check_box"), verb="check"))
    assert "from mixed to on" in text_of(result)
    assert called(calls, "toggle")


def test_a_toggle_that_lands_somewhere_else_reports_where_it_landed(editor, calls):
    """`toggle` flips; it does not set.

    From a tri-state control it can land on the value that was not asked for,
    and which one depends on the platform and the widget. Reporting the wanted
    state as fact would state a result that was never observed — the whole
    reason the outcome is re-read instead of assumed.
    """
    editor.as_element().children()[0]._children[1]._children[1].checked = "mixed"
    result = run(ActAction(type="act", target=ElementTarget(app="TextEdit", selector="check_box"), verb="uncheck"))
    body = text_of(result)

    assert "it is now on, not off" in body, body
    assert "read it again" in body, body
    assert called(calls, "toggle")


def test_unsupported_action_returns_guidance_not_a_traceback(editor):
    editor.as_element().children()[0]._children[2]._unsupported = ("expand",)
    result = run(ActAction(type="act", target=ElementTarget(app="TextEdit", selector="text_area"), verb="expand"))
    assert result["status"] == "error"
    body = text_of(result)
    # The verb-on-element branch of the guidance: read the actions list.
    assert "does not expose that verb" in body
    # And the branch that matters for Error::Unsupported, which xa11y maps onto
    # this same exception class — telling a model to fall back to input there
    # would send it at the mechanism that just reported itself unavailable.
    assert "Unsupported" in body


# ── Refs ─────────────────────────────────────────────────────────────────────


def test_a_ref_resolves_through_stable_id(editor, calls):
    ref = ref_for(editor, r"text_field")
    run(TypeAction(type="type", target=ElementTarget(ref=ref), text="x"))
    assert called(calls, "type_text")


def test_a_ref_without_stable_id_resolves_through_its_structural_path(editor, calls):
    """The second 'Bold' button is only addressable by position, which is what paths are for."""
    snapshot = text_of(run(SnapshotAction(type="snapshot", app="TextEdit")))
    bold_refs = [line.strip().split(" ", 1)[0] for line in snapshot.splitlines() if 'button "Bold"' in line]
    assert len(bold_refs) == 2

    from strands_xa11y._refs import REFS

    assert REFS.get(bold_refs[1]).path.endswith("button[name='Bold']:nth(2)")
    run(ClickAction(type="click", target=PointerTarget(ref=bold_refs[1])))
    pressed = called(calls, "press")[0][1][0]
    assert pressed is editor.as_element().children()[0]._children[0]._children[2]


def test_a_ref_to_a_vanished_element_fails_loudly(editor):
    ref = ref_for(editor, r"text_field")
    window = editor.as_element().children()[0]
    window._children[1]._children = []  # the field is gone; its handle is stale
    from strands_xa11y._refs import REFS

    REFS.get(ref).element = None

    result = run(TypeAction(type="type", target=ElementTarget(ref=ref), text="x"))
    assert result["status"] == "error"
    assert "fresh snapshot" in text_of(result)


def test_an_unknown_ref_is_rejected_before_anything_happens(editor, calls):
    result = run(ClickAction(type="click", target=PointerTarget(ref="e9999")))
    assert result["status"] == "error"
    assert "Unknown ref" in text_of(result)
    assert calls == []


# ── Synthesised input ────────────────────────────────────────────────────────


def test_key_aliases_are_normalised(editor, calls):
    run(KeyAction(type="key", keys=["esc"], hold=["cmd", "shift"]))
    _, args, _ = called(calls, "input.chord")[0]
    assert args[0] == "Escape"
    assert args[1] == ["Meta", "Shift"]


def test_key_without_modifiers_taps(editor, calls):
    run(KeyAction(type="key", keys=["a", "Enter"], repeat=2))
    assert len(called(calls, "input.press")) == 4


def test_scroll_passes_deltas_through(editor, calls):
    run(ScrollAction(type="scroll", target=PointerTarget(point=(5, 5)), dy=-3))
    _, args, _ = called(calls, "input.scroll")[0]
    assert args[1:] == (0, -3)


# ── Screenshots ──────────────────────────────────────────────────────────────


def test_screenshot_withholds_the_image_by_default(editor):
    result = run(ScreenshotAction(type="screenshot"))
    assert len(result["content"]) == 1
    assert "withheld" in text_of(result)


def test_screenshot_returns_an_image_when_asked(editor):
    result = run(ScreenshotAction(type="screenshot", send_image=True))
    assert result["content"][1]["image"]["format"] == "png"


def test_oversized_screenshots_are_dropped_with_the_reason(editor):
    fake_xa11y.NEXT_SCREENSHOT = fake_xa11y.Screenshot(payload=b"x" * (6 * 1024 * 1024))
    try:
        result = run(ScreenshotAction(type="screenshot", send_image=True))
    finally:
        fake_xa11y.NEXT_SCREENSHOT = None
    assert len(result["content"]) == 1
    assert "exceeds the 5MB limit" in text_of(result)


def test_screenshot_can_be_saved_to_disk(editor, tmp_path):
    path = tmp_path / "shot.png"
    run(ScreenshotAction(type="screenshot", save_path=str(path)))
    assert path.read_bytes().startswith(b"\x89PNG")


def test_screenshot_of_an_element_captures_that_element(editor, calls):
    result = run(ScreenshotAction(type="screenshot", target=ElementTarget(app="TextEdit", selector="text_area")))
    assert result["status"] == "success"
    _, args, _ = called(calls, "screenshot")[0]
    assert args[0] is editor.as_element().children()[0]._children[2]
    assert "selector 'text_area'" in text_of(result)


def test_screenshot_of_a_region_passes_the_rectangle_through(editor, calls):
    result = run(ScreenshotAction(type="screenshot", region=(0, 0, 64, 32)))
    _, args, _ = called(calls, "screenshot")[0]
    assert args[1] == (0, 0, 64, 32)
    assert "region (0, 0, 64, 32)" in text_of(result)


def test_screenshot_reports_the_dimensions_it_captured(editor):
    assert "100x50 physical px at scale 1.0" in text_of(run(ScreenshotAction(type="screenshot")))


# ── App resolution ───────────────────────────────────────────────────────────


def test_partial_app_names_resolve(editor):
    assert run(FindAction(type="find", app="textedit", selector="button"))["status"] == "success"
    assert run(FindAction(type="find", app="Text", selector="button"))["status"] == "success"


def test_ambiguous_app_names_ask_for_a_precise_one(editor):
    fake_xa11y.APPS.append(fake_xa11y.App("TextEditor Pro", pid=99))
    result = run(FindAction(type="find", app="Text", selector="button"))
    assert result["status"] == "error"
    assert "matches several running applications" in text_of(result)


def test_pid_selector(editor):
    assert run(FindAction(type="find", app="pid:4242", selector="button"))["status"] == "success"


def test_unknown_app_surfaces_the_backend_diagnosis(editor):
    result = run(FindAction(type="find", app="Nonexistent", selector="button"))
    assert result["status"] == "error"
    assert "TextEdit" in text_of(result)  # xa11y's candidate list


@pytest.mark.parametrize("spec", [None, "foreground"])
def test_foreground_is_the_default_app(editor, spec):
    assert run(FindAction(type="find", app=spec, selector="button"))["status"] == "success"
