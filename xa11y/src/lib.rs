//! xa11y — Cross-Platform Accessibility Client Library
//!
//! Provides a unified API for reading and interacting with accessibility trees
//! across desktop platforms (macOS, Windows, Linux).
//!
//! # Quick Start
//!
//! ```no_run
//! use std::time::Duration;
//! use xa11y::*;
//!
//! let app = App::by_name("Safari", Duration::from_secs(5)).expect("App not found");
//!
//! for child in app.children().unwrap() {
//!     println!("{}: {:?}", child.role, child.name);
//! }
//!
//! app.locator(r#"button[name="OK"]"#).press().expect("Failed to press");
//! ```

use std::sync::{Arc, OnceLock};

// Re-export public types.
pub use xa11y_core::{
    App, Diagnosis, Element, ElementData, ElementState, Error, Event, EventKind, Locator,
    RawPlatformData, Rect, Result, Role, StateFlag, StateSet, Subscription, SubscriptionIter,
    Toggled, TreeNode,
};

// Re-export the process-wide default-timeout configuration (see
// `xa11y_core::config`): the default for every auto-wait / `wait_*` call
// that doesn't pass an explicit timeout. `set_default_timeout` overrides the
// `XA11Y_DEFAULT_TIMEOUT` environment variable, which overrides the built-in
// 5 seconds.
pub use xa11y_core::{default_timeout, set_default_timeout, DEFAULT_TIMEOUT_ENV_VAR};

// Re-export input simulation surface.
pub use xa11y_core::input;
pub use xa11y_core::{
    anchor_point, point_for, Anchor, ClickOptions, ClickTarget, DragOptions, InputProvider,
    InputSim, IntoPoint, Key, Keyboard, Mouse, MouseButton, Point, ScrollDelta,
};

// Re-export screenshot surface.
pub use xa11y_core::screenshot;
pub use xa11y_core::{Screenshot, ScreenshotProvider};

// Re-export bidi text helpers (see `xa11y_core::text`). `name`, `value`, and
// `description` on `ElementData` are stripped of bidi format controls; these
// helpers let callers strip ad-hoc strings or check membership.
pub use xa11y_core::{is_bidi_control, strip_bidi, strip_bidi_opt};

// Implementation details used by platform backends and Python bindings.
#[doc(hidden)]
pub use xa11y_core::{CancelHandle, EventReceiver, Provider, RecvStatus, Selector, SelectorGroup};

/// Shared in-memory mock Provider — re-exported from `xa11y-core` when the
/// `test-support` feature is enabled. Used by language-binding tests so
/// Python and JS don't each carry their own copy of the fixture.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use xa11y_core::mock;

#[doc(hidden)]
pub mod cli;

// Re-export the extension trait so `use xa11y::*` enables `App::by_name(...)`.
pub use app_ext::AppExt;

// ── Internal singleton ──────────────────────────────────────────────────────

static PROVIDER: OnceLock<std::result::Result<&'static dyn Provider, String>> = OnceLock::new();

fn get_provider_ref() -> Result<&'static dyn Provider> {
    PROVIDER
        .get_or_init(|| {
            create_provider_boxed()
                .map(|b| &*Box::leak(b))
                .map_err(|e| format!("{e}"))
        })
        .as_ref()
        .copied()
        .map_err(|msg| Error::Platform {
            code: -1,
            message: msg.clone(),
        })
}

#[doc(hidden)]
pub fn provider() -> Result<Arc<dyn Provider>> {
    Ok(Arc::new(get_provider_ref()?))
}

// ── Platform provider construction (internal) ───────────────────────────────

#[doc(hidden)]
#[cfg(feature = "testing")]
pub fn create_provider() -> Result<Arc<dyn Provider>> {
    create_provider_boxed().map(Arc::from)
}

/// Build an [`InputSim`] backed by the platform's native input-synthesis API
/// (CGEvent on macOS, SendInput on Windows, XTest on X11). Returns
/// [`Error::Unsupported`] on a Wayland-only Linux session and
/// [`Error::Platform`] on any other platform we don't ship a backend for.
///
/// `InputSim` is cheap to clone — construct one and share it.
pub fn input_sim() -> Result<InputSim> {
    #[cfg(target_os = "macos")]
    {
        let backend = xa11y_macos::MacOSInputProvider::new()?;
        Ok(InputSim::new(Arc::new(backend)))
    }
    #[cfg(target_os = "windows")]
    {
        let backend = xa11y_windows::WindowsInputProvider::new()?;
        Ok(InputSim::new(Arc::new(backend)))
    }
    #[cfg(target_os = "linux")]
    {
        let backend = xa11y_linux::LinuxInputProvider::new()?;
        Ok(InputSim::new(Arc::new(backend)))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err(Error::Platform {
            code: -1,
            message: format!(
                "Input simulation not available on platform: {}",
                std::env::consts::OS
            ),
        })
    }
}

// ── Screenshot entry points ────────────────────────────────────────────
//
// Three bare functions instead of a factory + handle. The platform backend
// (ScreenCaptureKit on macOS, X11 `GetImage` or xdg-desktop-portal on Linux,
// GDI on Windows) is initialised lazily on first call and memoized in a
// `OnceLock`, so repeated captures reuse the same backend without paying
// construction cost per call.
//
// All three return:
// - [`Error::PermissionDenied`] on macOS if Screen Recording is not granted
//   (or on Linux if the Wayland portal denies consent).
// - [`Error::Unsupported`] on Linux if neither `DISPLAY` nor `WAYLAND_DISPLAY`
//   is set, and on older Windows contexts where `BitBlt` is unavailable.

