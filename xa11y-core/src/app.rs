use std::sync::Arc;

use crate::element::{Element, ElementData};
use crate::error::{Error, Result};
use crate::event_provider::Subscription;
use crate::locator::Locator;
use crate::provider::Provider;
use crate::selector::Selector;

/// A running application, the entry point for accessibility queries.
///
/// `App` is **not** an [`Element`] — it represents the application as a whole
/// and provides a [`locator`](App::locator) to search its accessibility tree.
///
/// # Example
/// ```no_run
/// # use xa11y_core::*;
/// # use std::sync::Arc;
/// # fn example(provider: Arc<dyn Provider>) -> Result<()> {
/// let app = App::by_name(provider, "Safari")?;
/// app.locator(r#"button[name="OK"]"#).press()?;
/// # Ok(())
/// # }
/// ```
pub struct App {
    /// Application name.
    pub name: String,
    /// Process ID.
    pub pid: Option<u32>,
    /// The underlying element data for this application.
    pub data: ElementData,
    provider: Arc<dyn Provider>,
}

impl App {
    /// Find an application by exact name.
    ///
    /// Returns [`Error::PermissionDenied`] if the platform provider was created
    /// without required permissions.
    pub fn by_name(provider: Arc<dyn Provider>, name: &str) -> Result<Self> {
        let selector_str = format!(r#"application[name="{}"]"#, name);
        let selector = Selector::parse(&selector_str)?;
        let results = provider.find_elements(None, &selector, Some(1), Some(0))?;
        let data = results
            .into_iter()
            .next()
            .ok_or(Error::SelectorNotMatched {
                selector: selector_str,
            })?;
        Ok(Self::from_data(provider, data))
    }

    /// Find an application by process ID.
    pub fn by_pid(provider: Arc<dyn Provider>, pid: u32) -> Result<Self> {
        let selector_str = "application";
        let selector = Selector::parse(selector_str)?;
        let results = provider.find_elements(None, &selector, None, Some(0))?;
        let data =
            results
                .into_iter()
                .find(|d| d.pid == Some(pid))
                .ok_or(Error::SelectorNotMatched {
                    selector: format!("application with pid={}", pid),
                })?;
        Ok(Self::from_data(provider, data))
    }

    /// List all running applications.
    pub fn list(provider: Arc<dyn Provider>) -> Result<Vec<Self>> {
        let selector = Selector::parse("application")?;
        let results = provider.find_elements(None, &selector, None, Some(0))?;
        Ok(results
            .into_iter()
            .map(|d| Self::from_data(Arc::clone(&provider), d))
            .collect())
    }

    fn from_data(provider: Arc<dyn Provider>, data: ElementData) -> Self {
        let name = data.name.clone().unwrap_or_default();
        let pid = data.pid;
        Self {
            name,
            pid,
            data,
            provider,
        }
    }

    /// Create a [`Locator`] to search this application's accessibility tree.
    pub fn locator(&self, selector: &str) -> Locator {
        Locator::new(
            Arc::clone(&self.provider),
            Some(self.data.clone()),
            selector,
        )
    }

    /// Subscribe to accessibility events from this application.
    pub fn subscribe(&self) -> Result<Subscription> {
        self.provider.subscribe(&self.data)
    }

    /// Get direct children (typically windows) of this application.
    pub fn children(&self) -> Result<Vec<Element>> {
        let children = self.provider.get_children(Some(&self.data))?;
        Ok(children
            .into_iter()
            .map(|d| Element::new(d, Arc::clone(&self.provider)))
            .collect())
    }

    /// Get the provider reference.
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }
}

impl std::fmt::Display for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "application \"{}\"", self.name)
    }
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("name", &self.name)
            .field("pid", &self.pid)
            .finish()
    }
}
