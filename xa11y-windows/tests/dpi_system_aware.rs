//! A system-aware host process (issue #506).
//!
//! The tests pin their own process to `SYSTEM_AWARE` before xa11y loads, the
//! way an embedding host that fixed its awareness would. xa11y's process-level
//! request is then refused, and every coordinate call must switch its thread
//! to Per-Monitor V2 for the call (`DpiScope`) and restore it afterwards.
//!
//! What they assert:
//! - after every provider, capture and input call the host thread is still
//!   system aware (the scope restored it);
//! - the call switched the thread at least once (`__dpi_scope_switches`). On
//!   a single 96-DPI monitor, the CI runner's hardware, scaled and physical
//!   coordinates coincide, so this counter is what proves the scope ran;
//! - the coordinates match physical values read under Per-Monitor V2. That
//!   comparison can only fail on a mixed-DPI desktop.
//!
//! They are also the runtime layer of the DPI-scope enforcement (see
//! `src/coords.rs`): this is a debug build, so every coordinate-bearing
//! wrapper `debug_assert!`s that its thread is per-monitor, and here the
//! thread starts out system aware.
//!
//! `#[ignore]`d: they need a live desktop with the AccessKit test app running,
//! so `scripts/run_integ_tests_windows.ps1` runs them with `--ignored` while
//! that app is up. One process for the binary, pinned once.

#![cfg(target_os = "windows")]

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::HiDpi::{
    GetAwarenessFromDpiAwarenessContext, GetThreadDpiAwarenessContext,
    SetProcessDpiAwarenessContext, SetThreadDpiAwarenessContext, DPI_AWARENESS,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DPI_AWARENESS_CONTEXT_SYSTEM_AWARE,
    DPI_AWARENESS_SYSTEM_AWARE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetPhysicalCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
};
use xa11y_core::input::{InputProvider, Point};
use xa11y_core::{ElementData, Error, Provider, ScreenshotProvider};
use xa11y_windows::{
    __dpi_scope_switches, WindowsInputProvider, WindowsProvider, WindowsScreenshot,
};

/// The AccessKit test app's process name, as the synthesized app node reports it.
const TEST_APP: &str = "xa11y-test-app";

fn thread_awareness() -> DPI_AWARENESS {
    unsafe { GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext()) }
}

/// Pin the process to SYSTEM_AWARE, once, before any xa11y call. Threads the
/// test harness starts get the process default.
fn pin_system_aware() {
    static PIN: std::sync::Once = std::sync::Once::new();
    PIN.call_once(|| {
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE) }
            .expect("the test process pins SYSTEM_AWARE before xa11y loads");
    });
    assert_eq!(thread_awareness(), DPI_AWARENESS_SYSTEM_AWARE);
}

/// Run `call`, then assert it switched this thread into a scope and back.
#[track_caller]
fn scoped<T>(what: &str, call: impl FnOnce() -> T) -> T {
    let before = __dpi_scope_switches();
    let out = call();
    assert!(
        __dpi_scope_switches() > before,
        "{what} did not enter a DpiScope on a system-aware thread"
    );
    assert_eq!(
        thread_awareness(),
        DPI_AWARENESS_SYSTEM_AWARE,
        "{what} did not restore the host thread's awareness"
    );
    out
}

/// Run `f` with this thread temporarily Per-Monitor V2: the test's own
/// reference measurement of physical coordinates.
fn physical<T>(f: impl FnOnce() -> T) -> T {
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    assert!(!previous.0.is_null(), "the test can switch its own thread");
    let out = f();
    unsafe { SetThreadDpiAwarenessContext(previous) };
    out
}

/// The AccessKit test app's top-level window, read with every call checked
/// by [`scoped`].
fn test_app_window(provider: &WindowsProvider) -> ElementData {
    find_test_app_window(provider, |what, call| scoped(what, call))
}

/// The same read with no scope checks, for the Per-Monitor V2 reference
/// measurement (a PMv2 thread needs no switch, so `scoped` would fail there).
fn test_app_window_unchecked(provider: &WindowsProvider) -> ElementData {
    find_test_app_window(provider, |_, call| call())
}

