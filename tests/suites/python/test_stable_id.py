"""Which identifier a Windows element reports as ``stable_id``.

An element with a native window of its own (a top-level window, or a
WinForms control) is identified by that window handle, ``hwnd:0x…``. A
windowless element falls back to its AutomationId. The AutomationId is
always in ``raw["automation_id"]`` when the element has one, so a caller
that wants the identifier the app chose (a WinForms control's ``Name``) can
read it there, and ``[stable_id=…]`` keeps meaning the handle.
"""

from __future__ import annotations

import os
import sys

import pytest

APP = os.environ.get("XA11Y_TEST_APP", "tauri")

pytestmark = pytest.mark.skipif(
    sys.platform != "win32" or APP != "winforms",
    reason="needs WinForms controls, which have both a window handle and an AutomationId",
)

AGREE = 'check_box[name="Agree to terms"]'


def test_control_with_window_uses_its_handle_as_stable_id(app):
    agree = app.locator(AGREE).element()
    handle = agree.raw.get("native_window_handle")
    assert handle is not None, (
        f"the check box has no window of its own, so it cannot show the rule: {agree.raw}"
    )
    assert agree.stable_id == f"hwnd:{handle}", (
        f"stable_id is {agree.stable_id!r}, not the window handle 'hwnd:{handle}'"
    )


def test_automation_id_is_in_raw(app):
    agree = app.locator(AGREE).element()
    assert agree.raw.get("automation_id") == "agreeCheckBox", agree.raw


def test_stable_id_finds_the_control_by_handle(app):
    agree = app.locator(AGREE).element()
    found = app.locator(f'[stable_id="{agree.stable_id}"]').element()
    assert found.name == "Agree to terms"


def test_automation_id_is_not_the_stable_id(app):
    assert app.locator('[stable_id="agreeCheckBox"]').count() == 0


def test_top_level_window_keeps_its_window_handle(app):
    windows = [w for w in app.windows() if w.raw.get("automation_id") == "TestForm"]
    assert len(windows) == 1, [(w.name, w.raw) for w in app.windows()]
    window = windows[0]
    assert window.stable_id is not None and window.stable_id.startswith("hwnd:"), (
        f"a top-level window's stable_id is {window.stable_id!r}, not its window handle"
    )
    assert window.raw.get("native_window_handle") == window.stable_id.removeprefix("hwnd:")
