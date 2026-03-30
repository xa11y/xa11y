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
//! let safari = App::from_name(provider().unwrap(), "Safari")
//!     .expect("Failed to get app");
//!
//! // Snapshot navigation — read-only tree
//! let root = safari.elements().expect("Failed to snapshot");
//! for child in root.children() {
//!     println!("{}: {:?}", child.role, child.name);
//! }
//!
//! // Locator for actions — refetches every time
//! safari.locator("button[name=\"OK\"]").press().expect("Failed to press");
//!
//! // Locator to get matching elements
//! let buttons = safari.locator("button").elements().expect("Query failed");
//! println!("Found {} buttons", buttons.len());
//!
//! // By PID
//! let app = App::from_pid(provider().unwrap(), 1234).expect("Failed to get app");
//! ```

use std::sync::{Arc, OnceLock};

// Re-export public types.
pub use xa11y_core::{
    root_element, Action, ActionData, App, Element, ElementData, ElementState, Error, Event,
    EventType, Locator, PermissionStatus, RawPlatformData, Rect, Result, Role, StateFlag, StateSet,
    Subscription, SubscriptionIter, TextChangeData, TextChangeType, Toggled,
};

// Implementation details used by platform backends and Python bindings.
#[doc(hidden)]
pub use xa11y_core::{CancelHandle, EventReceiver, Provider};

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
    fn resolve_pid_by_name(&self, name: &str) -> Result<u32> {
        self.0.resolve_pid_by_name(name)
    }
    fn get_elements(&self, pid: u32) -> Result<Element> {
        self.0.get_elements(pid)
    }
    fn get_apps(&self) -> Result<Element> {
        self.0.get_apps()
    }
    fn perform_action(
        &self,
        element: &Element,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        self.0.perform_action(element, action, data)
    }
    fn check_permissions(&self) -> Result<PermissionStatus> {
        self.0.check_permissions()
    }
    fn subscribe(&self, pid: u32) -> Result<Subscription> {
        self.0.subscribe(pid)
    }
}

/// Get the global provider as an `Arc<dyn Provider>`.
///
/// Pass this to `App::from_name()`, `App::from_pid()`, etc.
pub fn provider() -> Result<Arc<dyn Provider>> {
    Ok(Arc::new(StaticProviderRef(get_provider_ref()?)))
}

/// Perform an action on an element from a specific snapshot.
#[cfg(feature = "testing")]
pub fn perform_action(element: &Element, action: Action, data: Option<ActionData>) -> Result<()> {
    get_provider_ref()?.perform_action(element, action, data)
}

/// Check if accessibility permissions are granted.
pub fn check_permissions() -> Result<PermissionStatus> {
    get_provider_ref()?.check_permissions()
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
