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
/// # Action model
///
/// Common actions are first-class methods with proper typed signatures.
/// Platform-specific or custom actions use [`perform_action`](Self::perform_action)
/// as an escape hatch — it takes a `snake_case` action name string.
///
/// Providers should check platform permissions in their constructor (`new()`)
/// and return [`Error::PermissionDenied`](crate::Error::PermissionDenied) if
/// required permissions are not granted.
pub trait Provider: Send + Sync {
    // ── Tree navigation ─────────────────────────────────────────────

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

    // ── Common actions ──────────────────────────────────────────────

    /// Click / tap / invoke the element.
    fn press(&self, element: &ElementData) -> Result<()>;

    /// Set keyboard focus to the element.
    fn focus(&self, element: &ElementData) -> Result<()>;

    /// Remove keyboard focus from the element.
    fn blur(&self, element: &ElementData) -> Result<()>;

    /// Toggle a checkbox or switch.
    fn toggle(&self, element: &ElementData) -> Result<()>;

    /// Select an item in a list, tab group, or menu.
    fn select(&self, element: &ElementData) -> Result<()>;

    /// Expand a collapsible element (combo box, tree item, disclosure).
    fn expand(&self, element: &ElementData) -> Result<()>;

    /// Collapse an expanded element.
    fn collapse(&self, element: &ElementData) -> Result<()>;

    /// Show the element's context menu or dropdown.
    fn show_menu(&self, element: &ElementData) -> Result<()>;

    /// Increment a slider or spinner by one step.
    fn increment(&self, element: &ElementData) -> Result<()>;

    /// Decrement a slider or spinner by one step.
    fn decrement(&self, element: &ElementData) -> Result<()>;

    /// Scroll the element into the visible area.
    fn scroll_into_view(&self, element: &ElementData) -> Result<()>;

    // ── Typed operations ────────────────────────────────────────────

    /// Set the text value of the element.
    fn set_value(&self, element: &ElementData, value: &str) -> Result<()>;

    /// Set the numeric value of the element (slider, spinner).
    fn set_numeric_value(&self, element: &ElementData, value: f64) -> Result<()>;

    /// Insert text at the current cursor position.
    fn type_text(&self, element: &ElementData, text: &str) -> Result<()>;

    /// Select a text range (0-based character offsets).
    fn set_text_selection(&self, element: &ElementData, start: u32, end: u32) -> Result<()>;

    // ── Generic action escape hatch ─────────────────────────────────

    /// Perform an action by `snake_case` name.
    ///
    /// This is the escape hatch for platform-specific actions not covered by
    /// the first-class methods above. The provider converts the name to the
    /// platform's convention (e.g. `"custom_thing"` → `"AXCustomThing"` on
    /// macOS) and invokes it.
    ///
    /// Well-known action names (`"press"`, `"focus"`, etc.) should also work
    /// here — providers should delegate to the corresponding method.
    fn perform_action(&self, element: &ElementData, action: &str) -> Result<()>;

    // ── Events ──────────────────────────────────────────────────────

    /// Subscribe to all accessibility events for an application.
    ///
    /// The element should be an application-level element (role=Application).
    /// The provider extracts the PID from `element.pid`.
    ///
    /// Returns a [`Subscription`] that receives events until dropped.
    fn subscribe(&self, element: &ElementData) -> Result<Subscription>;
}
