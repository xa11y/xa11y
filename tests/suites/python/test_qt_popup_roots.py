"""Windows Qt cases where native window roots differ from the UIA tree."""

from __future__ import annotations

import ctypes
import sys
import time

import pytest


@pytest.fixture(autouse=True)
def windows_qt_only(app_name):
    if sys.platform != "win32" or app_name != "qt":
        pytest.skip("requires Qt on Windows")


def wait_for_native_popup(pid: int) -> None:
    """Wait until Qt has created its HWND popup, not just reported expanded."""
    from ctypes import wintypes

    user32 = ctypes.WinDLL("user32", use_last_error=True)
    callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    user32.EnumWindows.argtypes = [callback_type, wintypes.LPARAM]
    user32.GetWindowThreadProcessId.argtypes = [
        wintypes.HWND,
        ctypes.POINTER(wintypes.DWORD),
    ]
    user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
    user32.IsWindowVisible.argtypes = [wintypes.HWND]

    def popup_exists() -> bool:
        found = False

        @callback_type
        def collect(hwnd, _param):
            nonlocal found
            owner_pid = wintypes.DWORD()
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner_pid))
            if owner_pid.value == pid and user32.IsWindowVisible(hwnd):
                class_name = ctypes.create_unicode_buffer(256)
                user32.GetClassNameW(hwnd, class_name, len(class_name))
                found |= "QWindowPopup" in class_name.value
            return True

        user32.EnumWindows(collect, 0)
        return found

    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if popup_exists():
            return
        time.sleep(0.05)
    pytest.fail("Qt popup HWND did not open")


def test_open_combo_item_has_one_app_match(app):
    """An open popup must not make one Qt item appear twice in an app query."""
    combo = app.locator('combo_box[name="Fruit"]')
    combo.expand()
    try:
        wait_for_native_popup(app.pid)
        items = app.locator('list_item[name="Banana"]').elements()
        assert len(items) == 1, (
            f"expected one Banana item in the open combo, found {len(items)}"
        )
    finally:
        combo.collapse()


def test_open_file_menu_item_has_one_app_match(app):
    """Adding a menu HWND root must not repeat items already under the main root."""
    app.locator('button[name="Open File Menu"]').press()
    try:
        wait_for_native_popup(app.pid)
        items = app.locator('menu_item[name="New"]').elements()
        assert len(items) == 1, f"expected one File > New item, found {len(items)}"
    finally:
        # Invoke an item to dismiss the test fixture's popup.
        app.locator('menu_item[name="New"]').elements()[0].press()


def test_owned_dialog_is_in_app_windows(app):
    """A same-process owned dialog is still a top-level application window."""
    app.locator('button[name="Open Dialog"]').press()
    try:
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if app.locator('dialog[name="Sample Dialog"]').exists():
                break
            time.sleep(0.05)
        else:
            pytest.fail("Qt dialog did not open")

        assert any(window.name == "Sample Dialog" for window in app.windows()), (
            "the open owned dialog is missing from App.windows()"
        )
    finally:
        app.locator('button[name="Close Dialog"]').press()
