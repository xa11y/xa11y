use std::sync::Arc;

use crate::error::Result;
use crate::locator::Locator;
use crate::node::Node;
use crate::provider::{AppLookup, Provider, WindowHandle};
use crate::role::Role;

/// A handle to a running application.
///
/// `App` is the primary entry point for interacting with an application's
/// accessibility tree. It holds a reference to the platform provider and
/// the target application identity.
///
/// # Construction
///
/// Use the named constructors to get an `App` handle:
///
/// - [`App::from_name`] — find by display name (case-insensitive substring match)
/// - [`App::from_pid`] — find by process ID
/// - [`App::from_window`] — find by platform-specific window handle
/// - [`App::all`] — list all running applications
///
/// Use [`locator()`](App::locator) to create action-capable element references,
/// or [`nodes()`](App::nodes) to take a snapshot of the tree for inspection.
#[derive(Clone)]
pub struct App {
    provider: Arc<dyn Provider>,
    lookup: AppLookup,
    app_name: String,
    pid: Option<u32>,
}

impl App {
    /// Find an application by display name.
    ///
    /// Performs a case-insensitive substring match against running applications.
    /// Validates the app exists and caches its metadata.
    pub fn from_name(provider: Arc<dyn Provider>, name: &str) -> Result<Self> {
        let lookup = AppLookup::ByName(name.to_string());
        let tree = lookup.fetch_tree(&*provider)?;
        Ok(Self {
            provider,
            lookup,
            app_name: tree.app_name.clone(),
            pid: tree.pid,
        })
    }

    /// Find an application by process ID.
    ///
    /// Validates the app exists and caches its metadata.
    pub fn from_pid(provider: Arc<dyn Provider>, pid: u32) -> Result<Self> {
        let lookup = AppLookup::ByPid(pid);
        let tree = lookup.fetch_tree(&*provider)?;
        Ok(Self {
            provider,
            lookup,
            app_name: tree.app_name.clone(),
            pid: tree.pid,
        })
    }

    /// Find an application by platform-specific window handle.
    ///
    /// Validates the app exists and caches its metadata.
    pub fn from_window(provider: Arc<dyn Provider>, handle: WindowHandle) -> Result<Self> {
        let lookup = AppLookup::ByWindow(handle);
        let tree = lookup.fetch_tree(&*provider)?;
        Ok(Self {
            provider,
            lookup,
            app_name: tree.app_name.clone(),
            pid: tree.pid,
        })
    }

    /// Internal constructor for pre-validated apps (e.g. from `all()`).
    #[doc(hidden)]
    pub fn new(
        provider: Arc<dyn Provider>,
        lookup: AppLookup,
        app_name: String,
        pid: Option<u32>,
    ) -> Self {
        Self {
            provider,
            lookup,
            app_name,
            pid,
        }
    }

    /// The application's display name (cached from initial discovery).
    pub fn name(&self) -> &str {
        &self.app_name
    }

    /// The application's process ID (cached from initial discovery).
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Get the underlying provider (for Python bindings and internal use).
    #[doc(hidden)]
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    /// Get the internal lookup key (for Python bindings and internal use).
    #[doc(hidden)]
    pub fn lookup(&self) -> &AppLookup {
        &self.lookup
    }

    /// Create a [`Locator`] for lazy element interaction.
    ///
    /// The locator re-resolves against a fresh tree snapshot on every
    /// operation, making it immune to staleness.
    pub fn locator(&self, selector: &str) -> Locator {
        Locator::new(Arc::clone(&self.provider), self.lookup.clone(), selector)
    }

    /// Snapshot the application's accessibility tree.
    ///
    /// Returns the root [`Node`] of the snapshot. Navigate with
    /// `children()` and `parent()` — all within the same consistent snapshot.
    pub fn nodes(&self) -> Result<Node> {
        let tree = self.lookup.fetch_tree(&*self.provider)?;
        let tree = Arc::new(tree);
        Ok(Node::new(tree, 0))
    }

    /// List all running applications.
    ///
    /// Returns one `App` per discovered application. Each can then be used
    /// for locators and snapshots.
    pub fn all(provider: Arc<dyn Provider>) -> Result<Vec<App>> {
        let tree = provider.get_apps()?;
        let root = tree.root_data();
        let apps = tree
            .children_data(root)
            .into_iter()
            .filter(|child| child.role == Role::Application)
            .filter_map(|child| {
                let name = child.name.clone()?;
                let lookup = match child.pid {
                    Some(pid) => AppLookup::ByPid(pid),
                    None => AppLookup::ByName(name.clone()),
                };
                Some(App::new(Arc::clone(&provider), lookup, name, child.pid))
            })
            .collect();
        Ok(apps)
    }
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("name", &self.app_name)
            .field("pid", &self.pid)
            .field("lookup", &self.lookup)
            .finish()
    }
}
