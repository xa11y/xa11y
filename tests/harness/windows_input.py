"""Read-only native routing evidence for Windows input-test failures (#605)."""

from __future__ import annotations

import ctypes
import sys
from ctypes import wintypes


def _process_image(kernel32, pid: int) -> str:
    # Limited-query access only; never open a process for mutation or injection.
    handle = kernel32.OpenProcess(0x1000, False, pid)
    if not handle:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        image = ctypes.create_unicode_buffer(1025)
        length = wintypes.DWORD(len(image))
        if not kernel32.QueryFullProcessImageNameW(
            handle, 0, image, ctypes.byref(length)
        ):
            raise ctypes.WinError(ctypes.get_last_error())
        return repr(image.value)
    finally:
        if not kernel32.CloseHandle(handle):
            raise ctypes.WinError(ctypes.get_last_error())


def _snapshot(user32, kernel32=None) -> str:
    point = wintypes.POINT()
    if not user32.GetPhysicalCursorPos(ctypes.byref(point)):
        raise ctypes.WinError(ctypes.get_last_error())

    def detail(probe) -> str:
        try:
            return probe()
        except Exception as exc:  # noqa: BLE001 - retain other routing evidence
            return f"<probe failed: {type(exc).__name__}: {exc}>"

    images = {}

    def process_image(pid) -> str:
        if kernel32 is None:
            return "<not queried>"
        if pid not in images:
            images[pid] = detail(lambda: _process_image(kernel32, pid))
        return images[pid]

    def process_id(hwnd) -> int:
        pid = wintypes.DWORD()
        if not user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid)):
            raise ctypes.WinError(ctypes.get_last_error())
        return pid.value

    def window_class(hwnd) -> str:
        name = ctypes.create_unicode_buffer(257)
        if not user32.GetClassNameW(hwnd, name, len(name)):
            raise ctypes.WinError(ctypes.get_last_error())
        return repr(name.value)

    def root_window(hwnd) -> str:
        root = user32.GetAncestor(hwnd, 2)  # GA_ROOT, not the owner chain.
        if not root:
            raise OSError("GetAncestor(GA_ROOT) returned no window")
        pid = process_id(root)
        return f"hwnd={root:#x}, pid={pid}, image={process_image(pid)}"

    def window(hwnd) -> str:
        if not hwnd:
            return "<none>"
        pid = process_id(hwnd)
        # Bounded and read-only: GetWindowTextW reads the caption without
        # messaging another process. A zero length can mean an empty caption.
        title = ctypes.create_unicode_buffer(257)
        ctypes.set_last_error(0)
        length = user32.GetWindowTextW(hwnd, title, len(title))
        if length == 0 and ctypes.get_last_error():
            raise ctypes.WinError(ctypes.get_last_error())
        return (
            f"hwnd={hwnd:#x}, pid={pid}, title={title.value!r}, "
            f"class={detail(lambda: window_class(hwnd))}, image={process_image(pid)}, "
            f"root=({detail(lambda: root_window(hwnd))})"
        )

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
        user32.GetWindowThreadProcessId.argtypes = [
            wintypes.HWND,
            ctypes.POINTER(wintypes.DWORD),
        ]
        user32.GetWindowThreadProcessId.restype = wintypes.DWORD
        user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
        user32.GetWindowTextW.restype = ctypes.c_int
        user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
        user32.GetClassNameW.restype = ctypes.c_int
        user32.GetAncestor.argtypes = [wintypes.HWND, wintypes.UINT]
        user32.GetAncestor.restype = wintypes.HWND
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel32.OpenProcess.restype = wintypes.HANDLE
        kernel32.QueryFullProcessImageNameW.argtypes = [
            wintypes.HANDLE,
            wintypes.DWORD,
            wintypes.LPWSTR,
            ctypes.POINTER(wintypes.DWORD),
        ]
        kernel32.QueryFullProcessImageNameW.restype = wintypes.BOOL
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel32.CloseHandle.restype = wintypes.BOOL
        return _snapshot(user32, kernel32)
    except Exception as exc:  # noqa: BLE001 - preserve the input-test failure
        return f"Windows input routing probe failed: {type(exc).__name__}: {exc}"
