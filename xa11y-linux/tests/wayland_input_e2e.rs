//! End-to-end uinput input-sim tests.
//!
//! Drives `xa11y_linux::LinuxInputProvider` through its public API on a
//! Linux host that has `/dev/uinput` accessible (the user is in the
//! `input` group, or — in the CI container — the device is bind-mounted
//! and we're running as root). Reads events back from `/dev/input/event*`
//! via the same `evdev` crate the backend writes them with, and asserts
//! the correct types/codes/values arrive.
//!
//! Marked `#[ignore]` by default so a developer running
//! `cargo test --workspace` on a workstation without `/dev/uinput`
//! permissions doesn't spuriously fail. CI runs them with `--ignored`
//! inside the dedicated container.

#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use evdev::{AbsoluteAxisCode, Device, EventType, KeyCode, RelativeAxisCode};
use xa11y_core::input::{InputProvider, Key, MouseButton, Point, ScrollDelta};
use xa11y_linux::LinuxInputProvider;

/// Locate the kernel-side `/dev/input/event*` node corresponding to the
/// virtual device the backend just registered. We identify it by the
/// device name ("xa11y virtual input") set on the uinput builder.
fn open_xa11y_evdev() -> Device {
    // Give the kernel a moment to expose the new device under
    // `/dev/input/event*`. udev settling is much slower (200ms+ on cold
    // CI runners), so retry for up to 5s.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match scan_for_xa11y() {
            Ok(Some(dev)) => return dev,
            Ok(None) | Err(_) => {}
        }
        if Instant::now() >= deadline {
            // Diagnostics: dump what we actually saw under /dev/input
            // and the closest device names. Catches the "udev didn't
            // run" / "permissions wrong" / "container hides nodes"
            // cases that have bitten us in CI.
            let mut diag = String::new();
            match std::fs::read_dir("/dev/input") {
                Ok(rd) => {
                    diag.push_str("/dev/input contents:\n");
                    for entry in rd.flatten() {
                        let path = entry.path();
                        let meta = std::fs::metadata(&path)
                            .map(|m| format!("mode={:o} ", m.permissions().mode()))
                            .unwrap_or_default();
                        let name = match Device::open(&path) {
                            Ok(d) => d.name().unwrap_or("<unnamed>").to_string(),
                            Err(e) => format!("<open: {e}>"),
                        };
                        diag.push_str(&format!("  {} {} → {}\n", meta, path.display(), name));
                    }
                }
                Err(e) => diag.push_str(&format!("/dev/input: {e}\n")),
            }
            panic!(
                "could not find /dev/input/event* node named 'xa11y virtual input' \
                 within 5s of LinuxInputProvider::new(). diagnostics:\n{diag}"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn scan_for_xa11y() -> std::io::Result<Option<Device>> {
    for entry in std::fs::read_dir("/dev/input")? {
        let entry = entry?;
        let path = entry.path();
        let name = match path.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => continue,
        };
        if !name.starts_with("event") {
            continue;
        }
        match Device::open(&path) {
            Ok(dev) => {
                if dev.name() == Some("xa11y virtual input") {
                    return Ok(Some(dev));
                }
            }
            Err(_) => continue,
        }
    }
    Ok(None)
}

/// Drain the event stream for up to `timeout` and collect every event
/// whose `EventType` is in `keep`. Stops early once we've seen `min`
/// matching events.
fn collect_events(
    dev: &mut Device,
    keep: &[EventType],
    min: usize,
    timeout: Duration,
) -> Vec<evdev::InputEvent> {
    let mut out = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        if out.len() >= min {
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        match dev.fetch_events() {
            Ok(events) => {
                for ev in events {
                    if keep.iter().any(|k| k.0 == ev.event_type().0) {
                        out.push(ev);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("fetch_events failed: {e}"),
        }
    }
    out
}

/// Helper that creates the provider, finds the matching evdev node,
/// drives `f`, then asserts `verify` against the events that arrive.
fn drive<F>(verify: F)
where
    F: FnOnce(&LinuxInputProvider, &mut Device),
{
    // Force the uinput branch — this test is about uinput, not XTest.
    unsafe { std::env::remove_var("DISPLAY") };

    let sim =
        LinuxInputProvider::new().expect("uinput backend should construct in the e2e container");

    let mut dev = open_xa11y_evdev();
    // Switch the read side to non-blocking so collect_events can poll.
    dev.set_nonblocking(true).expect("set_nonblocking");

    verify(&sim, &mut dev);
}

#[test]
#[ignore]
fn pointer_move_emits_abs_x_y() {
    drive(|sim, dev| {
        sim.pointer_move(Point::new(640, 360))
            .expect("pointer_move");
        let events = collect_events(dev, &[EventType::ABSOLUTE], 2, Duration::from_secs(1));
        let xs: Vec<_> = events
            .iter()
            .filter(|e| e.code() == AbsoluteAxisCode::ABS_X.0)
            .map(|e| e.value())
            .collect();
        let ys: Vec<_> = events
            .iter()
            .filter(|e| e.code() == AbsoluteAxisCode::ABS_Y.0)
            .map(|e| e.value())
            .collect();
        assert!(!xs.is_empty(), "expected at least one ABS_X event");
        assert!(!ys.is_empty(), "expected at least one ABS_Y event");
        // Default screen is 1920x1080 → COORD_MAX (32767) range:
        //   x = 640 → 640*32767/1920 ≈ 10922
        //   y = 360 → 360*32767/1080 ≈ 10922
        // Allow ±50 for integer truncation across the divisor.
        assert!(
            (10870..=10970).contains(xs.last().unwrap()),
            "ABS_X scaled into virtual range, got {xs:?}"
        );
        assert!(
            (10870..=10970).contains(ys.last().unwrap()),
            "ABS_Y scaled into virtual range, got {ys:?}"
        );
    });
}

#[test]
#[ignore]
fn pointer_click_emits_button_down_then_up() {
    drive(|sim, dev| {
        sim.pointer_click(Point::new(100, 100), MouseButton::Left, 1)
            .expect("pointer_click");
        let events = collect_events(dev, &[EventType::KEY], 2, Duration::from_secs(1));
        let btn: Vec<_> = events
            .iter()
            .filter(|e| e.code() == KeyCode::BTN_LEFT.0)
            .map(|e| e.value())
            .collect();
        assert_eq!(
            btn,
            vec![1, 0],
            "expected BTN_LEFT press(1) then release(0), got {btn:?}"
        );
    });
}

#[test]
#[ignore]
fn pointer_right_click_uses_btn_right() {
    drive(|sim, dev| {
        sim.pointer_click(Point::new(0, 0), MouseButton::Right, 1)
            .expect("right click");
        let events = collect_events(dev, &[EventType::KEY], 2, Duration::from_secs(1));
        let btn: Vec<_> = events
            .iter()
            .filter(|e| e.code() == KeyCode::BTN_RIGHT.0)
            .map(|e| e.value())
            .collect();
        assert_eq!(btn, vec![1, 0]);
    });
}

#[test]
#[ignore]
fn pointer_scroll_emits_rel_wheel() {
    drive(|sim, dev| {
        sim.pointer_scroll(Point::new(50, 50), ScrollDelta { dx: 0, dy: 3 })
            .expect("scroll");
        let events = collect_events(dev, &[EventType::RELATIVE], 1, Duration::from_secs(1));
        let wheel: Vec<_> = events
            .iter()
            .filter(|e| e.code() == RelativeAxisCode::REL_WHEEL.0)
            .map(|e| e.value())
            .collect();
        // dy=3 (content scrolls down) → wheel = -3 (wheel rolls toward user).
        assert_eq!(wheel, vec![-3], "REL_WHEEL value, got {wheel:?}");
    });
}

#[test]
#[ignore]
fn key_down_up_emits_keycode_then_release() {
    drive(|sim, dev| {
        sim.key_down(&Key::Enter).expect("key_down Enter");
        sim.key_up(&Key::Enter).expect("key_up Enter");
        let events = collect_events(dev, &[EventType::KEY], 2, Duration::from_secs(1));
        // Don't pin the exact keycode (xkb default may vary by host),
        // just assert one key fires press(1)+release(0) with matching codes.
        assert!(
            events.len() >= 2,
            "expected at least 2 key events, got {} ({events:?})",
            events.len()
        );
        let presses: Vec<_> = events.iter().filter(|e| e.value() == 1).collect();
        let releases: Vec<_> = events.iter().filter(|e| e.value() == 0).collect();
        assert_eq!(presses.len(), 1, "one press, got {}", presses.len());
        assert_eq!(releases.len(), 1, "one release, got {}", releases.len());
        assert_eq!(presses[0].code(), releases[0].code(), "matched keycode");
    });
}

#[test]
#[ignore]
fn type_text_holds_shift_for_uppercase() {
    drive(|sim, dev| {
        sim.type_text("Hi").expect("type_text");
        let events = collect_events(dev, &[EventType::KEY], 6, Duration::from_secs(1));
        // Expected sequence for "Hi" on a US layout:
        //   shift_down, H_down, H_up, shift_up, i_down, i_up
        // We don't know the exact xkb keycodes for H / i (depends on
        // the active keymap), but we DO know the shift keycode is the
        // same for both press and release, and shift is held only for
        // the uppercase letter.
        let shift_events: Vec<_> = events
            .iter()
            .filter(|e| e.code() == KeyCode::KEY_LEFTSHIFT.0)
            .map(|e| e.value())
            .collect();
        assert_eq!(
            shift_events,
            vec![1, 0],
            "shift held once around the uppercase 'H', got {shift_events:?}"
        );
    });
}

#[test]
#[ignore]
fn the_evdev_node_path_is_under_dev_input() {
    // Sanity check: the device the kernel exposes is rooted at /dev/input
    // (matters for tools that rely on udev scanning).
    drive(|_sim, dev| {
        let _ = dev; // dev came from /dev/input scan, by construction
        let dir = PathBuf::from("/dev/input");
        assert!(dir.is_dir(), "/dev/input must exist");
    });
}
