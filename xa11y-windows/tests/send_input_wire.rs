//! Wire-level tests for the Windows `SendInput` backend.
//!
//! Drives [`xa11y_windows::WindowsInputProvider`] through its public
//! [`InputProvider`] API and reads the resulting events back off the desktop
//! input queue with `WH_KEYBOARD_LL` / `WH_MOUSE_LL` hooks, asserting the
//! virtual-key codes, extended/injected flags, wheel deltas and absolute
//! pointer coordinates that actually reached the wire. It is the Windows
//! counterpart of `xa11y-linux/tests/wayland_input_e2e.rs`, which does the
//! same job through `libevdev`, and it covers the backend without a webview
//! or any other application in the loop (issue #348).
//!
//! The hooks **swallow** every injected event (they return a non-zero
//! `LRESULT` instead of calling the next hook), so nothing this file posts
//! ever reaches a window, a context menu, or the desktop. Events the hook did
//! not originate — a developer's real keyboard and mouse — are passed straight
//! through, so running the suite locally does not lock the machine out.
//!
//! Marked `#[ignore]` so `cargo test --workspace` never injects input as a
//! side effect of a plain unit-test run. CI runs them with `--ignored
//! --test-threads=1` in the `Windows (SendInput)` job.

#![cfg(target_os = "windows")]

use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_7, VK_A, VK_BACK, VK_CONTROL, VK_DELETE, VK_END, VK_ESCAPE, VK_F24, VK_F5,
    VK_HOME, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_NEXT, VK_OEM_1,
    VK_OEM_PERIOD, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP, VK_Z,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use xa11y_core::input::{InputProvider, Key, MouseButton, Point, ScrollDelta};
use xa11y_core::Error;
use xa11y_windows::WindowsInputProvider;

/// One notch of a physical mouse wheel — Win32's `WHEEL_DELTA`.
const WHEEL_DELTA: i32 = 120;

/// A point comfortably inside the primary monitor, used as the target for
/// every pointer test. Far enough from every edge that the absolute
/// normalisation round-trip has room on both sides, and away from the screen
/// corners Windows treats as hot zones.
const TARGET: Point = Point::new(300, 200);

/// How long to wait for injected events to come back through the hook.
const SETTLE: Duration = Duration::from_millis(400);

// ---------------------------------------------------------------------------
// Wire log
// ---------------------------------------------------------------------------

/// One event as the low-level hook saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wire {
    Key {
        message: u32,
        vk: u32,
        scan: u32,
        flags: u32,
    },
    Mouse {
        message: u32,
        x: i32,
        y: i32,
        mouse_data: u32,
        flags: u32,
    },
}

impl Wire {
    fn message(self) -> u32 {
        match self {
            Wire::Key { message, .. } | Wire::Mouse { message, .. } => message,
        }
    }
}

fn wire_log() -> &'static Mutex<Vec<Wire>> {
    static LOG: OnceLock<Mutex<Vec<Wire>>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(Vec::new()))
}

fn record(event: Wire) {
    wire_log()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(event);
}

