"""Integration tests for xa11y.input_sim() against the Tauri input-events page.

Each test drives synthesised mouse/keyboard events via `xa11y.input_sim()` and
asserts the webview received them by reading the event-log textarea's value
through the accessibility tree.

Every platform needs the host process to hold the input-synthesis permission
(Accessibility + Input Monitoring on macOS, XTest on X11 — no grant needed
on Windows). CI on macOS GitHub Actions runners typically lacks the grant,
so the harness sets XA11Y_SKIP_INPUT_SIM=1 there. That env var still works
and is still what the harness sets; pytest-xa11y is what reads it now, behind
the ``xa11y_requires("input_sim")`` marker below. Availability has to be
declared this way rather than probed because a missing grant is undetectable
on macOS: ``CGEventPost`` returns void, so the events are discarded with
every layer reporting success.

Hit-target and typed-text fields are identified by role + name (aria-label).
The event log is a read-only textarea; on macOS/Linux a11y trees it surfaces
as a `text_field` with the log text as its value.
"""

from __future__ import annotations

import os
import platform
import sys
import time

import pytest

import xa11y
from tests.harness.windows_input import input_snapshot

pytestmark = [
    pytest.mark.skipif(
        os.environ.get("XA11Y_TEST_APP") not in ("tauri", None),
        reason=(
            "input_sim tests only run against Tauri (one-per-platform strategy). "
            "Set XA11Y_TEST_APP=tauri, or leave it unset."
        ),
    ),
    pytest.mark.xa11y_requires("input_sim"),
]

HIT_TARGET = 'button[name="Hit target"]'
# UIA has no distinct multiline text role, so WebView2's <textarea> collapses to
# UIA_EditControlTypeId, which xa11y maps to `text_field`; WebKitGTK and
# WKWebView both expose it as `text_area`. Same collapse the egui entry in
# conftest.py records for AccessKit's MultilineTextInput. The role has to be in
# the selector rather than dropped: the enclosing <fieldset> takes its name from
# the <legend>, so a role-less `[name="Event log"]` matches two elements.
EVENT_LOG = (
    'text_field[name="Event log"]'
    if sys.platform == "win32"
    else 'text_area[name="Event log"]'
)
TYPED_FIELD = 'text_field[name="Typed text"]'
CLEAR_BUTTON = 'button[name="Clear log"]'

# Wait up to this long for a log line to appear after we post an event.
LOG_SETTLE_TIMEOUT = 2.0

# Wait up to this long for focus to actually land before synthesising input.
FOCUS_SETTLE_TIMEOUT = 5.0


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _read_log(app: xa11y.App) -> str:
    """Read the event log textarea value via a11y."""
    return app.locator(EVENT_LOG).element().value or ""


def _wait_for_log(app: xa11y.App, predicate, timeout: float = LOG_SETTLE_TIMEOUT) -> str:
    """Poll the log until `predicate(text)` returns True, or fail saying what it saw.

    Raises rather than returning the last value. Returning it produced
    `assert 'keydown' in ''` — an assertion that reports the symptom and hides
    both the wait and what the page actually held. The traceback still names
    the predicate, because it points at the calling line.
    """
    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        last = _read_log(app)
        if predicate(last):
            return last
        time.sleep(0.05)
    raise AssertionError(
        f"Event log never matched within {timeout:.1f}s.\n"
        f"  log was: {last!r}\n"
        f"  an empty log means the synthesised input never reached the webview "
        f"at all, rather than arriving in the wrong shape."
    )


def _clear_log(app: xa11y.App) -> None:
    app.locator(CLEAR_BUTTON).press()
    # The press itself is a synthetic a11y action (not input sim), so the
    # log should be empty immediately — but give one tick for the webview
    # handler to run. Best-effort on purpose: a stale line here is not worth
    # failing the test that has not run yet, and every caller asserts on the
    # content it goes on to produce.
    deadline = time.monotonic() + 0.5
    while time.monotonic() < deadline:
        if _read_log(app) == "":
            return
        time.sleep(0.05)


def _line(log: str, kind: str) -> str:
    """Most recent log line of `kind` — the page prepends, newest first."""
    for line in log.split("\n"):
        if line.startswith(kind + " "):
            return line
    return ""


def _field(line: str, key: str) -> str:
    """Pull `key=value` out of one log line (`click x=12 y=8 button=left ...`)."""
    for part in line.split():
        name, _, value = part.partition("=")
        if name == key:
            return value
    return ""


