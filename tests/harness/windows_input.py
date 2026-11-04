"""Read-only native routing evidence for Windows input-test failures (#605)."""

from __future__ import annotations

import ctypes
import sys
from ctypes import wintypes


def _snapshot(user32) -> str:
    point = wintypes.POINT()
    if not user32.GetPhysicalCursorPos(ctypes.byref(point)):
        raise ctypes.WinError(ctypes.get_last_error())

    def window(hwnd) -> str:
        if not hwnd:
            return "<none>"
        pid = wintypes.DWORD()
        if not user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid)):
            raise ctypes.WinError(ctypes.get_last_error())
        # Bounded and read-only: GetWindowTextW reads the caption without
        # messaging another process. A zero length can mean an empty caption.
        title = ctypes.create_unicode_buffer(257)
        ctypes.set_last_error(0)
        length = user32.GetWindowTextW(hwnd, title, len(title))
        if length == 0 and ctypes.get_last_error():
            raise ctypes.WinError(ctypes.get_last_error())
        return f"hwnd={hwnd:#x}, pid={pid.value}, title={title.value!r}"

    return (
        f"physical cursor=({point.x}, {point.y}); "
        f"foreground=({window(user32.GetForegroundWindow())}); "
        f"under cursor=({window(user32.WindowFromPhysicalPoint(point))})"
    )


def input_snapshot() -> str:
    """Sample cursor and window ownership, reporting probe errors explicitly.

    These sequential reads describe the instant sampled, not a guarantee of
    where an earlier event was delivered. Physical coordinates avoid the test
    process's DPI-awareness setting changing the cursor position reported.
    """
    if sys.platform != "win32":
        return "Windows input routing: not applicable"
    try:
        user32 = ctypes.WinDLL("user32", use_last_error=True)
        # HWND is pointer-sized; ctypes' default c_int would truncate it.
        user32.GetPhysicalCursorPos.argtypes = [ctypes.POINTER(wintypes.POINT)]
        user32.GetPhysicalCursorPos.restype = wintypes.BOOL
        user32.GetForegroundWindow.argtypes = []
        user32.GetForegroundWindow.restype = wintypes.HWND
        user32.WindowFromPhysicalPoint.argtypes = [wintypes.POINT]
        user32.WindowFromPhysicalPoint.restype = wintypes.HWND
        user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
        user32.GetWindowThreadProcessId.restype = wintypes.DWORD
        user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
        user32.GetWindowTextW.restype = ctypes.c_int
        return _snapshot(user32)
    except Exception as exc:  # noqa: BLE001 - preserve the input-test failure
        return f"Windows input routing probe failed: {type(exc).__name__}: {exc}"
