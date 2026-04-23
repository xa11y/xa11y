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

// Store the provider as an Arc<dyn Provider> so callers can cheaply clone a
// shared handle without an extra delegation wrapper or a leaked box. Err is
// carried as a string so we can clone the OnceLock's payload on every access.
static PROVIDER: OnceLock<std::result::Result<Arc<dyn Provider>, String>> = OnceLock::new();

#[doc(hidden)]
pub fn provider() -> Result<Arc<dyn Provider>> {
    match PROVIDER.get_or_init(|| {
        create_provider_boxed()
            .map(Arc::from)
            .map_err(|e| format!("{e}"))
    }) {
        Ok(arc) => Ok(Arc::clone(arc)),
        Err(msg) => Err(Error::Platform {
            code: -1,
            message: msg.clone(),
        }),
    }
}

// ── Platform provider construction (internal) ───────────────────────────────

#[doc(hidden)]
#[cfg(feature = "testing")]
pub fn create_provider() -> Result<Arc<dyn Provider>> {
    create_provider_boxed().map(Arc::from)
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
