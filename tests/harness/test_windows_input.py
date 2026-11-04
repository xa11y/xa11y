"""Routing diagnostics must preserve HWND width and the original failure."""

from __future__ import annotations

import ctypes
from ctypes import wintypes
from types import SimpleNamespace

from tests.harness import windows_input


class NativeCall:
    def __init__(self, implementation):
        self.implementation = implementation

    def __call__(self, *args):
        return self.implementation(*args)


def test_snapshot_keeps_physical_coordinates_and_pointer_sized_handles(monkeypatch):
    foreground = 0x123456789
    under_cursor = 0x23456789A

    def cursor(pointer):
        point = ctypes.cast(pointer, ctypes.POINTER(wintypes.POINT)).contents
        point.x, point.y = -1200, 450
        return 1

    def window_at(point):
        assert (point.x, point.y) == (-1200, 450)
        return under_cursor

    def process(hwnd, pointer):
        pid = ctypes.cast(pointer, ctypes.POINTER(wintypes.DWORD)).contents
        pid.value = 42 if hwnd == foreground else 99
        return 1

    def title(hwnd, buffer, capacity):
        assert capacity == 257
        buffer.value = "Target" if hwnd == foreground else "Overlay"
        return len(buffer.value)

    native = SimpleNamespace(
        GetPhysicalCursorPos=NativeCall(cursor),
        GetForegroundWindow=NativeCall(lambda: foreground),
        WindowFromPhysicalPoint=NativeCall(window_at),
        GetWindowThreadProcessId=NativeCall(process),
        GetWindowTextW=NativeCall(title),
    )
    monkeypatch.setattr(windows_input.sys, "platform", "win32")
    monkeypatch.setattr(ctypes, "WinDLL", lambda *_args, **_kwargs: native, raising=False)
    monkeypatch.setattr(ctypes, "set_last_error", lambda _code: None, raising=False)
    result = windows_input.input_snapshot()
    assert "physical cursor=(-1200, 450)" in result
    assert "foreground=(hwnd=0x123456789, pid=42, title='Target')" in result
    assert "under cursor=(hwnd=0x23456789a, pid=99, title='Overlay')" in result
    assert native.GetForegroundWindow.restype is wintypes.HWND
    assert native.WindowFromPhysicalPoint.restype is wintypes.HWND


def test_probe_failure_is_reported_without_replacing_test_failure(monkeypatch):
    def unavailable(*_args, **_kwargs):
        raise OSError("desktop unavailable")

    monkeypatch.setattr(windows_input.sys, "platform", "win32")
    monkeypatch.setattr(ctypes, "WinDLL", unavailable, raising=False)
    assert windows_input.input_snapshot() == (
        "Windows input routing probe failed: OSError: desktop unavailable"
    )


def test_no_foreground_or_window_under_cursor_is_explicit(monkeypatch):
    def cursor(pointer):
        return 1

    native = SimpleNamespace(
        GetPhysicalCursorPos=cursor,
        GetForegroundWindow=lambda: None,
        WindowFromPhysicalPoint=lambda _point: None,
    )
    result = windows_input._snapshot(native)
    assert "foreground=(<none>); under cursor=(<none>)" in result


def test_other_platform_does_not_load_user32(monkeypatch):
    monkeypatch.setattr(windows_input.sys, "platform", "linux")
    assert windows_input.input_snapshot() == "Windows input routing: not applicable"
