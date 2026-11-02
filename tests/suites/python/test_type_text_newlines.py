"""`type_text` line breaks against a native multiline edit control on Windows.

The Tauri input page (``test_input_sim.py``) covers typing in general, but
its fields are single-line, and WebView2 is not where the bug in issue #498
lived. A Win32/WinForms edit control turns a ``WM_CHAR`` CR *and* a
``WM_CHAR`` LF into a line break, so ``"\\r\\n"`` sent one UTF-16 unit at a
time typed a blank line between the two halves. WPF's TextBox drops Unicode
CR/LF taps entirely and breaks the line only on the Enter key. type_text now
types every line break as Enter, and these two frameworks are the two failure
shapes. The WinForms and WPF test apps each have a multiline "Notes"
TextBox, which is the control this needs.

The test types into the box, reads the value back through UIA, and counts
lines. It restores the original Notes text afterwards so the shared app is
left as the next suite expects.
"""

from __future__ import annotations

import os
import sys
import time

import pytest

import xa11y

APP = os.environ.get("XA11Y_TEST_APP", "tauri")

pytestmark = [
    pytest.mark.skipif(
        sys.platform != "win32" or APP not in ("winforms", "wpf"),
        reason="needs a native Windows multiline edit control (winforms / wpf test apps)",
    ),
    pytest.mark.xa11y_requires("input_sim"),
]

# UIA has no multiline edit control type, so the multiline TextBox is a
# `text_field` like the single-line Search box; the name tells them apart.
NOTES = 'text_field[name="Notes"]'
ORIGINAL_NOTES = {
    "winforms": "Line 1\r\nLine 2\r\nLine 3",
    "wpf": "Line 1\nLine 2\nLine 3",
}
SETTLE_TIMEOUT = 3.0


def _lines(value: str) -> list[str]:
    """Split on any line break the control stores (CRLF, LF, or CR)."""
    return value.replace("\r\n", "\n").replace("\r", "\n").split("\n")


def _focus_for_typing(app: xa11y.App) -> None:
    """Move keyboard focus into Notes; keystrokes go to the focus holder."""
    locator = app.locator(NOTES)
    locator.focus()
    locator.wait_focused(timeout=SETTLE_TIMEOUT)
    rect = locator.element().bounds
    assert rect is not None, "Notes has no bounds to click for keyboard focus"
    xa11y.input_sim().click((rect.x + rect.width // 2, rect.y + rect.height // 2))


def _wait_for_lines(app: xa11y.App, expected: list[str]) -> None:
    deadline = time.monotonic() + SETTLE_TIMEOUT
    value = ""
    while time.monotonic() < deadline:
        value = app.locator(NOTES).element().value or ""
        if _lines(value) == expected:
            return
        time.sleep(0.05)
    pytest.fail(
        f"Notes never held lines {expected!r} within {SETTLE_TIMEOUT:.1f}s; "
        f"last value was {value!r} (lines {_lines(value)!r}). An empty line "
        f"between the halves is the CR and the LF each typing a break."
    )


@pytest.mark.parametrize(
    "typed",
    ["line1\r\nline2", "line1\nline2", "line1\rline2"],
    ids=["crlf", "lf", "cr"],
)
def test_type_text_line_break_is_one_break(app, typed):
    notes = app.locator(NOTES)
    notes.set_value("")
    try:
        _wait_for_lines(app, [""])
        _focus_for_typing(app)
        xa11y.input_sim().type_text(typed)
        _wait_for_lines(app, ["line1", "line2"])
    finally:
        notes.set_value(ORIGINAL_NOTES[APP])
