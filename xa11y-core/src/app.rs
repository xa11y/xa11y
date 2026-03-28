use std::sync::Arc;

use crate::error::Result;
use crate::locator::Locator;
use crate::node::Node;
use crate::provider::{AppTarget, Provider};
use crate::role::Role;

/// A handle to a running application.
///
/// `App` is the primary entry point for interacting with an application's
/// accessibility tree. It holds a reference to the platform provider and
/// the target application identity.
///
/// Use [`locator()`](App::locator) to create action-capable element references,
/// or [`nodes()`](App::nodes) to take a snapshot of the tree for inspection.
pub struct App {
    provider: Arc<dyn Provider>,
    target: AppTarget,
    app_name: String,
    pid: Option<u32>,
}

impl App {
    /// Create a new App handle.
    ///
    /// Typically constructed via [`xa11y::app()`] rather than directly.
    pub fn new(
        provider: Arc<dyn Provider>,
        target: AppTarget,
        app_name: String,
        pid: Option<u32>,
    ) -> Self {
        Self {
            provider,
            target,
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

    /// The target used to identify this application.
    pub fn target(&self) -> &AppTarget {
        &self.target
    }

    /// Get the underlying provider (for Python bindings and internal use).
    #[doc(hidden)]
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    /// Create a [`Locator`] for lazy element interaction.
    ///
    /// The locator re-resolves against a fresh tree snapshot on every
    /// operation, making it immune to staleness.
    pub fn locator(&self, selector: &str) -> Locator {
        Locator::new(Arc::clone(&self.provider), self.target.clone(), selector)
    }

    /// Snapshot the application's accessibility tree.
    ///
    /// Returns the root [`Node`] of the snapshot. Navigate with
    /// `children()` and `parent()` — all within the same consistent snapshot.
    pub fn nodes(&self) -> Result<Node> {
        let tree = self.provider.get_app_tree(&self.target)?;
        let tree = Arc::new(tree);
        Ok(Node::new(tree, 0))
    }

    /// List all running applications (static helper).
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
                let target = match child.pid {
                    Some(pid) => AppTarget::ByPid(pid),
                    None => AppTarget::ByName(name.clone()),
                };
                Some(App::new(Arc::clone(&provider), target, name, child.pid))
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
            .field("target", &self.target)
            .finish()
    }
}
