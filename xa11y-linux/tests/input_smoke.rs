//! Smoke test for [`xa11y_linux::LinuxInputProvider`] against a live X server.
//!
//! Requires an X display — run under `xvfb-run`. Marked `#[ignore]` so it
//! doesn't fire in the normal unit-test pass.

#![cfg(target_os = "linux")]

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as _;

use xa11y_core::input::{InputProvider, Key, MouseButton, Point};
use xa11y_linux::LinuxInputProvider;

fn with_verifier<F: FnOnce(&LinuxInputProvider, x11rb::rust_connection::RustConnection, u32)>(
    f: F,
) {
    let provider = LinuxInputProvider::new().expect("LinuxInputProvider::new");
    let (conn, screen_num) =
        x11rb::rust_connection::RustConnection::connect(None).expect("verifier connect");
    let root = conn.setup().roots[screen_num].root;
    f(&provider, conn, root);
}

#[test]
#[ignore]
fn pointer_move_updates_pointer_position() {
    with_verifier(|provider, conn, root| {
        provider
            .pointer_move(Point::new(123, 45))
            .expect("pointer_move");
        let reply = conn.query_pointer(root).unwrap().reply().unwrap();
        assert_eq!(
            (reply.root_x, reply.root_y),
            (123, 45),
            "pointer should be at (123,45), got ({},{})",
            reply.root_x,
            reply.root_y
        );
    });
}

#[test]
#[ignore]
fn pointer_click_does_not_panic() {
    with_verifier(|provider, _conn, _root| {
        provider
            .pointer_click(Point::new(50, 50), MouseButton::Left, 1)
            .expect("pointer_click");
        provider
            .pointer_click(Point::new(60, 60), MouseButton::Left, 2)
            .expect("double click");
    });
}

#[test]
#[ignore]
fn keyboard_press_and_type_do_not_panic() {
    with_verifier(|provider, _conn, _root| {
        provider.key_down(&Key::Char('a')).expect("key_down a");
        provider.key_up(&Key::Char('a')).expect("key_up a");
        provider.type_text("Hello, world!").expect("type_text");
        provider.key_down(&Key::Enter).expect("key_down Enter");
        provider.key_up(&Key::Enter).expect("key_up Enter");
    });
}
