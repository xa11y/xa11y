"""UWP frame attribution and XAML content against the activated app process.

The native ApplicationFrameHost frame belongs to the hosted UWP app in xa11y.
Both harness PID variables identify the app process returned by activation.
"""

from __future__ import annotations

import os

import pytest
import xa11y

pytestmark = pytest.mark.skipif(
    os.environ.get("XA11Y_TEST_APP") != "uwp",
    reason="UWP smoke tests target the uwp cell only",
)

# Identity/DisplayName in test-apps/uwp/AppxManifest.xml: the frame's title,
# and the package display name a packaged app is reported under.
TITLE = "xa11y UWP Test"


def _app_pid() -> int:
    pid = os.environ.get("XA11Y_UWP_APP_PID")
    if not pid:
        pytest.skip("needs the harness-launched app (XA11Y_UWP_APP_PID)")
    return int(pid)


def test_frame_window_and_xaml_content_are_readable(app):
    """The session app has the frame window and the XAML tree inside it.

    The app tree crosses from its attributed frame into CoreWindow content.
    """
    window = app.locator(f'window[name*="{TITLE}"]').element()
    assert window.name and TITLE in window.name

    ok = app.locator('button[name="OK"]').element()
    assert ok.visible, f"OK button is not visible: {ok}"

    search = app.locator('text_field[name="Search"]').element()
    assert search.value == "hello world"


def test_app_is_found_by_its_own_pid():
    """App.by_pid on the UWP process resolves to the frame and its content."""
    pid = _app_pid()
    found = xa11y.App.by_pid(pid, timeout=5.0)
    assert found.locator(f'window[name*="{TITLE}"]').element()
    found.locator('button[name="OK"]').element()


def test_app_is_found_by_its_package_display_name():
    """App.by_name with the package DisplayName resolves to the UWP process.

    The pid check guards against resolving to the frame host instead of
    the activated UWP process.
    """
    pid = _app_pid()
    found = xa11y.App.by_name(TITLE, timeout=5.0)
    assert found.pid == pid, f"{TITLE!r} resolved to pid {found.pid}, not the UWP process {pid}"
    assert found.locator(f'window[name*="{TITLE}"]').element()
