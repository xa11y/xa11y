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
//!         let tree = app(
//!             &AppTarget::ByName("Safari".to_string()),
//!             &QueryOptions::default(),
//!         ).expect("Failed to get tree");
//!
//!         let buttons = tree.query("button").expect("Query failed");
//!         println!("Found {} buttons", buttons.len());
//!     }
//!     PermissionStatus::Denied { instructions } => {
//!         eprintln!("Accessibility not enabled: {}", instructions);
//!     }
//! }
//! ```

use std::sync::{Arc, OnceLock};

// Re-export all core types
pub use xa11y_core::*;

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
    fn get_app_tree(&self, target: &AppTarget, opts: &QueryOptions) -> Result<Tree> {
        self.0.get_app_tree(target, opts)
    }
    fn get_all_apps(&self, opts: &QueryOptions) -> Result<Tree> {
        self.0.get_all_apps(opts)
    }
    fn perform_action(
        &self,
        tree: &Tree,
        node: &Node,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        self.0.perform_action(tree, node, action, data)
    }
    fn check_permissions(&self) -> Result<PermissionStatus> {
        self.0.check_permissions()
    }
    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        self.0.list_apps()
    }
}

fn get_provider_arc() -> Result<Arc<dyn Provider>> {
    Ok(Arc::new(StaticProviderRef(get_provider_ref()?)))
}

// ── Module-level API ────────────────────────────────────────────────────────

/// Snapshot a specific application's accessibility tree.
pub fn app(target: &AppTarget, opts: &QueryOptions) -> Result<Tree> {
    get_provider_ref()?.get_app_tree(target, opts)
}

/// Snapshot all running applications (shallow).
pub fn all_apps(opts: &QueryOptions) -> Result<Tree> {
    get_provider_ref()?.get_all_apps(opts)
}

/// Check if accessibility permissions are granted.
pub fn check_permissions() -> Result<PermissionStatus> {
    get_provider_ref()?.check_permissions()
}

/// List running applications with their PIDs.
pub fn list_apps() -> Result<Vec<AppInfo>> {
    get_provider_ref()?.list_apps()
}

/// Get a reference to the global platform provider.
///
/// Use this to call `Provider` trait methods directly (e.g. `perform_action`).
/// For most use cases, prefer `Locator` which handles action dispatch,
/// retries, and re-querying automatically.
pub fn provider() -> Result<&'static dyn Provider> {
    get_provider_ref()
}

/// Create a Locator targeting a specific application.
pub fn locator(target: AppTarget, selector: &str) -> Result<Locator> {
    Ok(Locator::new(get_provider_arc()?, target, selector))
}

/// Create a Locator with custom query options.
pub fn locator_with_opts(target: AppTarget, selector: &str, opts: QueryOptions) -> Result<Locator> {
    Ok(Locator::with_opts(
        get_provider_arc()?,
        target,
        selector,
        opts,
    ))
}

// ── Platform provider construction (internal) ───────────────────────────────

/// Create a new platform-appropriate accessibility provider.
///
/// Returns a fresh provider instance (not the global singleton). Prefer
/// the module-level functions (`app`, `all_apps`, `provider`, etc.)
/// for normal use.
#[doc(hidden)]
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
