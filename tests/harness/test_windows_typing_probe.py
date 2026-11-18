"""A UIA observation retry must preserve the input sample and native failures."""

import importlib.util
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest


class ReadTimeout(Exception):
    pass


@pytest.fixture
def probe(monkeypatch):
    # Only the read helper is exercised: no Windows API or bindings needed.
    monkeypatch.setitem(sys.modules, "xa11y", SimpleNamespace(TimeoutError=ReadTimeout))
    spec = importlib.util.spec_from_file_location(
        "typing_probe_under_test", Path(__file__).with_name("windows_typing_probe.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def notes_reading(*observations):
    values = iter(observations)
    calls = []

    def element():
        calls.append(True)
        value = next(values)
        if isinstance(value, Exception):
            raise value
        return SimpleNamespace(value=value)

    return SimpleNamespace(element=element), calls


def test_one_busy_provider_read_preserves_the_same_sample(probe, capsys):
    notes, calls = notes_reading(ReadTimeout("UIA connection timed out"), "a\r\nb")
    timeouts = []
    assert probe._read_value(notes, timeouts) == "a\nb"
    assert len(calls) == 2
    assert timeouts == ["UIA connection timed out"]
    assert "UIA connection timed out" in capsys.readouterr().err


def test_read_retry_does_not_correct_or_accept_mismatched_text(probe):
    notes, calls = notes_reading(ReadTimeout("busy"), "dropped characters")
    assert probe._wait_value(notes, "expected", [], complete_length=True) != "expected"
    assert len(calls) == 2


@pytest.mark.parametrize("failure", [ReadTimeout("still busy"), RuntimeError("denied")])
def test_second_read_failure_is_terminal(probe, failure):
    notes, calls = notes_reading(ReadTimeout("busy"), failure)
    with pytest.raises(type(failure), match=str(failure)):
        probe._read_value(notes, [])
    assert len(calls) == 2


def test_other_provider_errors_are_never_retried(probe):
    notes, calls = notes_reading(RuntimeError("stale or denied"))
    with pytest.raises(RuntimeError, match="stale or denied"):
        probe._read_value(notes, [])
    assert len(calls) == 1
