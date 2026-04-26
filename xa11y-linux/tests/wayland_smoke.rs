//! Wayland session-detection smoke tests for [`xa11y_linux`].
//!
//! These exercise the environment-variable logic that picks a backend without
//! requiring a real Wayland compositor or `xdg-desktop-portal`:
//!
//! - Input sim on Wayland-only sessions reaches the portal RemoteDesktop
//!   call (returning `Ok` on a real session, or `Error::Platform` when the
//!   session bus / portal isn't available — but never `Unsupported`, which
//!   the old X11-only backend used to return).
//! - The screenshot provider construction picks the Wayland branch when only
//!   `WAYLAND_DISPLAY` is set, and the X11 branch when `DISPLAY` is set.
//!
//! Env mutation is serialised by `ENV_LOCK` and each test save/restore the
//! prior values, so they're safe to run as part of `cargo test --workspace`.

#![cfg(target_os = "linux")]

use xa11y_core::Error;
use xa11y_linux::{LinuxInputProvider, LinuxScreenshot};

// The three tests below all mutate process-global env vars. To guarantee
// single-threaded access even when the harness forgets `--test-threads=1`,
// they share a Mutex and each test performs its own save/restore.
use std::sync::Mutex;
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn scoped_env<F: FnOnce()>(display: Option<&str>, wayland: Option<&str>, f: F) {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev_display = std::env::var_os("DISPLAY");
    let prev_wayland = std::env::var_os("WAYLAND_DISPLAY");
    // SAFETY: env mutations are guarded by ENV_LOCK and restored below.
    unsafe {
        match display {
            Some(v) => std::env::set_var("DISPLAY", v),
            None => std::env::remove_var("DISPLAY"),
        }
        match wayland {
            Some(v) => std::env::set_var("WAYLAND_DISPLAY", v),
            None => std::env::remove_var("WAYLAND_DISPLAY"),
        }
    }
    f();
    unsafe {
        match prev_display {
            Some(v) => std::env::set_var("DISPLAY", v),
            None => std::env::remove_var("DISPLAY"),
        }
        match prev_wayland {
            Some(v) => std::env::set_var("WAYLAND_DISPLAY", v),
            None => std::env::remove_var("WAYLAND_DISPLAY"),
        }
    }
}

#[test]
fn input_provider_picks_wayland_branch() {
    // The input-sim Wayland constructor reaches out to the session bus and
    // the portal. Without those it must surface Error::Platform (or the
    // PermissionDenied raised when a portal denies the request) — but it
    // must *not* return Unsupported, which was the old X11-only behaviour.
    scoped_env(None, Some("wayland-0"), || {
        match LinuxInputProvider::new() {
            Ok(_) => {}
            Err(Error::Platform { message, .. }) => {
                assert!(
                    message.contains("session bus")
                        || message.contains("portal")
                        || message.contains("ei"),
                    "Wayland-branch error should come from the portal/EI path, got {message:?}"
                );
            }
            Err(Error::PermissionDenied { .. }) => {}
            Err(Error::Unsupported { feature }) => panic!(
                "Wayland input must no longer be Unsupported, got Unsupported {{ {feature:?} }}"
            ),
            Err(other) => panic!("unexpected error from Wayland-branch constructor: {other:?}"),
        }
    });
}

#[test]
fn input_provider_reports_unsupported_with_no_display() {
    scoped_env(None, None, || match LinuxInputProvider::new() {
        Err(Error::Unsupported { .. }) => {}
        Err(other) => panic!("expected Error::Unsupported, got {other:?}"),
        Ok(_) => panic!("expected Error::Unsupported, got Ok"),
    });
}

#[test]
fn screenshot_constructor_picks_wayland_branch() {
    // Construction must succeed even when no portal is reachable yet — the
    // error only shows up at capture time. This mirrors the X11 branch, where
    // the X server connection is also lazy from the caller's perspective in
    // that `new()` doesn't probe the portal RPC.
    scoped_env(None, Some("wayland-0"), || {
        // If the session bus isn't available inside the container, we surface
        // Error::Platform; that's fine — we're testing that the *branch* was
        // Wayland, not that the portal RPC succeeds.
        match LinuxScreenshot::new() {
            Ok(_) => {}
            Err(Error::Platform { message, .. }) => {
                assert!(
                    message.contains("session bus"),
                    "Wayland-branch error should come from session-bus connect, got {message:?}"
                );
            }
            Err(other) => panic!("unexpected error from Wayland-branch constructor: {other:?}"),
        }
    });
}

#[test]
fn screenshot_constructor_reports_unsupported_with_no_display() {
    scoped_env(None, None, || match LinuxScreenshot::new() {
        Err(Error::Unsupported { .. }) => {}
        Err(other) => panic!("expected Error::Unsupported, got {other:?}"),
        Ok(_) => panic!("expected Error::Unsupported, got Ok"),
    });
}
