"""Fixtures for xa11y GTK4 integration tests."""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

import pytest

from tests.helpers import launch_test_app

APP_SCRIPT = str(Path(__file__).resolve().parent.parent.parent / "test-apps" / "gtk" / "app.py")
APP_NAMES = ["xa11y-gtk-test-app", "gtk-test-app", "python3", "python", "Python", "app.py"]


@pytest.fixture(scope="session")
def gtk_app():
    """Launch the GTK4 test app and return an xa11y Element handle."""
    pid_file = tempfile.mktemp(suffix=".pid")
    yield from launch_test_app(
        command=[sys.executable, APP_SCRIPT, "--pid-file", pid_file],
        app_names=APP_NAMES,
    )
