"""Failures are the tool's main teaching signal, so they get their own tests."""

from __future__ import annotations

import fake_xa11y
import pytest

from strands_xa11y._actions import run
from strands_xa11y._errors import describe
from strands_xa11y._session import require_consent
from strands_xa11y.models import (
    ClickAction,
    ElementTarget,
    FindAction,
    MouseAction,
    PointerTarget,
    ScreenshotAction,
)


def text_of(result) -> str:
    return result["content"][0]["text"]


def test_diagnosis_is_passed_through_verbatim():
    """xa11y's structured diagnosis is what lets the model fix its own selector."""
    message = describe(
        fake_xa11y.TimeoutError(
            "timed out",
            elapsed=5.0,
            condition="visible",
            selector="button[name='Sav']",
            last_observed="selector never matched",
            candidates=["button 'Save'", "button 'Save As…'"],
            scope="window 'Untitled'",
        )
    )
    assert "condition: visible" in message
    assert "selector: button[name='Sav']" in message
    assert "near misses: button 'Save'; button 'Save As…'" in message
    assert "search scope:" in message


def test_hidden_reason_says_why_a_matched_element_was_not_visible():
    """`scrolled_out` and `window_minimized` call for different fixes."""
    message = describe(
        fake_xa11y.TimeoutError(
            "timed out",
            condition="press target actionable (visible && enabled)",
            last_observed='matched button "Save" (visible=false, enabled=true, focused=false)',
            hidden_reason="scrolled_out",
        )
    )
    assert "hidden reason: scrolled_out" in message
    assert "hidden reason" not in describe(fake_xa11y.TimeoutError("timed out"))


def test_delivery_tells_the_model_whether_a_retry_is_safe():
    """`not_delivered` is safe to retry, `unknown` may double-submit."""
    safe = fake_xa11y.TimeoutError("timed out", condition="visible")
    safe.delivery = "not_delivered"
    assert "delivery: not_delivered" in describe(safe)
    assert "delivery: unknown" in describe(fake_xa11y.TimeoutError("timed out"))


def test_pointer_geometry_is_forwarded_without_mutating_the_error():
    error = fake_xa11y.SelectorNotMatchedError("point matches no display")
    geometry = {
        "point": {"x": 1000, "y": 1000},
        "element_bounds": {"x": 900, "y": 900, "width": 50, "height": 50},
        "anchor": {"offset": {"dx": 100, "dy": 100}},
        "checked_point": {"x": 2000, "y": 2000},
        "coordinate_space": "x11 physical root pixels",
        "displays": [{"x": -100, "y": 0, "width": 800, "height": 600}],
        "omitted_displays": 2,
    }
    error.pointer_target = geometry
    error.delivery = "not_delivered"
    message = describe(error)
    assert f"pointer target: {geometry}" in message
    assert "delivery: not_delivered" in message
    assert message.startswith("SelectorNotMatchedError: point matches no display")
    assert error.pointer_target is geometry
    assert str(error) == "point matches no display"
    assert "pointer target" not in describe(fake_xa11y.SelectorNotMatchedError("no match"))


def test_auto_scroll_says_whether_scrolling_was_already_tried():
    """A model told the scroll was refused should not ask for one."""
    message = describe(
        fake_xa11y.TimeoutError(
            "timed out",
            last_observed='matched button "Save" (visible=false, enabled=true, focused=false)',
            hidden_reason="scrolled_out",
            auto_scroll="refused",
        )
    )
    assert "auto scroll: refused" in message
    assert "auto scroll" not in describe(fake_xa11y.TimeoutError("timed out"))


@pytest.mark.parametrize(
    ("error", "expected"),
    [
        (fake_xa11y.PermissionDeniedError("nope"), "Privacy & Security"),
        (fake_xa11y.AccessibilityNotEnabledError("empty"), "--force-renderer-accessibility"),
        (fake_xa11y.ActionNotSupportedError("no"), "'actions' list"),
        (fake_xa11y.InvalidSelectorError("bad"), "snake_case"),
        (fake_xa11y.InvalidActionDataError("range"), "out-of-range"),
        (fake_xa11y.PlatformError("busy"), "modal"),
        (fake_xa11y.SelectorNotMatchedError("no match"), "near misses above"),
        (fake_xa11y.TimeoutError("timed out"), "last observed"),
    ],
)
def test_each_failure_mode_carries_its_fix(error, expected):
    assert expected in describe(error)


def test_an_unknown_failure_is_still_named_rather_than_swallowed():
    message = describe(RuntimeError("something odd"))
    assert message == "RuntimeError: something odd"


def test_a_message_ending_in_a_colon_keeps_it():
    """The message is xa11y's; trimming it would drop real content off the end."""
    assert describe(RuntimeError("AXError -25204:")) == "RuntimeError: AXError -25204:"


def test_an_exception_with_no_message_is_just_its_name():
    assert describe(fake_xa11y.PlatformError()).splitlines()[0] == "PlatformError"


def test_guidance_is_inherited_from_the_nearest_matching_base_class():
    class VendorSpecificPermissionDeniedError(fake_xa11y.PermissionDeniedError):
        pass

    assert "Privacy & Security" in describe(VendorSpecificPermissionDeniedError("nope"))


def test_a_diagnosis_with_no_fields_set_adds_nothing_to_the_message():
    message = describe(fake_xa11y.SelectorNotMatchedError("no match"))
    assert "near misses:" not in message
    assert "search scope:" not in message


def test_run_converts_backend_errors_into_tool_results(editor):
    result = run(FindAction(type="find", app="TextEdit", selector="button[name=unquoted]"))
    assert result["status"] == "error"
    assert "InvalidSelectorError" in text_of(result)


