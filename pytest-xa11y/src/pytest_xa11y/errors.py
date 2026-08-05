"""Exceptions raised by the plugin itself (never by the app under test)."""

from __future__ import annotations


class PytestXa11yError(Exception):
    """Base class for pytest-xa11y's own failures."""


class LauncherNotConfigured(PytestXa11yError):
    """No ``xa11y_launcher`` fixture was defined by the test suite."""


class AppLaunchError(PytestXa11yError):
    """The app under test could not be launched, found, or made ready."""


class AppDied(PytestXa11yError):
    """The app under test exited while the suite was still running.

    Raised between tests rather than inside one, so the run reports the
    process death instead of the pile of unrelated selector failures the
    remaining tests would otherwise produce.
    """
