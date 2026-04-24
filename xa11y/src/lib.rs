//! xa11y — Cross-Platform Accessibility Client Library
//!
//! Provides a unified API for reading and interacting with accessibility trees
//! across desktop platforms (macOS, Windows, Linux).
//!
//! # Quick Start
//!
//! ```no_run
//! use xa11y::*;
//!
//! let app = App::by_name("Safari").expect("App not found");
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
    App, Element, ElementData, ElementState, Error, Event, EventKind, Locator, RawPlatformData,
    Rect, Result, Role, StateFlag, StateSet, Subscription, SubscriptionIter, Toggled,
};

// Re-export input simulation surface.
pub use xa11y_core::input;
pub use xa11y_core::{
    anchor_point, point_for, Anchor, ClickOptions, ClickTarget, DragOptions, InputProvider,
    InputSim, IntoPoint, Key, Keyboard, Mouse, MouseButton, Point, ScrollDelta,
};

// Re-export screenshot surface.
pub use xa11y_core::screenshot;
pub use xa11y_core::{Screenshot, ScreenshotProvider, Screenshotter};

// Implementation details used by platform backends and Python bindings.
#[doc(hidden)]
pub use xa11y_core::{CancelHandle, EventReceiver, Provider, RecvStatus, Selector};

/// Shared in-memory mock Provider — re-exported from `xa11y-core` when the
/// `test-support` feature is enabled. Used by language-binding tests so
/// Python and JS don't each carry their own copy of the fixture.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use xa11y_core::mock;

#[doc(hidden)]
pub mod cli;

// Re-export the extension trait so `use xa11y::*` enables `App::by_name("Safari")`.
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

/// Build a [`Screenshotter`] backed by the platform's native capture API
/// (ScreenCaptureKit on macOS, X11 `GetImage` or xdg-desktop-portal on Linux,
/// stubbed on Windows).
///
/// Returns:
/// - [`Error::PermissionDenied`] on macOS if Screen Recording permission
///   hasn't been granted (or on Linux if the Wayland portal denies consent).
/// - [`Error::Unsupported`] on Linux if neither `DISPLAY` nor `WAYLAND_DISPLAY`
///   is set, and on Windows until a backend ships.
///
/// `Screenshotter` is cheap to clone — construct one and share it.
pub fn screenshotter() -> Result<Screenshotter> {
    #[cfg(target_os = "macos")]
    {
        let backend = xa11y_macos::MacOSScreenshot::new()?;
        Ok(Screenshotter::new(Arc::new(backend)))
    }
    #[cfg(target_os = "windows")]
    {
        let backend = xa11y_windows::WindowsScreenshot::new()?;
        Ok(Screenshotter::new(Arc::new(backend)))
    }
    #[cfg(target_os = "linux")]
    {
        let backend = xa11y_linux::LinuxScreenshot::new()?;
        Ok(Screenshotter::new(Arc::new(backend)))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err(Error::Platform {
            code: -1,
            message: format!(
                "Screenshot not available on platform: {}",
                std::env::consts::OS
            ),
        })
    }
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

    use super::{provider, App, Result};

    /// Extension trait that adds singleton-based constructors to [`App`].
    ///
    /// Imported automatically via `use xa11y::*`.
    ///
    /// # Example
    /// ```no_run
    /// use xa11y::*;
    ///
    /// let app = App::by_name("Safari")?;
    /// # Ok::<(), xa11y::Error>(())
    /// ```
    pub trait AppExt: Sized {
        /// Find an application by exact name using the global singleton provider.
        fn by_name(name: &str) -> Result<Self>;
        /// Find an application by exact name, polling until it appears or
        /// `timeout` elapses. See [`App::by_name_with_timeout`].
        fn by_name_timeout(name: &str, timeout: Duration) -> Result<Self>;
        /// Find an application by process ID using the global singleton provider.
        fn by_pid(pid: u32) -> Result<Self>;
        /// Find an application by process ID, polling until it appears or
        /// `timeout` elapses. See [`App::by_pid_with_timeout`].
        fn by_pid_timeout(pid: u32, timeout: Duration) -> Result<Self>;
        /// List all running applications using the global singleton provider.
        fn list() -> Result<Vec<Self>>;
    }

    impl AppExt for App {
        fn by_name(name: &str) -> Result<Self> {
            App::by_name_with(provider()?, name)
        }

        fn by_name_timeout(name: &str, timeout: Duration) -> Result<Self> {
            App::by_name_with_timeout(provider()?, name, timeout)
        }

        fn by_pid(pid: u32) -> Result<Self> {
            App::by_pid_with(provider()?, pid)
        }

        fn by_pid_timeout(pid: u32, timeout: Duration) -> Result<Self> {
            App::by_pid_with_timeout(provider()?, pid, timeout)
        }

        fn list() -> Result<Vec<Self>> {
            App::list_with(provider()?)
        }
    }
}
