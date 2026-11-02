"""UWP smoke tests: the ApplicationFrameHost-hosted app is found and readable.

Runs on the ``uwp`` cell only (``XA11Y_TEST_APP=uwp``). A UWP app's only
top-level window is an ``ApplicationFrameWindow`` owned by
ApplicationFrameHost.exe; the app's own process owns just the
``Windows.UI.Core.CoreWindow`` parented inside it. Issue #497 is that xa11y
attributes the frame — and so the whole app — to ApplicationFrameHost.

``XA11Y_UWP_APP_PID`` is the app's own process (the PID activation returned,
see test-apps/uwp/launch.ps1). ``XA11Y_TEST_APP_PID`` is the pid xa11y
reported when the harness found the app, which until #497 is fixed is
ApplicationFrameHost's (see ``_suite_app_pid`` in tests/harness/launch.py).
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

    Asserts what holds today, before #497: whichever process the frame is
    attributed to, its tree crosses into the CoreWindow's XAML content.
    """
    window = app.locator(f'window[name*="{TITLE}"]').element()
    assert window.name and TITLE in window.name

    ok = app.locator('button[name="OK"]').element()
    assert ok.visible, f"OK button is not visible: {ok}"

    search = app.locator('text_field[name="Search"]').element()
    assert search.value == "hello world"


# Both #497 tests are strict: the fix makes them pass, which fails the run
# until the markers come off, so the fix cannot land without them.
# `raises=` keeps the xfails from absorbing an unrelated failure (a crashed
# app, a platform error): only the lookup miss or the wrong-pid assertion that
# #497 causes counts as the expected failure.
@pytest.mark.xfail(
    strict=True,
    raises=xa11y.SelectorNotMatchedError,
    reason=(
        "#497: the frame is attributed to ApplicationFrameHost.exe, so the UWP "
        "process owns no top-level window xa11y reports"
    ),
)
def test_app_is_found_by_its_own_pid():
    """App.by_pid on the UWP process resolves to the frame and its content."""
    pid = _app_pid()
    found = xa11y.App.by_pid(pid, timeout=5.0)
    assert found.locator(f'window[name*="{TITLE}"]').element()
    found.locator('button[name="OK"]').element()


@pytest.mark.xfail(
    strict=True,
    raises=(AssertionError, xa11y.SelectorNotMatchedError),
    reason=(
        "#497: xa11y reports the UWP app as 'ApplicationFrameHost' rather than "
        "by its package display name"
    ),
)
def test_app_is_found_by_its_package_display_name():
    """App.by_name with the package DisplayName resolves to the UWP process.

    Before #497 the frame is reported as "ApplicationFrameHost", so the
    lookup misses; the pid check guards against it resolving to the frame
    host by any other route.
    """
    pid = _app_pid()
    found = xa11y.App.by_name(TITLE, timeout=5.0)
    assert found.pid == pid, f"{TITLE!r} resolved to pid {found.pid}, not the UWP process {pid}"
    assert found.locator(f'window[name*="{TITLE}"]').element()
