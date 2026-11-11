"""`type_text` reaches a control that reads key events (macOS).

The Cocoa test app's "Key Events" view keeps the first character of each
key event, as Calculator does. A string posted as one Unicode key event
typed only its first character there: "7*6=" arrived as "7". After a
function key, the Fn flag stayed on the next keys, and "ab" became Fn+A
and Fn+B, which the view never got.
"""

from __future__ import annotations

import sys
import time

import pytest

import xa11y

SETTLE_TIMEOUT = 3.0


def wait_for_keys(view, expected):
    deadline = time.monotonic() + SETTLE_TIMEOUT
    value = ""
    while time.monotonic() < deadline:
        value = view.element().value or ""
        if value == expected:
            break
        time.sleep(0.05)
    return value


@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
def test_cocoa_type_text_sends_each_character_as_a_key_event(app_name, app):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the key-event fixture belongs to the macOS Cocoa cell")

    view = app.locator('[name="Key Events"]')
    view.focus()
    view.wait_focused(timeout=SETTLE_TIMEOUT)
    xa11y.input_sim().type_text("7*6=é😀")

    value = wait_for_keys(view, "7*6=é😀")
    assert value == "7*6=é😀", (
        f"Key Events kept {value!r}, not '7*6=é😀'. Only the first character "
        f"means the text arrived as one key event."
    )


@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
@pytest.mark.parametrize("key", ["Home", "F5"])
def test_cocoa_keys_after_a_function_key_have_no_fn_flag(app_name, app, key):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the key-event fixture belongs to the macOS Cocoa cell")

    view = app.locator('[name="Key Events"]')
    view.focus()
    view.wait_focused(timeout=SETTLE_TIMEOUT)
    xa11y.input_sim().press(key)
    # The window server copies the key's flags into the session state a
    # moment after the post. Text sent at once still finds the old state.
    time.sleep(0.5)
    view.focus()
    xa11y.input_sim().type_text("ab")

    value = wait_for_keys(view, "ab")
    assert value == "ab", f"Key Events kept {value!r} after {key}, not 'ab'."


@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
def test_cocoa_key_after_a_chord_has_no_modifier(app_name, app):
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the key-event fixture belongs to the macOS Cocoa cell")

    view = app.locator('[name="Key Events"]')
    view.focus()
    view.wait_focused(timeout=SETTLE_TIMEOUT)
    xa11y.input_sim().chord("r", ["Meta", "Ctrl"])
    time.sleep(0.5)
    view.focus()
    xa11y.input_sim().press("a")

    value = wait_for_keys(view, "a")
    assert value == "a", f"Key Events kept {value!r} after Cmd+Ctrl+R, not 'a'."


CHORD_ROUNDS = 50


@pytest.mark.xa11y_requires("input_sim")
@pytest.mark.xa11y_frontmost
def test_cocoa_key_right_after_a_cmd_chord_has_no_cmd(app_name, app):
    """A key pressed right after a Cmd chord arrives without Cmd (#681).

    #681 measured the next key lost or arriving as Cmd+key about one time
    in twenty, with no wait between the two calls. The view marks a
    key that arrives with Command held with "⌘". Whether Cmd+U itself
    reaches the view is up to the menu, so only the "i" keys are judged: all
    of them must arrive, none with Cmd.
    """
    if sys.platform != "darwin" or app_name != "cocoa":
        pytest.skip("the key-event fixture belongs to the macOS Cocoa cell")

    view = app.locator('[name="Key Events"]')
    view.focus()
    view.wait_focused(timeout=SETTLE_TIMEOUT)
    sim = xa11y.input_sim()
    for _ in range(CHORD_ROUNDS):
        sim.chord("u", ["Meta"])
        sim.press("i")

    expected = "i" * CHORD_ROUNDS
    deadline = time.monotonic() + SETTLE_TIMEOUT
    value = keys = ""
    while time.monotonic() < deadline:
        value = view.element().value or ""
        keys = value.replace("⌘u", "")
        if keys == expected:
            break
        time.sleep(0.05)
    assert keys == expected, (
        f"Key Events kept {value!r} after {CHORD_ROUNDS} rounds of Cmd+U then i. "
        f"{keys.count('⌘i')} 'i' arrived with Cmd, and "
        f"{CHORD_ROUNDS - keys.count('i')} were lost."
    )
