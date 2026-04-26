//! End-to-end Wayland input tests.
//!
//! These drive [`xa11y_linux::LinuxInputProvider`] against a real GNOME
//! RemoteDesktop portal + libei stack. They are `#[ignore]`'d by default
//! and only run inside the `xa11y-wayland-libei` container via
//! `scripts/run_wayland_libei.sh`, which:
//!
//! 1. starts a D-Bus session bus, mutter (headless), pipewire, and
//!    xdg-desktop-portal-gnome,
//! 2. pre-grants RemoteDesktop access so the consent prompt doesn't block,
//! 3. invokes `cargo test ... -- --ignored` against this file.
//!
//! Success criterion: the libei round-trip completes without error. We
//! intentionally don't assert on observable side-effects (no app under
//! the cursor in the headless container) — the portal accepting our
//! events is enough to validate the wire-up. Unit-level event-shape
//! checks live in `src/wayland_input.rs::tests`.

#![cfg(target_os = "linux")]

use xa11y_core::input::{InputProvider, Key, MouseButton, Point, ScrollDelta};
use xa11y_linux::LinuxInputProvider;

#[test]
#[ignore]
fn pointer_move_and_click_via_portal() {
    let sim = LinuxInputProvider::new().expect("portal RemoteDesktop session");
    sim.pointer_move(Point::new(100, 100))
        .expect("pointer_move");
    sim.pointer_click(Point::new(100, 100), MouseButton::Left, 1)
        .expect("pointer_click");
}

#[test]
#[ignore]
fn type_text_via_portal() {
    let sim = LinuxInputProvider::new().expect("portal RemoteDesktop session");
    sim.type_text("hello").expect("type_text");
}

#[test]
#[ignore]
fn key_down_up_via_portal() {
    let sim = LinuxInputProvider::new().expect("portal RemoteDesktop session");
    sim.key_down(&Key::Enter).expect("key_down");
    sim.key_up(&Key::Enter).expect("key_up");
}

#[test]
#[ignore]
fn pointer_scroll_via_portal() {
    let sim = LinuxInputProvider::new().expect("portal RemoteDesktop session");
    sim.pointer_scroll(Point::new(100, 100), ScrollDelta { dx: 0, dy: 1 })
        .expect("pointer_scroll");
}