fn snapshot() -> Vec<Wire> {
    wire_log().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn clear() {
    wire_log().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

// `unsafe extern "system" fn` bodies are implicitly unsafe on edition 2021, so
// the raw-pointer reads below need no inner `unsafe` block.

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION the system documents lparam as a pointer to a
        // KBDLLHOOKSTRUCT that stays valid for the duration of the callback.
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if info.flags.contains(LLKHF_INJECTED) {
            record(Wire::Key {
                message: wparam.0 as u32,
                vk: info.vkCode,
                scan: info.scanCode,
                flags: info.flags.0,
            });
            // Swallow it: the assertion is about what SendInput put on the
            // wire, and nothing downstream should see synthetic input from a
            // test run. Real (non-injected) events fall through below.
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: as above, for MSLLHOOKSTRUCT.
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        if info.flags & LLMHF_INJECTED != 0 {
            record(Wire::Mouse {
                message: wparam.0 as u32,
                x: info.pt.x,
                y: info.pt.y,
                mouse_data: info.mouseData,
                flags: info.flags,
            });
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// Install the hooks on a dedicated pumping thread, once per process.
///
/// Low-level hooks are invoked on the thread that installed them, and only
/// while that thread is retrieving messages, so they need a thread of their
/// own: `SendInput` blocks until the hook chain returns, and calling it from
/// the pumping thread itself would re-enter. The hooks stay installed for the
/// lifetime of the test binary (tearing them down between tests would race
/// with events still in flight); Windows removes them when the process exits.
fn ensure_hooks() {
    static HOOKS: OnceLock<()> = OnceLock::new();
    HOOKS.get_or_init(|| {
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        thread::spawn(move || {
            // SAFETY: both hook procs are `fn` items with the required
            // signature and 'static lifetime. A null module handle plus thread
            // id 0 is the documented form for the low-level hook IDs.
            let (keyboard, mouse) = unsafe {
                let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0)
                    .expect("SetWindowsHookExW(WH_KEYBOARD_LL) failed");
                let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0)
                    .expect("SetWindowsHookExW(WH_MOUSE_LL) failed");
                (keyboard, mouse)
            };
            ready_tx
                .send(())
                .expect("hook thread could not report readiness");

            let mut msg = MSG::default();
            // GetMessageW returns 0 on WM_QUIT and -1 on error; both end the
            // pump. Nothing posts WM_QUIT here, so in practice this runs until
            // the process exits.
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {}

            unsafe {
                UnhookWindowsHookEx(keyboard).expect("UnhookWindowsHookEx(keyboard) failed");
                UnhookWindowsHookEx(mouse).expect("UnhookWindowsHookEx(mouse) failed");
            }
        });
        ready_rx
            .recv()
            .expect("hook thread died before installing its hooks");
    });
}

// ---------------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------------

/// Serialise tests against the shared hook log and hand back a clean slate.
///
/// CI runs with `--test-threads=1`, but the lock means a developer running the
/// file without that flag gets the same isolation rather than an interleaved
/// log and a confusing failure.
fn capture() -> MutexGuard<'static, ()> {
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    ensure_hooks();
    clear();
    guard
}

fn provider() -> WindowsInputProvider {
    WindowsInputProvider::new().expect("WindowsInputProvider::new failed")
}

/// Poll the log until it holds at least `count` events, or `SETTLE` elapses.
fn wait_for(count: usize) -> Vec<Wire> {
    let deadline = Instant::now() + SETTLE;
    loop {
        let events = snapshot();
        if events.len() >= count || Instant::now() >= deadline {
            return events;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Wait out `SETTLE` and return whatever arrived — for asserting *nothing* did.
fn settled() -> Vec<Wire> {
    thread::sleep(SETTLE);
    snapshot()
}

#[track_caller]
fn expect_key(event: Wire) -> (u32, u32, u32, u32) {
    match event {
        Wire::Key {
            message,
            vk,
            scan,
            flags,
        } => (message, vk, scan, flags),
        other => panic!("expected a keyboard event, got {other:?}"),
    }
}

#[track_caller]
fn expect_mouse(event: Wire) -> (u32, i32, i32, u32, u32) {
    match event {
        Wire::Mouse {
            message,
            x,
            y,
            mouse_data,
            flags,
        } => (message, x, y, mouse_data, flags),
        other => panic!("expected a mouse event, got {other:?}"),
    }
}

/// The virtual-key codes a low-level hook may report for a key sent as `vk`.
///
/// The backend deliberately sends the side-agnostic modifier codes — `Key::Shift`
/// means "a shift key", not "the left one" — but Windows resolves those to their
/// left-hand counterparts before the hook sees them (observed on windows-latest:
/// `VK_SHIFT` 0x10 arrives as `VK_LSHIFT` 0xA0, with `scanCode` still 0). Both
/// spellings name the same key, and which side the system picks is not something
/// the backend chooses or promises, so either is accepted. Every other code
/// arrives unchanged.
fn accepted_vks(vk: VIRTUAL_KEY) -> [u32; 2] {
    let sided = if vk == VK_SHIFT {
        VK_LSHIFT
    } else if vk == VK_CONTROL {
        VK_LCONTROL
    } else if vk == VK_MENU {
        VK_LMENU
    } else {
        vk
    };
    [u32::from(vk.0), u32::from(sided.0)]
}

/// Alt-modified keys arrive as `WM_SYSKEY*` rather than `WM_KEY*`.
fn is_key_down(message: u32) -> bool {
    message == WM_KEYDOWN || message == WM_SYSKEYDOWN
}

fn is_key_up(message: u32) -> bool {
    message == WM_KEYUP || message == WM_SYSKEYUP
}

/// The signed notch count carried in the high word of `mouseData`.
fn wheel_delta(mouse_data: u32) -> i32 {
    i32::from(((mouse_data >> 16) as u16) as i16)
}

/// System DPI scale. The backend converts logical points to physical ones with
/// the DPI of the monitor under the point; on the single-monitor runners this
/// job targets, that is the system DPI.
fn system_scale() -> f64 {
    // SAFETY: GetDpiForSystem takes no arguments and cannot fail.
    f64::from(unsafe { GetDpiForSystem() }) / 96.0
}

/// Press and release one key, returning the two events it produced.
#[track_caller]
fn tap(sim: &WindowsInputProvider, key: &Key) -> Vec<Wire> {
    clear();
    sim.key_down(key).expect("key_down failed");
    sim.key_up(key).expect("key_up failed");
    wait_for(2)
}

// ---------------------------------------------------------------------------
// Keyboard
// ---------------------------------------------------------------------------

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn key_down_and_up_report_the_virtual_key() {
    let _guard = capture();
    let events = tap(&provider(), &Key::Char('a'));

    assert_eq!(events.len(), 2, "expected one down and one up: {events:?}");
    let (message, vk, _, flags) = expect_key(events[0]);
    assert!(is_key_down(message), "{events:?}");
    assert_eq!(vk, u32::from(VK_A.0));
    assert!(
        flags & LLKHF_INJECTED.0 != 0,
        "SendInput events must be marked injected: {events:?}"
    );

    let (message, vk, _, _) = expect_key(events[1]);
    assert!(is_key_up(message), "{events:?}");
    assert_eq!(vk, u32::from(VK_A.0));
}

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn named_keys_map_to_their_virtual_key_codes() {
    let _guard = capture();
    let sim = provider();

    let cases: &[(Key, VIRTUAL_KEY)] = &[
        (Key::Enter, VK_RETURN),
        (Key::Escape, VK_ESCAPE),
        (Key::Backspace, VK_BACK),
        (Key::Tab, VK_TAB),
        (Key::Space, VK_SPACE),
        (Key::Shift, VK_SHIFT),
        (Key::Ctrl, VK_CONTROL),
        (Key::Alt, VK_MENU),
        (Key::Meta, VK_LWIN),
        (Key::F(5), VK_F5),
        (Key::F(24), VK_F24),
        (Key::Char('z'), VK_Z),
        (Key::Char('7'), VK_7),
        (Key::Char(';'), VK_OEM_1),
        (Key::Char('.'), VK_OEM_PERIOD),
    ];

    for (key, expected) in cases {
        let events = tap(&sim, key);
        assert_eq!(events.len(), 2, "{key:?} produced {events:?}");
        let accepted = accepted_vks(*expected);
        let (down, vk, _, _) = expect_key(events[0]);
        assert!(is_key_down(down), "{key:?} produced {events:?}");
        assert!(
            accepted.contains(&vk),
            "{key:?} should report one of {accepted:?}, produced {events:?}"
        );
        let (up, _, _, _) = expect_key(events[1]);
        assert!(is_key_up(up), "{key:?} produced {events:?}");
    }
}

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn navigation_keys_are_flagged_extended() {
    let _guard = capture();
    let sim = provider();

    // Keys `vk_for` marks extended, and keys it deliberately does not — the
    // flag is what distinguishes the navigation cluster from the numeric
    // keypad, so getting it backwards is silently wrong rather than broken.
    let extended: &[(Key, VIRTUAL_KEY)] = &[
        (Key::ArrowUp, VK_UP),
        (Key::ArrowLeft, VK_LEFT),
        (Key::ArrowRight, VK_RIGHT),
        (Key::Home, VK_HOME),
        (Key::End, VK_END),
        (Key::PageUp, VK_PRIOR),
        (Key::PageDown, VK_NEXT),
        (Key::Delete, VK_DELETE),
    ];
    for (key, expected) in extended {
        let events = tap(&sim, key);
        assert_eq!(events.len(), 2, "{key:?} produced {events:?}");
        for event in &events {
            let (_, vk, _, flags) = expect_key(*event);
            assert_eq!(vk, u32::from(expected.0), "{key:?} produced {events:?}");
            assert!(
                flags & LLKHF_EXTENDED.0 != 0,
                "{key:?} should be flagged extended: {events:?}"
            );
        }
    }

    for key in [Key::Char('a'), Key::Shift, Key::Enter, Key::Space] {
        let events = tap(&sim, &key);
        for event in &events {
            let (_, _, _, flags) = expect_key(*event);
            assert!(
                flags & LLKHF_EXTENDED.0 == 0,
                "{key:?} should not be flagged extended: {events:?}"
            );
        }
    }
}

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn unmappable_char_is_rejected_before_any_event_is_posted() {
    let _guard = capture();
    let err = provider()
        .key_down(&Key::Char('€'))
        .expect_err("Key::Char('€') has no virtual-key mapping and must be rejected");
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    assert!(
        settled().is_empty(),
        "a rejected key must not reach the wire: {:?}",
        snapshot()
    );
}

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn function_key_out_of_range_is_rejected_before_any_event_is_posted() {
    let _guard = capture();
    let err = provider()
        .key_down(&Key::F(25))
        .expect_err("F25 is out of the 1..=24 range and must be rejected");
    assert!(matches!(err, Error::InvalidActionData { .. }), "{err:?}");
    assert!(
        settled().is_empty(),
        "a rejected key must not reach the wire: {:?}",
        snapshot()
    );
}

// ---------------------------------------------------------------------------
// Typing
// ---------------------------------------------------------------------------

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn type_text_sends_one_unicode_pair_per_utf16_unit() {
    let _guard = capture();
    // Non-ASCII plus an astral character: neither has a virtual-key mapping,
    // and the emoji is a surrogate pair, so this only passes if the backend
    // really is going through KEYEVENTF_UNICODE over UTF-16 code units.
    let text = "hé😀";
    let units: Vec<u16> = text.encode_utf16().collect();
    assert_eq!(units.len(), 4, "fixture should be 4 UTF-16 units");

    provider().type_text(text).expect("type_text failed");
    let events = wait_for(units.len() * 2);
    assert_eq!(
        events.len(),
        units.len() * 2,
        "expected a down/up pair per code unit: {events:?}"
    );

    let mut sent = Vec::new();
    for (i, event) in events.iter().enumerate() {
        let (message, _, scan, _) = expect_key(*event);
        if i % 2 == 0 {
            assert!(is_key_down(message), "{events:?}");
            sent.push(scan as u16);
        } else {
            assert!(is_key_up(message), "{events:?}");
            assert_eq!(scan as u16, sent[i / 2], "{events:?}");
        }
    }
    assert_eq!(sent, units, "{events:?}");
}

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn type_text_of_empty_string_sends_nothing() {
    let _guard = capture();
    provider().type_text("").expect("type_text failed");
    assert!(settled().is_empty(), "{:?}", snapshot());
}

// ---------------------------------------------------------------------------
// Pointer
// ---------------------------------------------------------------------------

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn pointer_move_lands_on_the_requested_point() {
    let _guard = capture();
    provider()
        .pointer_move(TARGET)
        .expect("pointer_move failed");

    let events = wait_for(1);
    assert_eq!(events.len(), 1, "{events:?}");
    let (message, x, y, _, flags) = expect_mouse(events[0]);
    assert_eq!(message, WM_MOUSEMOVE, "{events:?}");
    assert!(
        flags & LLMHF_INJECTED != 0,
        "SendInput events must be marked injected: {events:?}"
    );

    // The backend normalises to the 0..=65535 virtual-desktop range and the
    // system maps it back to a pixel, so allow a pixel of rounding either way.
    let scale = system_scale();
    let expected_x = (f64::from(TARGET.x) * scale).round() as i32;
    let expected_y = (f64::from(TARGET.y) * scale).round() as i32;
    assert!(
        (x - expected_x).abs() <= 2 && (y - expected_y).abs() <= 2,
        "pointer landed at ({x}, {y}), expected ~({expected_x}, {expected_y}) \
         at scale {scale}: {events:?}"
    );
}

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn pointer_down_and_up_report_the_button() {
    let _guard = capture();
    let sim = provider();

    for (button, down, up) in [
        (MouseButton::Left, WM_LBUTTONDOWN, WM_LBUTTONUP),
        (MouseButton::Right, WM_RBUTTONDOWN, WM_RBUTTONUP),
        (MouseButton::Middle, WM_MBUTTONDOWN, WM_MBUTTONUP),
    ] {
        clear();
        sim.pointer_down(button).expect("pointer_down failed");
        sim.pointer_up(button).expect("pointer_up failed");
        let events = wait_for(2);
        assert_eq!(events.len(), 2, "{button:?} produced {events:?}");
        assert_eq!(events[0].message(), down, "{button:?} produced {events:?}");
        assert_eq!(events[1].message(), up, "{button:?} produced {events:?}");
    }
}

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn pointer_click_moves_then_presses_and_releases() {
    let _guard = capture();
    let sim = provider();

    for (button, down, up) in [
        (MouseButton::Left, WM_LBUTTONDOWN, WM_LBUTTONUP),
        (MouseButton::Right, WM_RBUTTONDOWN, WM_RBUTTONUP),
        (MouseButton::Middle, WM_MBUTTONDOWN, WM_MBUTTONUP),
    ] {
        clear();
        sim.pointer_click(TARGET, button, 1)
            .expect("pointer_click failed");
        let events = wait_for(3);
        let messages: Vec<u32> = events.iter().map(|e| e.message()).collect();
        assert_eq!(
            messages,
            vec![WM_MOUSEMOVE, down, up],
            "{button:?} produced {events:?}"
        );
    }
}

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn pointer_click_repeats_the_press_pair_per_count() {
    let _guard = capture();
    provider()
        .pointer_click(TARGET, MouseButton::Left, 2)
        .expect("pointer_click failed");

    let events = wait_for(5);
    let messages: Vec<u32> = events.iter().map(|e| e.message()).collect();
    assert_eq!(
        messages,
        vec![
            WM_MOUSEMOVE,
            WM_LBUTTONDOWN,
            WM_LBUTTONUP,
            WM_LBUTTONDOWN,
            WM_LBUTTONUP
        ],
        "{events:?}"
    );
}

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn pointer_click_with_zero_count_sends_nothing() {
    let _guard = capture();
    provider()
        .pointer_click(TARGET, MouseButton::Left, 0)
        .expect("pointer_click failed");
    assert!(
        settled().is_empty(),
        "a zero-count click must not even move the pointer: {:?}",
        snapshot()
    );
}

// ---------------------------------------------------------------------------
// Scroll
// ---------------------------------------------------------------------------

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn pointer_scroll_reports_wheel_notches() {
    let _guard = capture();
    let sim = provider();

    clear();
    sim.pointer_scroll(TARGET, ScrollDelta::new(0, 3))
        .expect("pointer_scroll failed");
    let events = wait_for(2);
    assert_eq!(
        events.iter().map(|e| e.message()).collect::<Vec<_>>(),
        vec![WM_MOUSEMOVE, WM_MOUSEWHEEL],
        "{events:?}"
    );
    let (_, _, _, mouse_data, _) = expect_mouse(events[1]);
    assert_eq!(wheel_delta(mouse_data), 3 * WHEEL_DELTA, "{events:?}");

    clear();
    sim.pointer_scroll(TARGET, ScrollDelta::new(-2, 0))
        .expect("pointer_scroll failed");
    let events = wait_for(2);
    assert_eq!(
        events.iter().map(|e| e.message()).collect::<Vec<_>>(),
        vec![WM_MOUSEMOVE, WM_MOUSEHWHEEL],
        "{events:?}"
    );
    let (_, _, _, mouse_data, _) = expect_mouse(events[1]);
    assert_eq!(wheel_delta(mouse_data), -2 * WHEEL_DELTA, "{events:?}");

    // Both axes at once: vertical first, then horizontal.
    clear();
    sim.pointer_scroll(TARGET, ScrollDelta::new(1, -1))
        .expect("pointer_scroll failed");
    let events = wait_for(3);
    assert_eq!(
        events.iter().map(|e| e.message()).collect::<Vec<_>>(),
        vec![WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_MOUSEHWHEEL],
        "{events:?}"
    );
    let (_, _, _, vertical, _) = expect_mouse(events[1]);
    let (_, _, _, horizontal, _) = expect_mouse(events[2]);
    assert_eq!(wheel_delta(vertical), -WHEEL_DELTA, "{events:?}");
    assert_eq!(wheel_delta(horizontal), WHEEL_DELTA, "{events:?}");
}

#[test]
#[ignore = "injects input; run via the Windows (SendInput) CI job"]
fn pointer_scroll_with_no_delta_only_moves() {
    let _guard = capture();
    provider()
        .pointer_scroll(TARGET, ScrollDelta::new(0, 0))
        .expect("pointer_scroll failed");
    let events = settled();
    assert_eq!(
        events.iter().map(|e| e.message()).collect::<Vec<_>>(),
        vec![WM_MOUSEMOVE],
        "{events:?}"
    );
}
