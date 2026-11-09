"""The capability table, through the Python surface.

Hermetic: ``capabilities()`` reads a static table and never connects to an
app, so these run without a desktop.
"""

import sys

import pytest
import xa11y

CURRENT = {"linux": "linux", "darwin": "macos", "win32": "windows"}[sys.platform]


def test_current_platform_by_default():
    caps = xa11y.capabilities()
    assert isinstance(caps, xa11y.Capabilities)
    assert caps.platform == CURRENT
    if CURRENT == "linux":
        assert caps.os_version is None


def test_every_platform_answers_every_feature():
    names = xa11y.Capabilities.names()
    assert len(names) == len(set(names))
    for platform in ("linux", "macos", "windows"):
        caps = xa11y.capabilities(platform)
        assert caps.platform == platform
        assert len(caps) == len(names)
        assert [c.name for c in caps] == names
        assert [c.name for c in caps.entries()] == names
        for cap in caps:
            assert isinstance(cap, xa11y.Capability)
            assert cap.support in ("supported", "partial", "unsupported")
            assert cap.description.endswith(".")
            if cap.support != "supported":
                assert cap.note, f"{platform} {cap.name}: no reason given"


def test_answers_differ_by_platform():
    linux = xa11y.capabilities("linux")
    mac = xa11y.capabilities("macos")
    win = xa11y.capabilities("windows")
    assert linux.support("event:menu_opened") == "unsupported"
    assert mac.support("event:menu_opened") == "supported"
    assert win.support("window:enter_fullscreen") == "unsupported"
    assert linux.support("window:maximize") == "partial"
    assert "X11" in linux.note("window:maximize")
    assert mac.note("event:focus_changed") is None


def test_lookup_forms():
    caps = xa11y.capabilities("linux")
    assert "event:focus_changed" in caps
    assert caps.has("event:focus_changed")
    assert not caps.has("event:nope")
    assert "event:nope" not in caps
    assert 42 not in caps
    assert caps["action:blur"].name == "action:blur"
    with pytest.raises(KeyError):
        caps["event:nope"]
    assert caps.get("action:blur").support == "supported"
    assert repr(caps).startswith("Capabilities(platform='linux'")
    assert "action:blur" in repr(caps.get("action:blur"))


def test_unknown_name_suggests_close_ones():
    caps = xa11y.capabilities("macos")
    with pytest.raises(xa11y.InvalidActionDataError) as info:
        caps.get("event:menu_open")
    assert "event:menu_opened" in str(info.value)
    with pytest.raises(xa11y.InvalidActionDataError):
        caps.support("bogus")


def test_versions():
    caps = xa11y.capabilities("windows", "10.0.19045")
    assert caps.os_version == "10.0.19045"
    assert xa11y.capabilities("macos", os_version="14.5").os_version == "14.5"
    with pytest.raises(xa11y.InvalidActionDataError, match="Linux has no single platform"):
        xa11y.capabilities("linux", "6.1")
    with pytest.raises(xa11y.InvalidActionDataError, match="dotted version"):
        xa11y.capabilities("windows", "eleven")


def test_unknown_platform():
    with pytest.raises(xa11y.InvalidActionDataError, match="unknown platform"):
        xa11y.capabilities("darwin")


def test_version_alone_narrows_the_current_platform():
    if CURRENT == "linux":
        with pytest.raises(xa11y.InvalidActionDataError):
            xa11y.capabilities(os_version="1.0")
    else:
        caps = xa11y.capabilities(os_version="1.0")
        assert caps.platform == CURRENT
        assert caps.os_version == "1.0"


def test_environment_source_is_described_without_misreporting_uia():
    import os
    import subprocess

    code = (
        "import xa11y\n"
        "try:\n"
        "    print(xa11y.capabilities().backend)\n"
        "except xa11y.ActionNotSupportedError:\n"
        "    print('unsupported')\n"
        "print(xa11y.capabilities('windows').backend)\n"
    )
    env = {**os.environ, "XA11Y_ACCESSIBILITY_SOURCE": "ia2"}
    out = subprocess.run(
        [sys.executable, "-c", code], env=env, capture_output=True, text=True, check=True
    ).stdout.split()
    assert out == ["ia2" if CURRENT == "windows" else "unsupported", "uia"], out


def test_explicit_ia2_capabilities():
    caps = xa11y.capabilities("windows", backend="ia2")
    assert caps.backend == "ia2"
    assert caps.support("event:focus_changed") == "unsupported"
    assert caps.support("text:object_at") == "supported"
    assert caps.support("relation:controlled_by") == "supported"
    for platform in ("linux", "macos"):
        with pytest.raises(xa11y.ActionNotSupportedError):
            xa11y.capabilities(platform, backend="ia2")
