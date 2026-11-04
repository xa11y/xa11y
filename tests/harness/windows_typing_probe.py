"""Compare unpaced native packets with InputSim in a fresh Windows client.

This process only queries UIA and never registers event handlers. Its control
bypasses InputSim pacing intentionally; mismatches there are measurements,
not failures of the production API. InputSim mismatches make the process fail.
"""

from __future__ import annotations

import argparse
import ctypes
import json
import time
from ctypes import wintypes

import xa11y

NOTES = 'text_field[name="Notes"]'


class KeyboardInput(ctypes.Structure):
    _fields_ = [
        ("vk", wintypes.WORD),
        ("scan", wintypes.WORD),
        ("flags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("extra", ctypes.c_size_t),
    ]


class MouseInput(ctypes.Structure):
    _fields_ = [
        ("dx", wintypes.LONG),
        ("dy", wintypes.LONG),
        ("data", wintypes.DWORD),
        ("flags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("extra", ctypes.c_size_t),
    ]


class InputPayload(ctypes.Union):
    _fields_ = [("keyboard", KeyboardInput), ("mouse", MouseInput)]


class NativeInput(ctypes.Structure):
    _fields_ = [("type", wintypes.DWORD), ("payload", InputPayload)]


def _normalized(text):
    return text.replace("\r\n", "\n").replace("\r", "\n")


def _unpaced_type(text):
    """One SendInput call per scalar, with no delay or event subscriptions."""
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    send = user32.SendInput
    send.argtypes = [wintypes.UINT, ctypes.POINTER(NativeInput), ctypes.c_int]
    send.restype = wintypes.UINT
    expected_size = 40 if ctypes.sizeof(ctypes.c_void_p) == 8 else 28
    assert ctypes.sizeof(NativeInput) == expected_size, "native INPUT layout mismatch"
    for character in _normalized(text):
        events = []
        if character == "\n":
            parts = [(13, 0, 0)]  # VK_RETURN
        else:
            encoded = character.encode("utf-16-le")
            parts = [
                (0, lo | (hi << 8), 4) for lo, hi in zip(encoded[::2], encoded[1::2])
            ]
        for vk, scan, flags in parts:
            for extra_flags in (0, 2):  # KEYEVENTF_KEYUP on the second event
                events.append(
                    NativeInput(
                        1,
                        InputPayload(
                            keyboard=KeyboardInput(vk, scan, flags | extra_flags, 0, 0)
                        ),
                    )
                )
        batch = (NativeInput * len(events))(*events)
        sent = send(len(batch), batch, ctypes.sizeof(NativeInput))
        if sent != len(batch):
            raise RuntimeError(
                f"unpaced SendInput sent {sent}/{len(batch)} events; "
                f"GetLastError={ctypes.get_last_error()}"
            )


def _foreground_pid():
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.GetForegroundWindow.restype = wintypes.HWND
    user32.GetWindowThreadProcessId.argtypes = [
        wintypes.HWND,
        ctypes.POINTER(wintypes.DWORD),
    ]
    pid = wintypes.DWORD()
    user32.GetWindowThreadProcessId(user32.GetForegroundWindow(), ctypes.byref(pid))
    return pid.value


def _wait_value(notes, expected, *, complete_length=False):
    deadline = time.monotonic() + 3.0
    value = ""
    while time.monotonic() < deadline:
        value = _normalized(notes.element().value or "")
        if value == expected or (complete_length and len(value) >= len(expected)):
            return value
        time.sleep(0.02)
    return value


def _sample(shape, attempt):
    prefix = "abcdefghijklm"
    if shape == "newline":
        prefix += "\r\n"
    elif shape in ("unicode", "composed"):
        prefix += "hé😀漢字"
        if shape == "composed":
            prefix += "\n"
    return prefix + f"nopqrstuvwxyz0123456789-{attempt}"


def compare(pid, repeats):
    app = xa11y.App.by_pid(pid, timeout=10.0)
    notes = app.locator(NOTES)
    original = notes.element().value or ""
    sim = xa11y.input_sim()
    notes.focus()
    notes.wait_focused(timeout=3.0)
    rect = notes.element().bounds
    assert rect is not None, "Notes must have bounds"
    sim.click((rect.x + rect.width // 2, rect.y + rect.height // 2))
    report = {"pid": pid, "control": {}, "paced": {}}
    try:
        for mode, shapes in [
            ("control", ["plain"]),
            ("paced", ["plain", "newline", "unicode", "composed"]),
        ]:
            for shape in shapes:
                failures = []
                bad = 0
                started = time.monotonic()
                attempts = repeats * 2 if mode == "control" else repeats
                for attempt in range(attempts):
                    typed = _sample(shape, attempt)
                    expected = _normalized(typed)
                    notes.set_value("")
                    assert _wait_value(notes, "") == "", "Notes reset never completed"
                    if mode == "control":
                        _unpaced_type(typed)
                    elif shape == "composed":
                        for character in typed:
                            sim.type_text(character)
                    else:
                        sim.type_text(typed)
                    actual = _wait_value(notes, expected, complete_length=True)
                    if actual != expected:
                        bad += 1
                        if len(failures) < 3:
                            failures.append(
                                {
                                    "attempt": attempt,
                                    "expected": expected[:120],
                                    "actual": actual[:120],
                                    "foreground_pid": _foreground_pid(),
                                }
                            )
                report[mode][shape] = {
                    "attempts": attempts,
                    "bad": bad,
                    "examples": failures,
                    "elapsed": round(time.monotonic() - started, 2),
                }
    finally:
        notes.set_value(original)
    return report


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--repeats", type=int, default=60)
    args = parser.parse_args()
    assert 1 <= args.repeats <= 100, "bounded comparison needs 1..100 repetitions"
    report = compare(args.pid, args.repeats)
    print(json.dumps(report, ensure_ascii=True), flush=True)
    return int(any(result["bad"] for result in report["paced"].values()))


if __name__ == "__main__":
    raise SystemExit(main())
