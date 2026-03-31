use crate::action::{Action, ActionData};
use crate::element::ElementData;
use crate::error::Result;
use crate::event_provider::Subscription;
use crate::selector::Selector;

/// Platform backend trait for accessibility tree access.
///
/// Providers implement lazy, on-demand tree navigation. Elements are identified
/// by their [`ElementData`] (which contains a provider-specific `handle` for
/// looking up the underlying platform object).
///
/// Providers should check platform permissions in their constructor (`new()`)
/// and return [`Error::PermissionDenied`](crate::Error::PermissionDenied) if
/// required permissions are not granted.
pub trait Provider: Send + Sync {
    /// Get direct children of an element.
    ///
    /// If `element` is `None`, returns top-level application elements.
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>>;

    /// Get the parent of an element.
    ///
    /// Returns `None` for top-level (application) elements.
    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>>;

    /// Search for elements matching a selector.
    ///
    /// The selector is already parsed by the core — providers match against it
    /// during traversal and can prune subtrees that can't match.
    ///
    /// If `root` is `None`, searches from the system root (all applications).
    /// If `limit` is `Some(n)`, stops after finding `n` matches.
    /// If `max_depth` is `Some(d)`, does not descend deeper than `d` levels.
    ///
    /// The default implementation traverses via [`get_children`](Self::get_children).
    fn find_elements(
        &self,
        root: Option<&ElementData>,
        selector: &Selector,
        limit: Option<usize>,
        max_depth: Option<u32>,
    ) -> Result<Vec<ElementData>> {
        crate::selector::find_elements_in_tree(
            |el| self.get_children(el),
            root,
            selector,
            limit,
            max_depth,
        )
    }

    /// Perform an action on an element.
    ///
    /// `Ok(())` means the platform API accepted the request without error.
    /// It does **not** guarantee the action had an observable effect — use
    /// `Locator::wait_*` methods to verify state changes.
    fn perform_action(
        &self,
        element: &ElementData,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()>;

    /// Subscribe to all accessibility events for an application.
    ///
    /// The element should be an application-level element (role=Application).
    /// The provider extracts the PID from `element.pid`.
    ///
    /// Returns a [`Subscription`] that receives events until dropped.
    fn subscribe(&self, element: &ElementData) -> Result<Subscription>;
}
