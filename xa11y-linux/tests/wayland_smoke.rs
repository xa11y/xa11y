//! Session-detection smoke tests for [`xa11y_linux`].
//!
//! These exercise the environment-variable logic that picks a backend
//! without requiring a real Wayland compositor or `xdg-desktop-portal`:
//!
//! - Input sim falls through to the uinput backend when `DISPLAY` is
//!   unset. The constructor either succeeds (the user is in `input` and
//!   `/dev/uinput` is accessible — typical CI with `--device /dev/uinput`)
//!   or surfaces `PermissionDenied`/`Unsupported`, but **never** the old
//!   "no backend on Wayland" `Unsupported` that the libei-only backend
//!   used to return.
//! - The screenshot provider picks the Wayland branch when only
//!   `WAYLAND_DISPLAY` is set, and the X11 branch when `DISPLAY` is set.
//!
//! Env mutation is serialised by `ENV_LOCK` and each test save/restore
//! the prior values, so they're safe to run as part of
//! `cargo test --workspace`.

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
fn input_provider_falls_through_to_uinput_when_no_display() {
    // Without DISPLAY, the constructor takes the uinput branch. The
    // outcome depends on the host:
    //   - Ok: /dev/uinput is open()-able (user is in `input` group, or
    //     we're running as root in the e2e container with --device).
    //   - PermissionDenied: `input` group missing — actionable error.
    //   - Unsupported (with "uinput" in the feature string): kernel
    //     module not loaded.
    // The one outcome the old libei backend used to return — Unsupported
    // because Wayland wasn't supported — must never come back now.
    scoped_env(None, Some("wayland-0"), || {
        match LinuxInputProvider::new() {
            Ok(_) => {}
            Err(Error::PermissionDenied { .. }) => {}
            Err(Error::Unsupported { feature }) => {
                assert!(
                    feature.contains("uinput"),
                    "Unsupported must come from uinput probing, got {feature:?}"
                );
            }
            Err(Error::Platform { message, .. }) => {
                assert!(
                    message.contains("uinput"),
                    "Platform error must come from uinput probing, got {message:?}"
                );
            }
            Err(other) => panic!("unexpected error from uinput-branch constructor: {other:?}"),
        }
    });
}

#[test]
fn input_provider_falls_through_to_uinput_with_no_envs() {
    // Same as above but with WAYLAND_DISPLAY also unset. uinput doesn't
    // care — Wayland is not a precondition for /dev/uinput.
    scoped_env(None, None, || match LinuxInputProvider::new() {
        Ok(_) => {}
        Err(Error::PermissionDenied { .. }) => {}
        Err(Error::Unsupported { feature }) => assert!(feature.contains("uinput")),
        Err(Error::Platform { message, .. }) => assert!(message.contains("uinput")),
        Err(other) => panic!("unexpected error: {other:?}"),
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
