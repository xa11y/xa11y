"""Integration tests for xa11y.input_sim() against the Tauri input-events page.

Each test drives synthesised mouse/keyboard events via `xa11y.input_sim()` and
asserts the webview received them by reading the event-log textarea's value
through the accessibility tree.

Every platform needs the host process to hold the input-synthesis permission
(Accessibility + Input Monitoring on macOS, XTest on X11 — no grant needed
on Windows). CI on macOS GitHub Actions runners typically lacks the grant,
so these tests are skipped there via the XA11Y_SKIP_INPUT_SIM env var set
by the harness.

Hit-target and typed-text fields are identified by role + name (aria-label).
The event log is a read-only textarea; on macOS/Linux a11y trees it surfaces
as a `text_field` with the log text as its value.
"""

from __future__ import annotations

import os
import platform
import time

import pytest
import xa11y

HIT_TARGET = 'button[name="Hit target"]'
EVENT_LOG = 'text_area[name="Event log"]'
TYPED_FIELD = 'text_field[name="Typed text"]'
CLEAR_BUTTON = 'button[name="Clear log"]'

# Wait up to this long for a log line to appear after we post an event.
LOG_SETTLE_TIMEOUT = 2.0


pytestmark = pytest.mark.skipif(
    os.environ.get("XA11Y_SKIP_INPUT_SIM") == "1",
    reason="Input simulation disabled (XA11Y_SKIP_INPUT_SIM=1)",
)


# ── Helpers ────────────────────────────────────────────────────────────────


def _read_log(app: xa11y.App) -> str:
    """Read the event log textarea value via a11y."""
    return app.locator(EVENT_LOG).element().value or ""


def _wait_for_log(app: xa11y.App, predicate, timeout: float = LOG_SETTLE_TIMEOUT) -> str:
    """Poll the log until `predicate(text)` returns True or we time out."""
    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        last = _read_log(app)
        if predicate(last):
            return last
        time.sleep(0.05)
    return last


def _clear_log(app: xa11y.App) -> None:
    app.locator(CLEAR_BUTTON).press()
    # The press itself is a synthetic a11y action (not input sim), so the
    # log should be empty immediately — but give one tick for the webview
    # handler to run.
    _wait_for_log(app, lambda t: t == "", timeout=0.5)


def _focus_hit_target(app: xa11y.App) -> None:
    """Focus the hit target so keyboard events land on a meaningful element."""
    app.locator(HIT_TARGET).focus()


def _focus_typed_field(app: xa11y.App) -> None:
    app.locator(TYPED_FIELD).focus()


def _hit_center(app: xa11y.App) -> tuple[int, int]:
    """Screen-coordinate center of the hit-target region."""
    el = app.locator(HIT_TARGET).element()
    r = el.bounds
    assert r is not None, "hit target has no bounds"
    return (r.x + r.width // 2, r.y + r.height // 2)


# ── Fixture ────────────────────────────────────────────────────────────────


@pytest.fixture
def sim() -> xa11y.InputSim:
    return xa11y.input_sim()


# ── Mouse ──────────────────────────────────────────────────────────────────


def test_single_click_reports_mousedown_and_up(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    sim.click(_hit_center(tauri_input_app))
    log = _wait_for_log(tauri_input_app, lambda t: "mouseup" in t and "mousedown" in t)
    assert "mousedown" in log
    assert "mouseup" in log
    assert "click" in log
    assert "button=left" in log


def test_double_click_reports_dblclick(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    sim.double_click(_hit_center(tauri_input_app))
    log = _wait_for_log(tauri_input_app, lambda t: "dblclick" in t)
    assert "dblclick" in log, f"expected dblclick in log, got:\n{log}"


def test_right_click_reports_right_button(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    sim.right_click(_hit_center(tauri_input_app))
    log = _wait_for_log(tauri_input_app, lambda t: "button=right" in t)
    assert "button=right" in log


def test_click_on_element_target(tauri_input_app, sim):
    """Passing an Element as the click target should use its centre bounds."""
    _clear_log(tauri_input_app)
    hit_el = tauri_input_app.locator(HIT_TARGET).element()
    sim.click(hit_el)
    log = _wait_for_log(tauri_input_app, lambda t: "click" in t)
    assert "click" in log


def test_drag_emits_mousemove_between_down_and_up(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    el = tauri_input_app.locator(HIT_TARGET).element()
    r = el.bounds
    assert r is not None
    # Drag within the hit target — browsers only fire mousemove on the
    # element that captured the mousedown, so both endpoints must be inside.
    start = (r.x + 20, r.y + 20)
    end = (r.x + r.width - 20, r.y + r.height - 20)
    sim.drag(start, end)
    log = _wait_for_log(tauri_input_app, lambda t: "mouseup" in t and "mousemove" in t)
    assert "mousedown" in log
    assert "mousemove" in log
    assert "mouseup" in log


# ── Keyboard ───────────────────────────────────────────────────────────────


def test_key_press_reports_keydown_keyup(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    _focus_hit_target(tauri_input_app)
    sim.press("a")
    log = _wait_for_log(tauri_input_app, lambda t: "keyup" in t and "keydown" in t)
    assert "keydown" in log
    assert "keyup" in log
    assert "key=a" in log


def test_named_key_press(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    _focus_hit_target(tauri_input_app)
    sim.press("Enter")
    log = _wait_for_log(tauri_input_app, lambda t: "keyup" in t)
    assert "key=Enter" in log


def test_chord_reports_modifier(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    _focus_hit_target(tauri_input_app)
    # Shift+a (capital A via the explicit-shift API contract).
    sim.chord("a", ["Shift"])
    log = _wait_for_log(tauri_input_app, lambda t: "keyup" in t and "shift" in t)
    assert "mods=shift" in log or "shift" in log.split("\n")[0]


def test_platform_meta_chord(tauri_input_app, sim):
    """Cmd/Ctrl+A should fire with the platform 'meta' modifier held."""
    _clear_log(tauri_input_app)
    _focus_typed_field(tauri_input_app)
    sim.type_text("hello")
    _clear_log(tauri_input_app)
    sim.chord("a", ["Meta"])
    log = _wait_for_log(tauri_input_app, lambda t: "keyup" in t and "key=a" in t)
    assert "meta" in log


# ── Typing ─────────────────────────────────────────────────────────────────


def test_type_text_writes_to_focused_input(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    _focus_typed_field(tauri_input_app)
    sim.type_text("hello xa11y")
    # Poll the typed-text input's value (not the event log) — type_text uses
    # Unicode / scancode paths that don't always generate synthetic key events
    # at the DOM level.
    deadline = time.monotonic() + LOG_SETTLE_TIMEOUT
    while time.monotonic() < deadline:
        val = tauri_input_app.locator(TYPED_FIELD).element().value or ""
        if val == "hello xa11y":
            return
        time.sleep(0.05)
    pytest.fail(f"typed-text field did not receive expected text, got: {val!r}")


# ── Scroll ─────────────────────────────────────────────────────────────────


def test_scroll_reports_wheel(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    sim.scroll(_hit_center(tauri_input_app), dx=0, dy=3)
    log = _wait_for_log(tauri_input_app, lambda t: "wheel" in t)
    assert "wheel" in log


# ── Smoke ──────────────────────────────────────────────────────────────────


def test_input_sim_construct():
    """Constructing the sim should succeed on every supported platform."""
    sim = xa11y.input_sim()
    assert sim is not None


def test_platform_matrix_smoke():
    """Sanity: we know which platform this runs on."""
    assert platform.system() in ("Darwin", "Linux", "Windows")