def _command_chord_seen(log: str, expected_key: str) -> bool:
    """Whether the command-key chord's own events are in `log`.

    Require the chord's tapped key plus the command modifier. A predicate
    that accepts any key line can be satisfied by the stale
    `keyup key=a ... mods=-` line that `type_text` leaves in flight, which
    returned the wait before the chord had been delivered. The modifier is
    `mods=meta` where the platform routes it into `metaKey`, and the
    `Super` key name where WebKit-GTK does not.
    """
    lines = log.split("\n")
    tapped = [
        line
        for line in lines
        if line.startswith(("keydown ", "keyup "))
        and _field(line, "key") == expected_key
    ]
    return bool(tapped) and (
        any("meta" in _field(line, "mods").split(",") for line in tapped)
        or any(_field(line, "key") == "Super" for line in lines)
    )


def _focus_settled(app: xa11y.App, selector: str) -> None:
    """Give `selector` keyboard focus and wait until the platform agrees.

    `focus()` auto-waits for the target to be visible and enabled and then
    issues the focus action; it does not wait for focus to *land*, and under
    WebView2 a UIA SetFocus does not reliably move the webview's keyboard
    focus at all. Synthesised keystrokes go to whatever holds keyboard focus
    at the moment they are posted, so a test that types immediately after
    `focus()` can have its keystrokes delivered to the previous holder and
    dropped — the symptom is an empty event log.

    A real pointer click is what claims keyboard focus: it is delivered to
    the element rather than to a stale holder, and it settles the webview's
    focus transition before the first synthetic key. `wait_focused` stays
    because it verifies the platform saw the focus move; the click follows
    it so the two cannot race each other.
    """
    locator = app.locator(selector)
    locator.focus()
    locator.wait_focused(timeout=FOCUS_SETTLE_TIMEOUT)
    rect = locator.element().bounds
    assert rect is not None, f"{selector} has no bounds to click for keyboard focus"
    xa11y.input_sim().click((rect.x + rect.width // 2, rect.y + rect.height // 2))


def _focus_hit_target(app: xa11y.App) -> None:
    """Focus the hit target so keyboard events land on a meaningful element."""
    _focus_settled(app, HIT_TARGET)


def _focus_typed_field(app: xa11y.App) -> None:
    _focus_settled(app, TYPED_FIELD)


def _hit_center(app: xa11y.App) -> tuple[int, int]:
    """Screen-coordinate center of the hit-target region."""
    el = app.locator(HIT_TARGET).element()
    r = el.bounds
    assert r is not None, "hit target has no bounds"
    return (r.x + r.width // 2, r.y + r.height // 2)


# ---------------------------------------------------------------------------
# Fixture
# ---------------------------------------------------------------------------


@pytest.fixture
def sim() -> xa11y.InputSim:
    return xa11y.input_sim()


# ---------------------------------------------------------------------------
# Mouse
# ---------------------------------------------------------------------------


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


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="CGEvent drag/scroll do not reliably generate DOM mousedown/wheel events in WKWebView on macOS",
    strict=False,
)
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


# ---------------------------------------------------------------------------
# Mouse: click/drag options (core's ClickOptions / DragOptions)
# ---------------------------------------------------------------------------


def test_click_with_button_option(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    sim.click(_hit_center(tauri_input_app), button="right")
    log = _wait_for_log(tauri_input_app, lambda t: "button=right" in t)
    assert "button=right" in log


def test_click_with_count_option_reports_dblclick(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    sim.click(_hit_center(tauri_input_app), count=2)
    log = _wait_for_log(tauri_input_app, lambda t: "dblclick" in t)
    assert "dblclick" in log, f"expected dblclick in log, got:\n{log}"


def test_click_with_held_modifier(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    sim.click(_hit_center(tauri_input_app), held=["Shift"])
    log = _wait_for_log(tauri_input_app, lambda t: "click " in t)
    line = _line(log, "click")
    assert "shift" in _field(line, "mods"), f"expected shift held, got:\n{log}"


def test_click_anchor_offsets_from_element_top_left(tauri_input_app, sim):
    """`anchor` lands the click inside the element, not at its centre.

    The page logs `offsetX`/`offsetY` relative to the hit target, so an
    8px offset anchor must report coordinates well inside the top-left
    quadrant — which a centred click never would.
    """
    el = tauri_input_app.locator(HIT_TARGET).element()
    r = el.bounds
    assert r is not None, "hit target has no bounds"
    _clear_log(tauri_input_app)
    sim.click(el, anchor=(8, 8))
    log = _wait_for_log(tauri_input_app, lambda t: "click " in t)
    line = _line(log, "click")
    x, y = int(_field(line, "x")), int(_field(line, "y"))
    assert x < r.width // 2 and y < r.height // 2, (
        f"anchored click landed at ({x}, {y}) in a {r.width}x{r.height} target:\n{log}"
    )


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="CGEvent drag/scroll do not reliably generate DOM mousedown/wheel events in WKWebView on macOS",
    strict=False,
)
def test_mouse_down_and_up_report_press_and_release(tauri_input_app, sim):
    """The half-press primitives, without a composite click wrapping them."""
    _clear_log(tauri_input_app)
    sim.move_to(_hit_center(tauri_input_app))
    sim.mouse_down()
    sim.mouse_up()
    log = _wait_for_log(tauri_input_app, lambda t: "mouseup" in t and "mousedown" in t)
    assert "mousedown" in log
    assert "mouseup" in log


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="CGEvent drag/scroll do not reliably generate DOM mousedown/wheel events in WKWebView on macOS",
    strict=False,
)
def test_drag_with_duration_option(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    el = tauri_input_app.locator(HIT_TARGET).element()
    r = el.bounds
    assert r is not None
    start = (r.x + 20, r.y + 20)
    end = (r.x + r.width - 20, r.y + r.height - 20)
    sim.drag(start, end, duration=0.3)
    log = _wait_for_log(tauri_input_app, lambda t: "mouseup" in t and "mousemove" in t)
    assert "mousemove" in log


# ---------------------------------------------------------------------------
# Argument validation
#
# Each of these fails while parsing arguments, before any OS event is posted,
# so they need the sim but not the app.
# ---------------------------------------------------------------------------


def test_unknown_mouse_button_rejected(sim):
    with pytest.raises(ValueError, match="Unknown mouse button"):
        sim.mouse_down(button="scroll")


def test_unknown_anchor_rejected(sim):
    with pytest.raises(ValueError, match="Unknown anchor"):
        sim.click((0, 0), anchor="middle_left")


def test_negative_drag_duration_rejected(sim):
    with pytest.raises(ValueError, match="non-negative"):
        sim.drag((0, 0), (10, 10), duration=-1.0)


# ---------------------------------------------------------------------------
# Keyboard
# ---------------------------------------------------------------------------


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


def test_key_down_and_up_report_keydown_keyup(tauri_input_app, sim):
    """The half-press primitives, without `press` wrapping them."""
    _clear_log(tauri_input_app)
    _focus_hit_target(tauri_input_app)
    sim.key_down("a")
    sim.key_up("a")
    log = _wait_for_log(tauri_input_app, lambda t: "keyup" in t and "keydown" in t)
    assert "keydown" in log
    assert "keyup" in log
    assert "key=a" in log


def test_key_down_held_across_a_click(tauri_input_app, sim):
    """What the primitives exist for: holding a key across another action.

    Neither `press` (tap) nor `chord` (hold around one tap) can express this.
    """
    _clear_log(tauri_input_app)
    sim.key_down("Shift")
    try:
        sim.click(_hit_center(tauri_input_app))
    finally:
        sim.key_up("Shift")
    log = _wait_for_log(tauri_input_app, lambda t: "click " in t)
    line = _line(log, "click")
    assert "shift" in _field(line, "mods"), f"expected shift still held, got:\n{log}"


def test_chord_reports_modifier(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    _focus_hit_target(tauri_input_app)
    # Shift+a (capital A via the explicit-shift API contract).
    sim.chord("a", ["Shift"])
    log = _wait_for_log(tauri_input_app, lambda t: "keyup" in t and "shift" in t)
    assert "mods=shift" in log or "shift" in log.split("\n")[0]


def test_platform_meta_chord(tauri_input_app, sim):
    """Cmd/Win/Super held across a tap should fire the platform's 'command' key.

    The browser surfaces this differently per platform:
      - macOS: Cmd → KeyboardEvent.metaKey (`mods=meta`)
      - Windows: Win → KeyboardEvent.metaKey (`mods=meta`)
      - Linux: Super → KeyboardEvent.key == 'Super' (WebKit-GTK doesn't
        route Super into the metaKey flag, so we check for the key name
        on the keydown/keyup events instead).

    Windows uses F13: Win+semicolon opens the system emoji panel, which can
    interfere with this test and subsequent pointer tests. macOS and Linux
    retain semicolon. Each platform must report the chosen tap and modifier.
    """
    _clear_log(tauri_input_app)
    _focus_typed_field(tauri_input_app)
    sim.type_text("hello")
    tap = "F13" if sys.platform == "win32" else ";"
    # No second clear: the predicate requires the chord's own tap events, so
    # the type_text lines still in flight cannot satisfy the wait, and the
    # Clear press would move focus off the field right before the chord.
    sim.chord(tap, ["Meta"])
    log = _wait_for_log(tauri_input_app, lambda text: _command_chord_seen(text, tap))
    assert "meta" in log or "Super" in log or "Meta" in log, (
        f"expected platform command modifier in log, got:\n{log}"
    )


# ---------------------------------------------------------------------------
# Typing
# ---------------------------------------------------------------------------


def test_type_text_writes_to_focused_input(tauri_input_app, sim):
    # `_focus_typed_field` ends with a real pointer click, but the click moves
    # DOM focus asynchronously: on Linux the first character can be delivered
    # to the window while the field is still transitioning and not inserted,
    # leaving the value missing exactly its first character. Retry the whole
    # focus-and-type with focus already settled rather than asserting on a
    # half-delivered string. The Clear press resets the typed field too.
    val = ""
    for _ in range(3):
        _clear_log(tauri_input_app)
        _focus_typed_field(tauri_input_app)
        sim.type_text("hello xa11y")
        # Poll the typed-text input's value (not the event log) — type_text
        # uses Unicode / scancode paths that don't always generate synthetic
        # key events at the DOM level.
        deadline = time.monotonic() + LOG_SETTLE_TIMEOUT
        while time.monotonic() < deadline:
            val = tauri_input_app.locator(TYPED_FIELD).element().value or ""
            if val == "hello xa11y":
                return
            time.sleep(0.05)
    pytest.fail(f"typed-text field did not receive expected text, got: {val!r}")


@pytest.mark.skipif(
    not sys.platform.startswith("linux"),
    reason="Linux X11/uinput line-break mapping (#498); other platforms cover it in their own PRs",
)
def test_type_text_line_breaks_press_enter(tauri_input_app, sim):
    """Each line break — CRLF, CR or LF — types exactly one Enter (#498)."""
    _clear_log(tauri_input_app)
    _focus_hit_target(tauri_input_app)
    sim.type_text("a\r\nb\rc\nd")

    def enters(text: str) -> int:
        return sum(1 for line in text.split("\n") if line.startswith("keyup key=Enter"))

    log = _wait_for_log(tauri_input_app, lambda t: "keyup key=d" in t and enters(t) >= 3)
    assert enters(log) == 3, f"expected 3 Enter presses, log:\n{log}"


# ---------------------------------------------------------------------------
# Scroll
# ---------------------------------------------------------------------------


@pytest.mark.xfail(
    sys.platform == "darwin",
    reason="CGEvent drag/scroll do not reliably generate DOM mousedown/wheel events in WKWebView on macOS",
    strict=False,
)
def test_scroll_reports_wheel(tauri_input_app, sim):
    _clear_log(tauri_input_app)
    target = _hit_center(tauri_input_app)
    sim.scroll(target, dx=0, dy=3)
    posted_state = input_snapshot() if sys.platform == "win32" else None
    try:
        log = _wait_for_log(tauri_input_app, lambda t: "wheel" in t)
    except AssertionError as exc:
        if posted_state is None:
            raise
        raise AssertionError(
            f"{exc}\n"
            f"  scroll target={target}, app pid={tauri_input_app.pid}\n"
            f"  immediately after posting: {posted_state}\n"
            f"  after log timeout: {input_snapshot()}\n"
            f"  routing samples are observations, not proof of event delivery"
        ) from exc
    assert "wheel" in log


# ---------------------------------------------------------------------------
# Smoke
# ---------------------------------------------------------------------------


def test_input_sim_construct():
    """Constructing the sim should succeed on every supported platform."""
    sim = xa11y.input_sim()
    assert sim is not None


def test_platform_matrix_smoke():
    """Sanity: we know which platform this runs on."""
    assert platform.system() in ("Darwin", "Linux", "Windows")
