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
//! // Find an app by name
//! let safari = locator(provider().unwrap(), r#"application[name="Safari"]"#);
//!
//! // Lazy navigation
//! let app = safari.element().expect("App not found");
//! for child in app.children().unwrap() {
//!     println!("{}: {:?}", child.role, child.name);
//! }
//!
//! // Actions via locator
//! safari.child(r#"button[name="OK"]"#).press().expect("Failed to press");
//!
//! // Scoped locator from an element
//! let toolbar = app.children().unwrap().into_iter().next().unwrap();
//! toolbar.locator("button").elements().expect("Query failed");
//! ```

use std::sync::{Arc, OnceLock};

// Re-export public types.
pub use xa11y_core::{
    locator, Action, ActionData, Element, ElementData, ElementState, Error, Event, EventType,
    Locator, PermissionStatus, RawPlatformData, Rect, Result, Role, StateFlag, StateSet,
    Subscription, SubscriptionIter, TextChangeData, TextChangeType, Toggled,
};

// Implementation details used by platform backends and Python bindings.
#[doc(hidden)]
pub use xa11y_core::{CancelHandle, EventReceiver, Provider, Selector};

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
    fn check_permissions(&self) -> Result<PermissionStatus> {
        self.0.check_permissions()
    }
    fn subscribe(&self, element: &ElementData) -> Result<Subscription> {
        self.0.subscribe(element)
    }
}

/// Get the global provider as an `Arc<dyn Provider>`.
pub fn provider() -> Result<Arc<dyn Provider>> {
    Ok(Arc::new(StaticProviderRef(get_provider_ref()?)))
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