fn find_test_app_window(
    provider: &WindowsProvider,
    run: impl Fn(
        &str,
        &mut dyn FnMut() -> xa11y_core::Result<Vec<ElementData>>,
    ) -> xa11y_core::Result<Vec<ElementData>>,
) -> ElementData {
    let apps = run("get_children(None)", &mut || provider.get_children(None)).expect("list apps");
    let app = apps
        .iter()
        .find(|a| a.name.as_deref() == Some(TEST_APP))
        .unwrap_or_else(|| {
            panic!(
                "the {TEST_APP} app is not running; run this through \
                 scripts/run_integ_tests_windows.ps1"
            )
        });
    let windows = match run("app_windows", &mut || provider.app_windows(app)) {
        Ok(windows) => windows,
        Err(Error::ElementStale { selector }) => {
            panic!("{TEST_APP} exited during the test: {selector}")
        }
        Err(e) => panic!("reading {TEST_APP}'s windows failed: {e}"),
    };
    windows
        .into_iter()
        .find(|w| {
            w.bounds.is_some()
                && w.stable_id
                    .as_deref()
                    .is_some_and(|s| s.starts_with("hwnd:"))
        })
        .expect("the test app has an HWND window with bounds")
}

fn centre(bounds: xa11y_core::Rect) -> Point {
    Point::new(
        bounds.x + bounds.width as i32 / 2,
        bounds.y + bounds.height as i32 / 2,
    )
}

#[test]
#[ignore = "needs a live desktop with the AccessKit test app; run via scripts/run_integ_tests_windows.ps1"]
fn a_system_aware_host_reads_and_captures_through_a_scope() {
    pin_system_aware();
    let provider = WindowsProvider::new().expect("provider on a system-aware host");
    let screenshot = WindowsScreenshot::new().expect("screenshot on a system-aware host");
    assert_eq!(
        thread_awareness(),
        DPI_AWARENESS_SYSTEM_AWARE,
        "construction must not change the host thread's awareness"
    );

    // Tree read. Mixed-DPI only: the bounds equal the same read made on a
    // Per-Monitor V2 thread. (Not GetWindowRect: UIA reports the visible
    // frame, without the invisible resize borders GetWindowRect includes.)
    let window = test_app_window(&provider);
    let bounds = window.bounds.expect("bounds");
    let reference = physical(|| test_app_window_unchecked(&provider));
    assert_eq!(
        reference.stable_id, window.stable_id,
        "the reference read found a different window"
    );
    assert_eq!(
        Some(bounds),
        reference.bounds,
        "the test app window's bounds must be physical desktop pixels"
    );

    // Capture. Mixed-DPI only: the size is the PMv2 virtual screen.
    let (shot, _origin) = scoped("capture_full", || screenshot.capture_full())
        .expect("capture on a system-aware host");
    let (w, h) = physical(|| unsafe {
        (
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    });
    assert_eq!((shot.width as i32, shot.height as i32), (w, h));

    // The mapping re-validates the display layout when used, on this
    // (system-aware) thread, so that read must be scoped too.
    scoped("desktop_to_image", || shot.desktop_to_image(centre(bounds)))
        .expect("the window centre maps into the capture");
}

#[test]
#[ignore = "needs a live desktop with the AccessKit test app; run via scripts/run_integ_tests_windows.ps1"]
fn a_system_aware_host_moves_the_pointer_through_a_scope() {
    pin_system_aware();
    let provider = WindowsProvider::new().expect("provider on a system-aware host");
    let input = WindowsInputProvider::new().expect("input on a system-aware host");

    // Aim at the test app's window centre: a real point on a real display.
    let target = centre(test_app_window(&provider).bounds.expect("bounds"));
    scoped("pointer_move", || input.pointer_move(target)).expect("pointer_move");

    // Mixed-DPI only: the pointer is at the physical target.
    let mut now = POINT::default();
    unsafe { GetPhysicalCursorPos(&mut now) }.expect("cursor position");
    assert!(
        (now.x - target.x).abs() <= 1 && (now.y - target.y).abs() <= 1,
        "pointer landed at ({}, {}), not the physical target ({}, {})",
        now.x,
        now.y,
        target.x,
        target.y
    );
}