static SCREENSHOT_BACKEND: OnceLock<std::result::Result<Arc<dyn ScreenshotProvider>, String>> =
    OnceLock::new();

fn screenshot_backend() -> Result<Arc<dyn ScreenshotProvider>> {
    SCREENSHOT_BACKEND
        .get_or_init(create_screenshot_backend)
        .as_ref()
        .cloned()
        .map_err(|msg| Error::Platform {
            code: -1,
            message: msg.clone(),
        })
}

fn create_screenshot_backend() -> std::result::Result<Arc<dyn ScreenshotProvider>, String> {
    #[cfg(target_os = "macos")]
    {
        xa11y_macos::MacOSScreenshot::new()
            .map(|b| Arc::new(b) as Arc<dyn ScreenshotProvider>)
            .map_err(|e| format!("{e}"))
    }
    #[cfg(target_os = "windows")]
    {
        xa11y_windows::WindowsScreenshot::new()
            .map(|b| Arc::new(b) as Arc<dyn ScreenshotProvider>)
            .map_err(|e| format!("{e}"))
    }
    #[cfg(target_os = "linux")]
    {
        xa11y_linux::LinuxScreenshot::new()
            .map(|b| Arc::new(b) as Arc<dyn ScreenshotProvider>)
            .map_err(|e| format!("{e}"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err(format!(
            "Screenshot not available on platform: {}",
            std::env::consts::OS
        ))
    }
}

/// Capture the full primary display.
pub fn screenshot() -> Result<Screenshot> {
    screenshot_backend()?.capture_full()
}

/// Capture an explicit sub-rectangle of the screen.
pub fn screenshot_region(rect: Rect) -> Result<Screenshot> {
    screenshot_backend()?.capture_region(rect)
}

/// Capture the pixels under an element's current bounds.
///
/// Returns [`Error::NoElementBounds`] if the element has no bounds. The target
/// window is **not** raised or activated — see the `screenshot` module docs.
pub fn screenshot_element(element: &Element) -> Result<Screenshot> {
    let rect = element.bounds.ok_or(Error::NoElementBounds)?;
    screenshot_backend()?.capture_region(rect)
}

fn create_provider_boxed() -> Result<Box<dyn Provider>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(xa11y_macos::MacOSProvider::new()?))
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(xa11y_windows::WindowsProvider::new()?))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(xa11y_linux::LinuxProvider::new()?))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err(Error::Platform {
            code: -1,
            message: format!("Unsupported platform: {}", std::env::consts::OS),
        })
    }
}

// ── AppExt extension trait ───────────────────────────────────────────────────

mod app_ext {
    use std::time::Duration;

    use super::{provider, App, ElementData, Result};

    /// Extension trait that adds singleton-based constructors to [`App`].
    ///
    /// Imported automatically via `use xa11y::*`.
    ///
    /// # Example
    /// ```no_run
    /// use std::time::Duration;
    /// use xa11y::*;
    ///
    /// let app = App::by_name("Safari", Duration::from_secs(5))?;
    /// # Ok::<(), xa11y::Error>(())
    /// ```
    pub trait AppExt: Sized {
        /// Find an application by exact name using the global singleton
        /// provider, polling until it appears or `timeout` elapses. Pass
        /// `Duration::ZERO` for a single attempt with no waiting. See
        /// [`App::by_name_with`] for retry semantics.
        fn by_name(name: &str, timeout: Duration) -> Result<Self>;
        /// Find an application by process ID using the global singleton
        /// provider, polling until it appears or `timeout` elapses.
        ///
        /// This is the supported way to wait for a freshly launched process
        /// to surface in the accessibility tree — the poll covers the window
        /// between process spawn and the platform bridge registering the
        /// app, so callers don't need a hand-rolled loop over
        /// [`list`](Self::list). See [`App::by_pid_with`] for the full
        /// contract and [`by_name`](Self::by_name) for retry semantics.
        fn by_pid(pid: u32, timeout: Duration) -> Result<Self>;
        /// List all running applications using the global singleton provider.
        fn list() -> Result<Vec<Self>>;
        /// Find an application matching `predicate` using the global
        /// singleton provider, polling until one appears or `timeout`
        /// elapses. `predicate` runs against each running app's
        /// [`ElementData`] on every poll. See [`App::find_with`] for
        /// match / timeout semantics.
        fn find<F>(timeout: Duration, predicate: F) -> Result<Self>
        where
            F: Fn(&ElementData) -> bool;
        /// Like [`find`](Self::find), but with a fallible predicate:
        /// `Ok(false)` keeps polling while `Err(_)` aborts and propagates.
        /// See [`App::try_find_with`].
        fn try_find<F>(timeout: Duration, predicate: F) -> Result<Self>
        where
            F: Fn(&ElementData) -> Result<bool>;
    }

    impl AppExt for App {
        fn by_name(name: &str, timeout: Duration) -> Result<Self> {
            App::by_name_with(provider()?, name, timeout)
        }

        fn by_pid(pid: u32, timeout: Duration) -> Result<Self> {
            App::by_pid_with(provider()?, pid, timeout)
        }

        fn list() -> Result<Vec<Self>> {
            App::list_with(provider()?)
        }

        fn find<F>(timeout: Duration, predicate: F) -> Result<Self>
        where
            F: Fn(&ElementData) -> bool,
        {
            App::find_with(provider()?, timeout, predicate)
        }

        fn try_find<F>(timeout: Duration, predicate: F) -> Result<Self>
        where
            F: Fn(&ElementData) -> Result<bool>,
        {
            App::try_find_with(provider()?, timeout, predicate)
        }
    }
}
