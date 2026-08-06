"""AppLauncher validation — every rejection here is a mistake that would
otherwise surface as a confusing launch failure minutes into a CI run."""

from __future__ import annotations

import os

import pytest

from pytest_xa11y import AppLauncher


def test_command_as_string_is_rejected():
    with pytest.raises(ValueError, match="list of arguments, not a string"):
        AppLauncher(command="./my-app --headless")


def test_command_or_attach_pid_is_required():
    with pytest.raises(ValueError, match="either command"):
        AppLauncher()


def test_command_and_attach_pid_are_mutually_exclusive():
    with pytest.raises(ValueError, match="not both"):
        AppLauncher(command=["./app"], attach_pid=123)


def test_attach_pid_must_be_positive():
    with pytest.raises(ValueError, match="must be positive"):
        AppLauncher(attach_pid=0)


@pytest.mark.parametrize("bad", [0, -1.0, float("nan"), float("inf")])
def test_startup_timeout_must_be_positive_and_finite(bad):
    with pytest.raises(ValueError, match="positive, finite"):
        AppLauncher(command=["./app"], startup_timeout=bad)


def test_reset_must_be_callable():
    with pytest.raises(ValueError, match="must be callable"):
        AppLauncher(command=["./app"], reset="restart")


def test_sequences_are_normalised_to_tuples():
    launcher = AppLauncher(command=["./app", "--headless"], app_names=["a", "b"])
    assert launcher.command == ("./app", "--headless")
    assert launcher.app_names == ("a", "b")


def test_display_name_prefers_label_then_command_then_pid():
    assert AppLauncher(command=["/opt/bin/my-app"]).display_name == "my-app"
    assert AppLauncher(command=["/opt/bin/my-app"], label="Widgets").display_name == "Widgets"
    assert AppLauncher(attach_pid=42).display_name == "pid-42"


def test_resolved_env_merges_over_os_environ(monkeypatch):
    monkeypatch.setenv("XA11Y_TEST_SENTINEL", "outer")
    launcher = AppLauncher(command=["./app"], env={"QT_ACCESSIBILITY": "1"})
    env = launcher.resolved_env()
    assert env["QT_ACCESSIBILITY"] == "1"
    assert env["XA11Y_TEST_SENTINEL"] == "outer"
    assert len(env) >= len(os.environ)


def test_resolved_env_is_none_without_overrides():
    # None means "inherit", which is not the same as an empty environment.
    assert AppLauncher(command=["./app"]).resolved_env() is None


def test_app_names_and_prefix_are_mutually_exclusive():
    with pytest.raises(ValueError, match="not both"):
        AppLauncher(command=["./app"], app_names=["a"], app_name_prefix="Dialog")


def test_app_name_prefix_is_accepted_alone():
    launcher = AppLauncher(command=["./app"], app_name_prefix="Submit to ")
    assert launcher.app_name_prefix == "Submit to "


def test_spawns_and_exits_defaults_off_so_apps_are_watched():
    # The default has to be "watch this process": it is what makes a startup
    # crash report its exit code, and app_names must not opt out of that.
    assert AppLauncher(command=["./app"], app_names=["a"]).spawns_and_exits is False


def test_spawns_and_exits_and_attach_pid_are_mutually_exclusive():
    # Attach mode launches nothing, so there is no handoff to describe — and
    # the attached PID is the process to watch whatever spawned it.
    with pytest.raises(ValueError, match="not both"):
        AppLauncher(attach_pid=42, spawns_and_exits=True)
