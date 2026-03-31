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
    Action, ActionData, App, Element, ElementData, ElementState, Error, Event, EventType, Locator,
    RawPlatformData, Rect, Result, Role, StateFlag, StateSet, Subscription, SubscriptionIter,
    TextChangeData, TextChangeType, Toggled,
};

// Implementation details used by platform backends and Python bindings.
#[doc(hidden)]
pub use xa11y_core::{CancelHandle, EventReceiver, Provider, Selector};

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

/// Wrapper that lets a `&'static dyn Provider` be shared as `Arc<dyn Provider>`.
struct StaticProviderRef(&'static dyn Provider);

impl Provider for StaticProviderRef {
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        self.0.get_children(element)
    }
    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
        self.0.get_parent(element)
    }
    fn perform_action(
        &self,
        element: &ElementData,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        self.0.perform_action(element, action, data)
    }
    fn subscribe(&self, element: &ElementData) -> Result<Subscription> {
        self.0.subscribe(element)
    }
}

#[doc(hidden)]
pub fn provider() -> Result<Arc<dyn Provider>> {
    Ok(Arc::new(StaticProviderRef(get_provider_ref()?)))
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
        /// Find an application by process ID using the global singleton provider.
        fn by_pid(pid: u32) -> Result<Self>;
        /// List all running applications using the global singleton provider.
        fn list() -> Result<Vec<Self>>;
    }

    impl AppExt for App {
        fn by_name(name: &str) -> Result<Self> {
            App::by_name_with(provider()?, name)
        }

        fn by_pid(pid: u32) -> Result<Self> {
            App::by_pid_with(provider()?, pid)
        }

        fn list() -> Result<Vec<Self>> {
            App::list_with(provider()?)
        }
    }
}
