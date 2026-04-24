//! Wayland session-detection smoke tests for [`xa11y_linux`].
//!
//! These exercise the environment-variable logic that picks a backend without
//! requiring a real Wayland compositor or `xdg-desktop-portal`:
//!
//! - Input sim on Wayland-only sessions returns `Error::Unsupported`
//!   (per Tenet 1 — no silent fallback to keysym guessing or libei without an
//!   explicit backend).
//! - The screenshot provider construction picks the Wayland branch when only
//!   `WAYLAND_DISPLAY` is set, and the X11 branch when `DISPLAY` is set.
//!
//! Runs in a single-threaded section so env mutations don't race. Marked
//! `#[ignore]` so the normal unit-test pass doesn't flip env vars on a
//! developer workstation.

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
#[ignore]
fn input_provider_reports_unsupported_on_wayland_only_session() {
    scoped_env(None, Some("wayland-0"), || {
        match LinuxInputProvider::new() {
            Err(Error::Unsupported { feature }) => {
                assert!(
                    feature.contains("Wayland") || feature.contains("input simulation"),
                    "feature string should name the missing capability; got {feature:?}"
                );
            }
            Err(other) => panic!("expected Error::Unsupported, got {other:?}"),
            Ok(_) => panic!("expected Error::Unsupported, got Ok"),
        }
    });
}

#[test]
#[ignore]
fn input_provider_reports_unsupported_with_no_display() {
    scoped_env(None, None, || match LinuxInputProvider::new() {
        Err(Error::Unsupported { .. }) => {}
        Err(other) => panic!("expected Error::Unsupported, got {other:?}"),
        Ok(_) => panic!("expected Error::Unsupported, got Ok"),
    });
}

#[test]
#[ignore]
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
#[ignore]
fn screenshot_constructor_reports_unsupported_with_no_display() {
    scoped_env(None, None, || match LinuxScreenshot::new() {
        Err(Error::Unsupported { .. }) => {}
        Err(other) => panic!("expected Error::Unsupported, got {other:?}"),
        Ok(_) => panic!("expected Error::Unsupported, got Ok"),
    });
}
