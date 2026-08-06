"""pytest-xa11y — desktop UI testing with pytest, on top of xa11y.

Define one fixture saying how your app starts, and the plugin handles the
rest: launching it, waiting for it to register with the platform
accessibility API, resetting it between tests, skipping what this session
cannot do, and attaching the tree to whatever fails.

    import pytest
    from pytest_xa11y import AppLauncher

    @pytest.fixture(scope="session")
    def xa11y_launcher():
        return AppLauncher(command=["./my-app"], ready='button[name="Sign in"]')

    def test_sign_in(xa11y_app):
        xa11y_app.locator('text_field[name="Email"]').set_value("a@b.c")
        xa11y_app.locator('button[name="Sign in"]').press()
        xa11y_app.locator('static_text[name^="Welcome"]').wait_visible()

The plugin deliberately wraps none of xa11y's own API: ``xa11y_app`` is an
``xa11y.App``, and locators, elements, actions and errors are the library's
own. There is no second API surface here to drift out of step with it.
"""

from __future__ import annotations

from importlib.metadata import PackageNotFoundError, version

from .capabilities import KNOWN_CAPABILITIES, Capabilities, Capability
from .diagnostics import register_diagnostic
from .errors import AppDied, AppLaunchError, LauncherNotConfigured, PytestXa11yError
from .events import EventRecorder
from .frontmost import ensure_macos_frontmost
from .launcher import AppLauncher
from .session import AppSession

try:
    __version__ = version("pytest-xa11y")
except PackageNotFoundError:  # pragma: no cover - source tree without an install
    __version__ = "0.0.0.dev0"

__all__ = [
    "KNOWN_CAPABILITIES",
    "AppDied",
    "AppLaunchError",
    "AppLauncher",
    "AppSession",
    "Capabilities",
    "Capability",
    "EventRecorder",
    "LauncherNotConfigured",
    "PytestXa11yError",
    "__version__",
    "ensure_macos_frontmost",
    "register_diagnostic",
]
