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
//! let status = check_permissions().expect("Permission check failed");
//!
//! match status {
//!     PermissionStatus::Granted => {
//!         let root = app(
//!             &AppTarget::ByName("Safari".to_string()),
//!         ).expect("Failed to get app");
//!
//!         // Snapshot navigation — no refetch
//!         let buttons = root.query("button").expect("Query failed");
//!         println!("Found {} buttons", buttons.len());
//!         for btn in &buttons {
//!             if let Some(parent) = btn.parent() {
//!                 println!("  {} (parent: {})", btn.name.as_deref().unwrap_or("?"), parent.role);
//!             }
//!         }
//!
//!         // Locator for actions — refetches every time
//!         let loc = locator(
//!             AppTarget::ByName("Safari".to_string()),
//!             "button[name=\"OK\"]",
//!         ).expect("Failed to create locator");
//!         loc.press().expect("Failed to press");
//!     }
//!     PermissionStatus::Denied { instructions } => {
//!         eprintln!("Accessibility not enabled: {}", instructions);
//!     }
//! }
//! ```

use std::sync::{Arc, OnceLock};

// Re-export public types. Tree is exported because Provider trait methods reference it,
// but end users should interact with Node (returned by app/apps), not Tree directly.
pub use xa11y_core::{
    Action, ActionData, AppTarget, CancelHandle, ElementState, Error, Event, EventFilter,
    EventKind, EventReceiver, Locator, Node, NodeData, PermissionStatus, RawPlatformData, Rect,
    Result, Role, ScrollDirection, StateFlag, StateSet, Subscription, TextChangeData,
    TextChangeType, Toggled, Tree, WindowHandle,
};

// Provider traits are implementation details used by platform backends and Python bindings,
// not part of the public API for end users.
#[doc(hidden)]
pub use xa11y_core::{EventProvider, Provider};

// ── Internal singleton ──────────────────────────────────────────────────────

// Use Box::leak so the provider is never dropped — avoids Windows COM
// teardown crashes (STATUS_ACCESS_VIOLATION at process exit).
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

/// Wrapper that lets a `&'static dyn Provider` be shared as `Arc<dyn Provider>`
/// for use with `Locator`.
struct StaticProviderRef(&'static dyn Provider);

impl Provider for StaticProviderRef {
    fn get_app_tree(&self, target: &AppTarget) -> Result<xa11y_core::Tree> {
        self.0.get_app_tree(target)
    }
    fn get_apps(&self) -> Result<xa11y_core::Tree> {
        self.0.get_apps()
    }
    fn perform_action(
        &self,
        tree: &xa11y_core::Tree,
        node: &NodeData,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        self.0.perform_action(tree, node, action, data)
    }
    fn check_permissions(&self) -> Result<PermissionStatus> {
        self.0.check_permissions()
    }
}

/// Get the global provider as an `Arc<dyn Provider>`.
///
/// Returns a handle to the same singleton used by `app()`, `apps()`, etc.
#[doc(hidden)]
pub fn provider() -> Result<Arc<dyn Provider>> {
    Ok(Arc::new(StaticProviderRef(get_provider_ref()?)))
}

// ── Module-level API ────────────────────────────────────────────────────────

/// Snapshot a specific application's accessibility tree.
///
/// Returns the root [`Node`], which you can navigate via
/// `parent()`, `children()`, and `query()` — all using the snapshot
/// (no platform refetch).
pub fn app(target: &AppTarget) -> Result<Node> {
    let tree = get_provider_ref()?.get_app_tree(target)?;
    let tree = Arc::new(tree);
    Ok(Node::new(tree, 0))
}

/// Snapshot all running applications (shallow).
///
/// Returns the root [`Node`] of a merged tree containing all apps.
pub fn apps() -> Result<Node> {
    let tree = get_provider_ref()?.get_apps()?;
    let tree = Arc::new(tree);
    Ok(Node::new(tree, 0))
}

/// Perform an action on a node from a specific snapshot.
///
/// Uses the node's snapshot to identify the element — does NOT refetch.
/// For actions that always use fresh data, use a [`Locator`] instead.
#[cfg(feature = "testing")]
pub fn perform_action(node: &Node, action: Action, data: Option<ActionData>) -> Result<()> {
    let tree = node.tree();
    let node_data = tree
        .get_data(node.node_index())
        .expect("Node index must be valid within its snapshot");
    get_provider_ref()?.perform_action(tree, node_data, action, data)
}

/// Check if accessibility permissions are granted.
pub fn check_permissions() -> Result<PermissionStatus> {
    get_provider_ref()?.check_permissions()
}

/// Create a Locator targeting a specific application.
///
/// Locators re-resolve against a fresh snapshot on every operation,
/// making them immune to staleness.
pub fn locator(target: AppTarget, selector: &str) -> Result<Locator> {
    Ok(Locator::new(provider()?, target, selector))
}

// ── Platform provider construction (internal) ───────────────────────────────

/// Create a new platform-appropriate accessibility provider.
///
/// Returns a fresh provider instance (not the global singleton).
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

/// Create a platform-appropriate event provider (supports subscribe/wait).
///
/// Returns a boxed `EventProvider` trait object for the current platform.
/// EventProvider extends Provider with event subscription capabilities.
#[doc(hidden)]
#[cfg(feature = "testing")]
pub fn create_event_provider() -> Result<Box<dyn EventProvider>> {
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
