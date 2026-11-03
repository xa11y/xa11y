"""``xa11y enable-web-accessibility`` against the real Electron app (#511).

The suite's Electron is launched with its web tree already on, so on macOS
the request is taken and the command succeeds. Linux and Windows have no such
request: the command exits 1 with the guidance the library's refusal carries.
The deep before/after check lives in tests/suites/python/test_web_accessibility.py.
"""

from __future__ import annotations

import sys

import pytest


@pytest.fixture
def electron_only(app_name):
    if app_name != "electron":
        pytest.skip("enable-web-accessibility is exercised against the Electron app")


@pytest.mark.skipif(sys.platform != "darwin", reason="the request exists only on macOS")
def test_enable_web_accessibility_succeeds_on_macos(electron_only, run_cli, app_pid):
    rc, stdout, stderr = run_cli("enable-web-accessibility", "--pid", str(app_pid))
    assert rc == 0, f"stdout={stdout!r} stderr={stderr!r}"
    assert "web accessibility enabled for" in stdout, stdout
    assert f"(pid={app_pid})" in stdout, stdout


@pytest.mark.skipif(sys.platform == "darwin", reason="macOS takes the request")
def test_enable_web_accessibility_is_refused_with_guidance(electron_only, run_cli, app_pid):
    rc, stdout, stderr = run_cli("enable-web-accessibility", "--pid", str(app_pid))
    assert rc == 1, f"an unsupported operation exits 1; stdout={stdout!r} stderr={stderr!r}"
    if sys.platform.startswith("linux"):
        assert "--force-renderer-accessibility" in stderr, stderr
        assert "ACCESSIBILITY_ENABLED=1" in stderr, stderr
    else:
        assert "UI Automation" in stderr, stderr


def test_enable_web_accessibility_rejects_a_positional_argument(run_cli, app_pid):
    rc, _, stderr = run_cli("enable-web-accessibility", "--pid", str(app_pid), "extra")
    assert rc == 2, stderr
    assert "unexpected argument: extra" in stderr, stderr
