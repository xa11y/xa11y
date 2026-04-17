"""Fixtures for xa11y Electron integration tests.

The Electron test app exercises the Chromium accessibility bridge on Linux,
which has a quirk: unless launched with `--force-renderer-accessibility`, the
window's subtree is empty and xa11y must surface an error rather than silently
return zero results.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from tests.helpers import launch_test_app

import os
import tempfile

APP_DIR = Path(__file__).resolve().parent.parent.parent / "test-apps" / "electron"
ELECTRON_BIN = APP_DIR / "node_modules" / "electron" / "dist" / "electron"


def _electron_symlink(app_name: str) -> str:
    """Create a symlink named ``app_name`` pointing to the Electron binary.

    Chromium uses ``argv[0]``'s basename as the AT-SPI application name, so
    launching via a uniquely-named symlink lets each fixture's Electron
    instance be located via ``xa11y.App.by_name(...)`` without colliding with
    concurrent instances from other fixtures.
    """
    link_dir = Path(tempfile.gettempdir()) / "xa11y-electron-links"
    link_dir.mkdir(parents=True, exist_ok=True)
    link = link_dir / app_name
    if link.is_symlink() or link.exists():
        link.unlink()
    os.symlink(ELECTRON_BIN, link)
    return str(link)


def _base_command(app_name: str, *extra: str) -> list[str]:
    if not ELECTRON_BIN.exists():
        pytest.skip(
            f"Electron not installed at {ELECTRON_BIN} — run `npm install` in test-apps/electron"
        )
    return [
        _electron_symlink(app_name),
        "--no-sandbox",
        *extra,
        str(APP_DIR),
    ]


# Each fixture uses a unique app name so its Electron instance can be located
# via ``xa11y.App.by_name`` without colliding with the other fixture's process
# (both fixtures may be alive at once during a session).

@pytest.fixture(scope="session")
def electron_app_no_flag():
    """Launch Electron WITHOUT `--force-renderer-accessibility`.

    The Chromium AT-SPI bridge still registers (so the app appears in
    ``App.list()``) but the window's subtree is empty — the renderer
    accessibility tree was never built.
    """
    name = "xa11y-electron-noflag"
    yield from launch_test_app(
        command=_base_command(name),
        app_names=[name],
    )


@pytest.fixture(scope="session")
def electron_app_with_flag():
    """Launch Electron WITH `--force-renderer-accessibility`.

    The accessibility bridge is fully populated and the window subtree contains
    the rendered DOM.
    """
    name = "xa11y-electron-withflag"
    yield from launch_test_app(
        command=_base_command(name, "--force-renderer-accessibility"),
        app_names=[name],
    )
