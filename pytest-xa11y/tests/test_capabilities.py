"""Capability probing and the skip decisions that follow from it."""

from __future__ import annotations

import pytest
import xa11y
from _pytest.outcomes import Skipped

from pytest_xa11y import Capabilities, Capability
from pytest_xa11y.capabilities import INPUT_SIM, SCREENSHOT


@pytest.fixture
def no_probes(monkeypatch):
    """Fail loudly if a probe runs when the test did not arrange one."""

    def unexpected(*args, **kwargs):
        raise AssertionError("probe should not have run")

    monkeypatch.setattr(xa11y, "screenshot", unexpected)
    monkeypatch.setattr(xa11y, "input_sim", unexpected)


def test_unknown_capability_is_an_error(no_probes):
    caps = Capabilities()
    with pytest.raises(ValueError, match="Unknown capability"):
        caps.check("teleportation")


def test_disabled_capability_is_not_probed(no_probes):
    caps = Capabilities((SCREENSHOT,))
    available, reason = caps.check(SCREENSHOT)
    assert available is False
    assert "--xa11y-skip=screenshot" in reason


def test_input_sim_env_switch_is_honoured(monkeypatch, no_probes):
    monkeypatch.setenv("XA11Y_SKIP_INPUT_SIM", "1")
    available, reason = Capabilities().check(INPUT_SIM)
    assert available is False
    assert "XA11Y_SKIP_INPUT_SIM" in reason


@pytest.mark.parametrize(
    ("raised", "expected"),
    [
        (xa11y.PermissionDeniedError("grant Screen Recording"), "permission not granted"),
        (xa11y.ActionNotSupportedError("Unsupported: capture"), "unsupported"),
        (xa11y.PlatformError("Platform error (1): GetImage failed"), "no capture path"),
    ],
)
def test_screenshot_probe_maps_failures_to_reasons(monkeypatch, raised, expected):
    monkeypatch.setattr(xa11y, "screenshot", _raiser(raised))
    available, reason = Capabilities().check(SCREENSHOT)
    assert available is False
    assert expected in reason


def test_screenshot_probe_success(monkeypatch):
    monkeypatch.setattr(xa11y, "screenshot", lambda **kwargs: object())
    assert Capabilities().check(SCREENSHOT) == (True, None)


def test_probe_runs_once_per_session(monkeypatch):
    calls = []

    def probe(**kwargs):
        calls.append(kwargs)
        return object()

    monkeypatch.setattr(xa11y, "screenshot", probe)
    caps = Capabilities()
    caps.available(SCREENSHOT)
    caps.available(SCREENSHOT)
    caps.reason(SCREENSHOT)
    assert len(calls) == 1


def test_unrecognised_platform_error_propagates_rather_than_skipping(monkeypatch):
    # A capture pipeline that is genuinely broken must fail the suite, not
    # turn it green via a module-wide skip. Only the known "no capture path"
    # signatures mean unavailable; this is the same policy guard() applies.
    monkeypatch.setattr(
        xa11y, "screenshot", _raiser(xa11y.PlatformError("Platform error (5): decoder blew up"))
    )
    with pytest.raises(xa11y.PlatformError, match="decoder blew up"):
        Capabilities().check(SCREENSHOT)


def test_headless_linux_signature_is_recognised(monkeypatch):
    monkeypatch.setattr(
        xa11y,
        "screenshot",
        _raiser(
            xa11y.PlatformError(
                "Platform error (-1): Unsupported: screenshot (no DISPLAY or WAYLAND_DISPLAY set)"
            )
        ),
    )
    available, reason = Capabilities().check(SCREENSHOT)
    assert available is False
    assert "no capture path" in reason


def test_probe_captures_the_full_display_not_a_region(monkeypatch):
    # A full-display capture can succeed where a region capture is rejected,
    # so probing with a region would skip modules whose captures would work.
    calls = []
    monkeypatch.setattr(xa11y, "screenshot", lambda **kw: calls.append(kw) or object())
    Capabilities().check(SCREENSHOT)
    assert calls == [{}]


def test_skip_unless_skips_with_the_reason(monkeypatch):
    monkeypatch.setattr(xa11y, "screenshot", _raiser(xa11y.PermissionDeniedError("nope")))
    with pytest.raises(Skipped, match="screenshot unavailable"):
        Capabilities().skip_unless(SCREENSHOT)


def test_guard_converts_unavailability_to_a_skip(no_probes):
    caps = Capabilities()
    with pytest.raises(Skipped, match="permission not granted"):  # noqa: SIM117
        with caps.guard(SCREENSHOT):
            raise xa11y.PermissionDeniedError("grant Screen Recording")


def test_guard_reraises_a_real_capture_bug(no_probes):
    # A PlatformError that is not one of the known "no capture path in this
    # session" signatures is a genuine failure and must not be skipped away.
    caps = Capabilities()
    with pytest.raises(xa11y.PlatformError):  # noqa: SIM117
        with caps.guard(SCREENSHOT):
            raise xa11y.PlatformError("Platform error (5): decoder blew up")


def test_guard_recognises_headless_x11_region_rejection(no_probes):
    caps = Capabilities()
    with pytest.raises(Skipped):  # noqa: SIM117
        with caps.guard(SCREENSHOT):
            raise xa11y.PlatformError("Platform error (2): X11 GetImage BadMatch")


def test_input_sim_probe_reports_linux_style_construction_failure(monkeypatch):
    monkeypatch.setattr(
        xa11y, "input_sim", _raiser(xa11y.PermissionDeniedError("open /dev/uinput"))
    )
    available, reason = Capabilities().check(INPUT_SIM)
    assert available is False
    assert "/dev/uinput" in reason


def test_summary_names_the_unavailable_reason(monkeypatch, no_probes):
    caps = Capabilities((SCREENSHOT, INPUT_SIM))
    summary = caps.summary()
    assert "screenshot=no" in summary
    assert "input_sim=no" in summary


def _raiser(exc):
    def raise_it(*args, **kwargs):
        raise exc

    return raise_it


def test_capability_members_are_plain_strings(no_probes):
    # The enum is a spelling, not a second vocabulary: everything that takes
    # a capability name must keep taking a string.
    assert Capability.SCREENSHOT == "screenshot"
    assert f"{Capability.SCREENSHOT}" == "screenshot"
    caps = Capabilities((Capability.SCREENSHOT,))
    assert caps.available("screenshot") is False
    assert caps.available(Capability.SCREENSHOT) is False
