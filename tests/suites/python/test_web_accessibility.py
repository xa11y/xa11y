"""App.enable_web_accessibility against a real Electron app (issue #511).

On macOS the method asks the app to build its web content's accessibility
tree. The suite's own Electron instance is launched with that tree already on,
so this test launches a second one with it off, the way a user's Electron app
starts, and checks that the page's button is unreachable before the call and
reachable after it.

Linux and Windows have no such request: Chromium's tree there is a launch-time
flag (Linux) or built on demand for any UI Automation client (Windows). The
method says so with a clear refusal, and that refusal is what those cells
check.
"""

from __future__ import annotations

import subprocess
import sys
import time
from pathlib import Path

import pytest

import xa11y

PROJECT_ROOT = Path(__file__).resolve().parents[3]
ELECTRON_DIR = PROJECT_ROOT / "test-apps" / "electron"
OK_BUTTON = 'button[name="OK"]'


def _electron_binary() -> str:
    """The Electron executable itself, not the `.bin/electron` node shim.

    The shim spawns Electron as a child, so its pid is not the app's.
    `require("electron")` from the app's directory returns the real path.
    """
    out = subprocess.run(
        ["node", "-e", 'process.stdout.write(require("electron"))'],
        cwd=ELECTRON_DIR,
        capture_output=True,
        check=True,
        text=True,
        timeout=30,
    )
    return out.stdout.strip()


@pytest.fixture
def electron_without_web_accessibility():
    """A second Electron test app, started with web accessibility off."""
    proc = subprocess.Popen(
        [
            _electron_binary(),
            str(ELECTRON_DIR / "main.js"),
            "--xa11y-web-accessibility=off",
            "--xa11y-app-name=xa11y-electron-web-a11y-off",
        ],
        cwd=ELECTRON_DIR,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        yield xa11y.App.by_pid(proc.pid, timeout=30)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=10)


@pytest.fixture
def electron_only(app_name):
    if app_name != "electron":
        pytest.skip("enable_web_accessibility is exercised against the Electron app")


@pytest.mark.skipif(sys.platform != "darwin", reason="the request exists only on macOS")
def test_enable_web_accessibility_makes_web_content_reachable(
    electron_only, electron_without_web_accessibility
):
    app = electron_without_web_accessibility
    # Before the request the page's button is not in the tree, and the miss
    # says why and what to call.
    with pytest.raises(xa11y.SelectorNotMatchedError) as miss:
        app.locator(OK_BUTTON).element()
    assert "enable_web_accessibility" in (miss.value.last_observed or ""), (
        f"the miss should suggest enable_web_accessibility; "
        f"last_observed={miss.value.last_observed!r}; scope:\n{miss.value.scope}"
    )

    app.enable_web_accessibility(timeout=30)

    assert app.locator(OK_BUTTON).element().name == "OK"


@pytest.mark.skipif(sys.platform != "darwin", reason="the request exists only on macOS")
def test_enable_web_accessibility_is_idempotent(electron_only, app):
    # The suite's app was launched with the tree already on: the request is
    # still taken, and its content is already there.
    app.enable_web_accessibility(timeout=10)
    app.enable_web_accessibility(timeout=10)
    assert app.locator(OK_BUTTON).element().name == "OK"


@pytest.mark.skipif(sys.platform == "darwin", reason="macOS takes the request")
def test_enable_web_accessibility_is_refused_with_guidance(electron_only, app):
    with pytest.raises(xa11y.ActionNotSupportedError) as refused:
        app.enable_web_accessibility(timeout=0)
    message = str(refused.value)
    if sys.platform.startswith("linux"):
        assert "--force-renderer-accessibility" in message, message
        assert "ACCESSIBILITY_ENABLED=1" in message, message
    else:
        assert "UI Automation" in message, message


# TEMPORARY CI diagnostic (PR #619); removed before merge.
@pytest.mark.skipif(sys.platform != "darwin", reason="macOS only")
def test_diag_first_access(electron_only, electron_without_web_accessibility):
    app = electron_without_web_accessibility
    log = []
    t0 = time.monotonic()
    for _ in range(40):
        try:
            n_web = len(app.locator("web_area").elements())
            n_kids = len(app.locator("web_area *").elements())
            n_btn = len(app.locator('button[name="OK"]').elements())
            log.append(f"t={time.monotonic()-t0:.2f}s web_area={n_web} descendants={n_kids} ok_buttons={n_btn}")
        except Exception as e:  # noqa: BLE001
            log.append(f"t={time.monotonic()-t0:.2f}s error {type(e).__name__}: {str(e)[:150]}")
        time.sleep(0.1)
    cells = app.locator('table[name="Users Table"] table_cell').elements()
    dump = [f"cells={len(cells)}"]
    for c in cells[:2]:
        dump.append(f"cell raw={c.raw} name={c.name!r} value={c.value!r}")
    for st in app.locator('table[name="Users Table"] static_text').elements()[:3]:
        dump.append(f"static_text raw={st.raw} name={st.name!r} value={st.value!r}")
    pytest.fail("DIAG\n" + "\n".join(log) + "\n" + "\n".join(dump))
