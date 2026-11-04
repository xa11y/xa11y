"""`type_text` line breaks against native multiline text controls.

A line break means the Enter key on every platform (issue #498), so
``"\\r\\n"``, ``"\\n"`` and ``"\\r"`` must each type exactly one line break.

The Tauri input page (``test_input_sim.py``) covers typing in general, but
its fields are single-line, and WebView2 is not where the bug lived.

- **Windows.** A Win32/WinForms edit control turns a ``WM_CHAR`` CR *and* a
  ``WM_CHAR`` LF into a line break, so ``"\\r\\n"`` sent one UTF-16 unit at a
  time typed a blank line between the two halves. WPF's TextBox drops Unicode
  CR/LF taps entirely and breaks the line only on the Enter key. These two
  frameworks are the two failure shapes, and the WinForms and WPF test apps
  each have a multiline "Notes" TextBox.
- **macOS.** Line breaks were posted as Unicode text, so a Return-driven
  control never saw the key. The Cocoa test app's "Notes" is a real multiline
  NSTextView, where Return inserts one ``"\\n"``. Keystrokes go to the active
  app, and the harness runs the Cocoa app in the background, so that test
  claims the frontmost slot first.

Each test types into the box, reads the value back through the accessibility
API, and counts lines. It restores the original Notes text afterwards so the
shared app is left as the next suite expects.
"""

from __future__ import annotations

import os
import sys
import time

import pytest

import xa11y

APP = os.environ.get("XA11Y_TEST_APP", "tauri")

pytestmark = [pytest.mark.xa11y_requires("input_sim")]

LINE_BREAKS = pytest.mark.parametrize(
    "typed",
    ["line1\r\nline2", "line1\nline2", "line1\rline2"],
    ids=["crlf", "lf", "cr"],
)

# UIA has no multiline edit control type, so the multiline TextBox is a
# `text_field` like the single-line Search box; the name tells them apart.
WINDOWS_NOTES = 'text_field[name="Notes"]'
WINDOWS_ORIGINAL_NOTES = {
    "winforms": "Line 1\r\nLine 2\r\nLine 3",
    "wpf": "Line 1\nLine 2\nLine 3",
}
COCOA_NOTES = 'text_area[name="Notes"]'
SETTLE_TIMEOUT = 3.0


def _lines(value: str) -> list[str]:
    """Split on any line break the control stores (CRLF, LF, or CR)."""
    return value.replace("\r\n", "\n").replace("\r", "\n").split("\n")


def _wait_for_lines(app: xa11y.App, selector: str, expected: list[str]) -> None:
    deadline = time.monotonic() + SETTLE_TIMEOUT
    value = ""
    while time.monotonic() < deadline:
        value = app.locator(selector).element().value or ""
        if _lines(value) == expected:
            return
        time.sleep(0.05)
    pytest.fail(
        f"Notes never held lines {expected!r} within {SETTLE_TIMEOUT:.1f}s; "
        f"last value was {value!r} (lines {_lines(value)!r}). An empty line "
        f"between the halves is the CR and the LF each typing a break; one line "
        f"means the break never arrived as Enter."
    )


def _focus_windows_notes(app: xa11y.App) -> None:
    """Move keyboard focus into Notes; keystrokes go to the focus holder."""
    locator = app.locator(WINDOWS_NOTES)
    locator.focus()
    locator.wait_focused(timeout=SETTLE_TIMEOUT)
    rect = locator.element().bounds
    assert rect is not None, "Notes has no bounds to click for keyboard focus"
    xa11y.input_sim().click((rect.x + rect.width // 2, rect.y + rect.height // 2))


@pytest.mark.skipif(
    sys.platform != "win32" or APP not in ("winforms", "wpf"),
    reason="needs a native Windows multiline edit control (winforms / wpf test apps)",
)
@LINE_BREAKS
def test_type_text_line_break_is_one_break(app, typed):
    notes = app.locator(WINDOWS_NOTES)
    notes.set_value("")
    try:
        _wait_for_lines(app, WINDOWS_NOTES, [""])
        _focus_windows_notes(app)
        xa11y.input_sim().type_text(typed)
        _wait_for_lines(app, WINDOWS_NOTES, ["line1", "line2"])
    finally:
        notes.set_value(WINDOWS_ORIGINAL_NOTES[APP])


@pytest.mark.skipif(
    sys.platform != "win32" or APP not in ("winforms", "wpf"),
    reason="needs a native Windows multiline edit control (winforms / wpf test apps)",
)
@pytest.mark.parametrize("shape", ["plain", "newline", "unicode", "composed"])
def test_type_text_preserves_each_character_repeatedly(app, shape):
    """Queued Unicode packets used to substitute the final character (#620).

    The defect appeared about once in twenty calls on WPF, including calls
    without newlines. Check repeated full values, including characters that
    cannot be represented as ordinary virtual keys and UTF-16 surrogate pairs.
    """
    notes = app.locator(WINDOWS_NOTES)
    sim = xa11y.input_sim()
    try:
        _focus_windows_notes(app)
        for attempt in range(60):
            suffix = f"nopqrstuvwxyz0123456789-{attempt}"
            prefix = "abcdefghijklm"
            if shape == "newline":
                prefix += "\r\n"
            elif shape in ("unicode", "composed"):
                prefix += "hé😀漢字"
                if shape == "composed":
                    prefix += "\n"
            typed = prefix + suffix
            notes.set_value("")
            if shape == "composed":
                # Consecutive one-scalar calls must preserve the same text,
                # including the emoji and the real Enter between lines.
                for character in typed:
                    sim.type_text(character)
            else:
                sim.type_text(typed)
            _wait_for_lines(app, WINDOWS_NOTES, _lines(typed))
    finally:
        notes.set_value(WINDOWS_ORIGINAL_NOTES[APP])


@pytest.mark.skipif(
    sys.platform != "darwin" or APP != "cocoa",
    reason="needs the Cocoa test app's multiline NSTextView",
)
@pytest.mark.xa11y_frontmost
@LINE_BREAKS
def test_cocoa_type_text_line_break_is_one_break(app, typed):
    notes = app.locator(COCOA_NOTES)
    original = notes.element().value or ""
    notes.set_value("")
    try:
        _wait_for_lines(app, COCOA_NOTES, [""])
        notes.focus()
        notes.wait_focused(timeout=SETTLE_TIMEOUT)
        xa11y.input_sim().type_text(typed)
        _wait_for_lines(app, COCOA_NOTES, ["line1", "line2"])
    finally:
        notes.set_value(original)
