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
//!         let slack = app("Slack", &QueryOptions::default())
//!             .expect("Failed to get app");
//!
//!         let buttons = slack.query("button").expect("Query failed");
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
    fn perform_action_raw(
        &self,
        tree: &Tree,
        node_index: u32,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        self.0.perform_action_raw(tree, node_index, action, data)
    }
    fn check_permissions(&self) -> Result<PermissionStatus> {
        self.0.check_permissions()
    }
    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        self.0.list_apps()
    }
}

/// Get the global provider as an `Arc<dyn Provider>`.
///
/// Returns a handle to the same singleton used by `app()`, `apps()`, etc.
/// Useful when you need to pass a provider to objects that store it (e.g. `Locator`).
pub fn provider() -> Result<Arc<dyn Provider>> {
    Ok(Arc::new(StaticProviderRef(get_provider_ref()?)))
}

// ── Module-level API ────────────────────────────────────────────────────────

/// Helper to convert a Tree into a root Node cursor with provider attached.
fn tree_to_node(tree: Tree, target: Option<AppTarget>) -> Node {
    let data = Arc::new(TreeData {
        app_name: tree.app_name,
        pid: tree.pid,
        screen_size: tree.screen_size,
        nodes: tree.nodes,
        provider: provider().ok(),
        target,
    });
    Node::new(data, 0)
}

/// Snapshot a specific application by name.
///
/// Returns the root Node of the application's accessibility tree.
///
/// # Example
/// ```no_run
/// let slack = xa11y::app("Slack", &xa11y::QueryOptions::default()).unwrap();
/// for btn in slack.query("button").unwrap() {
///     println!("{:?}", btn.name());
/// }
/// ```
pub fn app(name: &str, opts: &QueryOptions) -> Result<Node> {
    let target = AppTarget::ByName(name.to_string());
    let tree = get_provider_ref()?.get_app_tree(&target, opts)?;
    Ok(tree_to_node(tree, Some(target)))
}

/// Snapshot a specific application by process ID.
pub fn app_by_pid(pid: u32, opts: &QueryOptions) -> Result<Node> {
    let target = AppTarget::ByPid(pid);
    let tree = get_provider_ref()?.get_app_tree(&target, opts)?;
    Ok(tree_to_node(tree, Some(target)))
}

/// Get all running applications as nodes.
///
/// Returns a list of application nodes (role = Application).
/// Syntactic sugar for `query("app")`.
pub fn apps(opts: &QueryOptions) -> Result<Vec<Node>> {
    query("app", opts)
}

/// Query across all running applications.
///
/// Takes a fresh snapshot of all apps and runs the selector against it.
///
/// # Example
/// ```no_run
/// // All buttons across all apps
/// let buttons = xa11y::query("button", &xa11y::QueryOptions::default()).unwrap();
///
/// // All apps
/// let apps = xa11y::query("app", &xa11y::QueryOptions::default()).unwrap();
/// for app in &apps {
///     println!("{}: pid={:?}", app.name().unwrap_or("?"), app.pid());
/// }
/// ```
pub fn query(selector_str: &str, opts: &QueryOptions) -> Result<Vec<Node>> {
    let tree = get_provider_ref()?.get_all_apps(opts)?;
    let data = Arc::new(TreeData {
        app_name: tree.app_name,
        pid: tree.pid,
        screen_size: tree.screen_size,
        nodes: tree.nodes,
        provider: provider().ok(),
        target: None,
    });

    let selector = xa11y_core::selector::Selector::parse(selector_str)?;
    let indices = selector.match_raw_nodes(&data.nodes);
    Ok(indices
        .into_iter()
        .map(|idx| Node::new(Arc::clone(&data), idx))
        .collect())
}

/// Perform an action on an element from a specific snapshot.
#[cfg(feature = "testing")]
pub fn perform_action(
    tree: &Tree,
    node: &RawNode,
    action: Action,
    data: Option<ActionData>,
) -> Result<()> {
    get_provider_ref()?.perform_action_raw(tree, node.index, action, data)
}

/// Check if accessibility permissions are granted.
pub fn check_permissions() -> Result<PermissionStatus> {
    get_provider_ref()?.check_permissions()
}

/// List running applications with their PIDs.
///
/// Deprecated: use `apps()` instead.
pub fn list_apps() -> Result<Vec<AppInfo>> {
    get_provider_ref()?.list_apps()
}

/// Create a Locator targeting a specific application.
pub fn locator(target: AppTarget, selector: &str) -> Result<Locator> {
    Ok(Locator::new(provider()?, target, selector))
}

/// Create a Locator with custom query options.
pub fn locator_with_opts(target: AppTarget, selector: &str, opts: QueryOptions) -> Result<Locator> {
    Ok(Locator::with_opts(provider()?, target, selector, opts))
}

// ── Platform provider construction (internal) ───────────────────────────────

/// Create a new platform-appropriate accessibility provider.
///
/// Returns a fresh provider instance (not the global singleton).
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
