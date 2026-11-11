"""Which identifier a Windows element reports as ``stable_id`` (#684).

A WinForms control has a window of its own and an AutomationId (its
``Name``). The window handle changes each time the app starts, so the
AutomationId is the control's ``stable_id``. A top-level window keeps its
handle even when it has an AutomationId: every instance of a form reports
the same ``Name``, and two open windows would otherwise share an identity.
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


def test_control_with_automation_id_uses_it_as_stable_id(app):
    agree = app.locator(AGREE).element()
    assert "native_window_handle" in agree.raw, (
        f"the check box has no window of its own, so it cannot show the bug: {agree.raw}"
    )
    assert agree.raw.get("automation_id") == "agreeCheckBox", agree.raw
    assert agree.stable_id == "agreeCheckBox", (
        f"stable_id is {agree.stable_id!r}, not the AutomationId 'agreeCheckBox'"
    )


def test_stable_id_finds_the_control(app):
    found = app.locator('[stable_id="agreeCheckBox"]').element()
    assert found.name == "Agree to terms"


def test_top_level_window_keeps_its_window_handle(app):
    windows = [w for w in app.windows() if w.raw.get("automation_id") == "TestForm"]
    assert len(windows) == 1, [(w.name, w.raw) for w in app.windows()]
    window = windows[0]
    assert window.stable_id is not None and window.stable_id.startswith("hwnd:"), (
        f"a top-level window's stable_id is {window.stable_id!r}, not its window handle"
    )
    assert window.raw.get("native_window_handle") == window.stable_id.removeprefix("hwnd:")
