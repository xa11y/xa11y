use std::sync::Arc;

use crate::element::Element;
use crate::error::Result;
use crate::event_provider::Subscription;
use crate::locator::Locator;
use crate::provider::Provider;
use crate::role::Role;

/// A handle to a running application.
///
/// `App` is the primary entry point for interacting with an application's
/// accessibility tree. Internally identified by PID — even when constructed
/// by name, the name is resolved to a PID at construction time.
///
/// # Construction
///
/// - [`App::from_name`] — find by display name (case-insensitive substring match)
/// - [`App::from_pid`] — find by process ID
/// - [`App::all`] — list all running applications
///
/// Use [`locator()`](App::locator) to create action-capable element references,
/// or [`elements()`](App::elements) to take a snapshot of the tree for inspection.
#[derive(Clone)]
pub struct App {
    provider: Arc<dyn Provider>,
    pid: u32,
    app_name: String,
}

impl App {
    /// Find an application by display name.
    ///
    /// Performs a case-insensitive substring match against running applications.
    /// Resolves to a PID at construction time.
    pub fn from_name(provider: Arc<dyn Provider>, name: &str) -> Result<Self> {
        let pid = provider.resolve_pid_by_name(name)?;
        let tree = provider.get_tree(pid)?;
        Ok(Self {
            provider,
            pid,
            app_name: tree.app_name.clone(),
        })
    }

    /// Find an application by process ID.
    ///
    /// Validates the app exists and caches its metadata.
    pub fn from_pid(provider: Arc<dyn Provider>, pid: u32) -> Result<Self> {
        let tree = provider.get_tree(pid)?;
        Ok(Self {
            provider,
            pid,
            app_name: tree.app_name.clone(),
        })
    }

    /// Internal constructor for pre-validated apps (e.g. from `all()`).
    #[doc(hidden)]
    pub fn new(provider: Arc<dyn Provider>, pid: u32, app_name: String) -> Self {
        Self {
            provider,
            pid,
            app_name,
        }
    }

    /// The application's display name (cached from initial discovery).
    pub fn name(&self) -> &str {
        &self.app_name
    }

    /// The application's process ID.
    pub fn pid(&self) -> u32 {
        self.pid
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
        Locator::new(Arc::clone(&self.provider), self.pid, selector)
    }

    /// Snapshot the application's accessibility tree.
    ///
    /// Returns the root [`Element`] of the snapshot. Navigate with
    /// `children()` and `parent()` — all within the same consistent snapshot.
    pub fn elements(&self) -> Result<Element> {
        let tree = self.provider.get_tree(self.pid)?;
        let tree = Arc::new(tree);
        Ok(Element::new(tree, 0))
    }

    /// Subscribe to all accessibility events for this application.
    ///
    /// Returns a [`Subscription`] that receives events until dropped.
    ///
    /// ```no_run
    /// # use xa11y_core::*;
    /// # fn example(app: &App) -> Result<()> {
    /// let sub = app.subscribe()?;
    /// let event = sub.wait_for(
    ///     |e| e.event_type == EventType::FocusChanged,
    ///     std::time::Duration::from_secs(5),
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn subscribe(&self) -> Result<Subscription> {
        self.provider.subscribe(self.pid)
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
                let pid = child.pid?;
                Some(App::new(Arc::clone(&provider), pid, name))
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
            .finish()
    }
}