def test_mutating_actions_need_consent(editor, monkeypatch):
    monkeypatch.setenv("BYPASS_TOOL_CONSENT", "false")
    monkeypatch.setattr("builtins.input", lambda _: "n")
    monkeypatch.setattr("strands_xa11y._session.sys.stdin", type("Stdin", (), {"isatty": lambda self: True})())

    result = run(ClickAction(type="click", target=PointerTarget(app="TextEdit", selector="button[name='Bold']")))
    assert result["status"] == "error"
    assert "declined" in text_of(result)


def test_reading_the_tree_never_asks_for_consent(editor, monkeypatch):
    monkeypatch.setenv("BYPASS_TOOL_CONSENT", "false")
    monkeypatch.setattr("builtins.input", lambda _: pytest.fail("read-only actions must not prompt"))

    assert run(FindAction(type="find", app="TextEdit", selector="button"))["status"] == "success"


def test_sending_pixels_to_the_model_does_ask(editor, monkeypatch):
    """Withholding the image is free; shipping it is a privacy decision."""
    monkeypatch.setenv("BYPASS_TOOL_CONSENT", "false")
    monkeypatch.setattr("builtins.input", lambda _: "n")
    monkeypatch.setattr("strands_xa11y._session.sys.stdin", type("Stdin", (), {"isatty": lambda self: True})())

    assert run(ScreenshotAction(type="screenshot"))["status"] == "success"
    assert run(ScreenshotAction(type="screenshot", send_image=True))["status"] == "error"


def test_writing_pixels_to_disk_also_asks(editor, monkeypatch, tmp_path):
    """save_path puts whatever is on screen into a file; that leaves the process too."""
    monkeypatch.setenv("BYPASS_TOOL_CONSENT", "false")
    monkeypatch.setattr("builtins.input", lambda _: "n")
    monkeypatch.setattr("strands_xa11y._session.sys.stdin", type("Stdin", (), {"isatty": lambda self: True})())

    path = tmp_path / "shot.png"
    assert run(ScreenshotAction(type="screenshot", save_path=str(path)))["status"] == "error"
    assert not path.exists()


def test_consent_granted_lets_the_action_through(editor, monkeypatch):
    monkeypatch.setenv("BYPASS_TOOL_CONSENT", "false")
    monkeypatch.setattr("builtins.input", lambda _: "Y")
    monkeypatch.setattr("strands_xa11y._session.sys.stdin", type("Stdin", (), {"isatty": lambda self: True})())

    result = run(ClickAction(type="click", target=PointerTarget(app="TextEdit", selector="button[name='Bold']")))
    assert result["status"] == "success"


@pytest.mark.parametrize("setting", ["TRUE", "True", "true"])
def test_the_bypass_is_case_insensitive(editor, monkeypatch, setting):
    monkeypatch.setenv("BYPASS_TOOL_CONSENT", setting)
    monkeypatch.setattr("builtins.input", lambda _: pytest.fail("the bypass should have skipped the prompt"))
    assert (
        run(ClickAction(type="click", target=PointerTarget(app="TextEdit", selector="button[name='Bold']")))["status"]
        == "success"
    )


def test_the_consent_summary_leaves_out_fields_the_caller_never_set(editor, monkeypatch):
    """The prompt is for a human to read, so it shows the request, not the whole schema."""
    monkeypatch.setenv("BYPASS_TOOL_CONSENT", "false")
    prompts = []
    monkeypatch.setattr("builtins.input", lambda prompt: prompts.append(prompt) or "y")
    monkeypatch.setattr("strands_xa11y._session.sys.stdin", type("Stdin", (), {"isatty": lambda self: True})())

    run(ClickAction(type="click", target=PointerTarget(app="TextEdit", selector="button[name='Bold']")))
    assert "button=" not in prompts[0]  # left at its default
    assert "count=" not in prompts[0]


def test_an_action_with_nothing_but_a_type_still_summarises(editor, monkeypatch):
    monkeypatch.setenv("BYPASS_TOOL_CONSENT", "false")
    prompts = []
    monkeypatch.setattr("builtins.input", lambda prompt: prompts.append(prompt) or "y")
    monkeypatch.setattr("strands_xa11y._session.sys.stdin", type("Stdin", (), {"isatty": lambda self: True})())

    run(MouseAction(type="mouse", op="down"))
    assert "mouse" in prompts[0]


def test_without_a_terminal_consent_is_refused_not_assumed(monkeypatch):
    monkeypatch.setenv("BYPASS_TOOL_CONSENT", "false")
    monkeypatch.setattr("strands_xa11y._session.sys.stdin", type("Stdin", (), {"isatty": lambda self: False})())

    with pytest.raises(Exception, match="BYPASS_TOOL_CONSENT"):
        require_consent("click(...)")


def test_consent_summary_names_the_target(editor, monkeypatch):
    monkeypatch.setenv("BYPASS_TOOL_CONSENT", "false")
    prompts = []
    monkeypatch.setattr("builtins.input", lambda prompt: prompts.append(prompt) or "y")
    monkeypatch.setattr("strands_xa11y._session.sys.stdin", type("Stdin", (), {"isatty": lambda self: True})())

    run(ClickAction(type="click", target=PointerTarget(app="TextEdit", selector="button[name='Bold']")))
    assert "click(" in prompts[0]
    assert "Bold" in prompts[0]


def test_missing_element_target_is_a_schema_error_not_a_crash():
    with pytest.raises(Exception, match="exactly one"):
        ElementTarget()
