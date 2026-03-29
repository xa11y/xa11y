use std::sync::Arc;
use std::time::Duration;

use crate::action::{Action, ActionData};
use crate::element::{Element, ElementData};
use crate::error::{Error, Result};
use crate::event::ElementState;
use crate::provider::Provider;
use crate::tree::Tree;

/// A lazy element descriptor that re-resolves against a fresh accessibility
/// tree snapshot on every operation.
///
/// Inspired by Playwright's `Locator` pattern: a Locator never holds a live
/// reference to a UI element. Instead, it stores a selector and resolves it
/// on demand, making it immune to staleness.
///
/// # Example
/// ```no_run
/// # use xa11y_core::*;
/// # use std::sync::Arc;
/// # use std::time::Duration;
/// # fn example(provider: Arc<dyn Provider>) -> Result<()> {
/// let app = App::from_name(provider, "MyApp")?;
/// let save_btn = app.locator("button[name=\"Save\"]");
/// save_btn.press()?;           // snapshot → query → press
/// save_btn.wait_visible(Duration::from_secs(5))?; // poll until visible
/// save_btn.press()?;           // re-resolves against fresh snapshot
/// # Ok(())
/// # }
/// ```
pub struct Locator {
    provider: Arc<dyn Provider>,
    pid: u32,
    selector: String,
    /// Which match to select (0-based). `None` means first match.
    nth: Option<usize>,
}

/// Snapshot produced by a single resolve call.
/// Bundles the tree and the index of the matched element so that callers
/// can use both without lifetime issues.
struct Resolved {
    tree: Tree,
    element_index: u32,
}

impl Locator {
    /// Create a new Locator.
    #[doc(hidden)]
    pub fn new(provider: Arc<dyn Provider>, pid: u32, selector: &str) -> Self {
        Self {
            provider,
            pid,
            selector: selector.to_string(),
            nth: None,
        }
    }

    /// Return a new Locator that selects the nth match (0-based).
    pub fn nth(mut self, n: usize) -> Self {
        self.nth = Some(n);
        self
    }

    /// Return a new Locator that selects the first match.
    /// Equivalent to `.nth(0)` — mainly for readability.
    pub fn first(self) -> Self {
        self.nth(0)
    }

    /// Return a new Locator scoped to a direct child matching `child_selector`.
    ///
    /// Appends ` > {child_selector}` to the current selector.
    pub fn child(mut self, child_selector: &str) -> Self {
        self.selector = format!("{} > {}", self.selector, child_selector);
        self.nth = None;
        self
    }

    /// Return a new Locator scoped to a descendant matching `desc_selector`.
    ///
    /// Appends ` {desc_selector}` to the current selector.
    pub fn descendant(mut self, desc_selector: &str) -> Self {
        self.selector = format!("{} {}", self.selector, desc_selector);
        self.nth = None;
        self
    }

    /// Get the selector string.
    pub fn selector(&self) -> &str {
        &self.selector
    }

    /// Get the underlying provider.
    #[doc(hidden)]
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    /// Get the PID used by this locator.
    #[doc(hidden)]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Get the nth index, if set.
    #[doc(hidden)]
    pub fn nth_index(&self) -> Option<usize> {
        self.nth
    }

    // ── Internal resolution ─────────────────────────────────────────

    /// Snapshot the tree and resolve the selector to a single element.
    fn resolve(&self) -> Result<Resolved> {
        let tree = self.provider.get_tree(self.pid)?;
        let matches = tree.query(&self.selector)?;
        let idx = self.nth.unwrap_or(0);
        let element = matches.get(idx).ok_or_else(|| Error::SelectorNotMatched {
            selector: self.selector.clone(),
        })?;
        Ok(Resolved {
            element_index: element.index,
            tree,
        })
    }

    // ── Queries (each takes a fresh snapshot) ───────────────────────

    /// Check if a matching element exists in the current tree.
    pub fn exists(&self) -> Result<bool> {
        match self.resolve() {
            Ok(_) => Ok(true),
            Err(Error::SelectorNotMatched { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Count matching elements in the current tree.
    pub fn count(&self) -> Result<usize> {
        let tree = self.provider.get_tree(self.pid)?;
        let matches = tree.query(&self.selector)?;
        Ok(matches.len())
    }

    /// Get a single [`Element`] handle from a fresh snapshot, with snapshot navigation.
    pub fn element(&self) -> Result<Element> {
        let r = self.resolve()?;
        let tree = Arc::new(r.tree);
        Ok(Element::new(tree, r.element_index))
    }

    /// Get all matching elements from a fresh snapshot.
    ///
    /// All returned elements share the same snapshot, so parent/children
    /// navigation is consistent across the result set.
    pub fn elements(&self) -> Result<Vec<Element>> {
        let tree = self.provider.get_tree(self.pid)?;
        let indices: Vec<u32> = tree
            .query(&self.selector)?
            .iter()
            .map(|e| e.index)
            .collect();
        let tree = Arc::new(tree);
        Ok(indices
            .into_iter()
            .map(|idx| Element::new(Arc::clone(&tree), idx))
            .collect())
    }

    // ── Actions (each takes a fresh snapshot) ───────────────────────

    /// Perform an action on the matched element (internal dispatch).
    fn perform(&self, action: Action, data: Option<ActionData>) -> Result<()> {
        if let Some(ref d) = data {
            d.validate(action)?;
        }
        let r = self.resolve()?;
        let element = r
            .tree
            .get_data(r.element_index)
            .expect("element_index valid after resolve");
        self.provider.perform_action(&r.tree, element, action, data)
    }

    /// Click / invoke the matched element.
    pub fn press(&self) -> Result<()> {
        self.perform(Action::Press, None)
    }

    /// Set keyboard focus on the matched element.
    pub fn focus(&self) -> Result<()> {
        self.perform(Action::Focus, None)
    }

    /// Toggle the matched element (checkbox, switch).
    pub fn toggle(&self) -> Result<()> {
        self.perform(Action::Toggle, None)
    }

    /// Select the matched element (list item, etc.).
    pub fn select(&self) -> Result<()> {
        self.perform(Action::Select, None)
    }

    /// Expand the matched element.
    pub fn expand(&self) -> Result<()> {
        self.perform(Action::Expand, None)
    }

    /// Collapse the matched element.
    pub fn collapse(&self) -> Result<()> {
        self.perform(Action::Collapse, None)
    }

    /// Set the text value of the matched element.
    pub fn set_value(&self, value: &str) -> Result<()> {
        self.perform(Action::SetValue, Some(ActionData::Value(value.to_string())))
    }

    /// Set the numeric value of the matched element (slider, spinner).
    pub fn set_numeric_value(&self, value: f64) -> Result<()> {
        self.perform(Action::SetValue, Some(ActionData::NumericValue(value)))
    }

    /// Increment the matched element (slider, spinner).
    pub fn increment(&self) -> Result<()> {
        self.perform(Action::Increment, None)
    }

    /// Decrement the matched element (slider, spinner).
    pub fn decrement(&self) -> Result<()> {
        self.perform(Action::Decrement, None)
    }

    /// Show the context menu for the matched element.
    pub fn show_menu(&self) -> Result<()> {
        self.perform(Action::ShowMenu, None)
    }

    /// Scroll the matched element into view.
    pub fn scroll_into_view(&self) -> Result<()> {
        self.perform(Action::ScrollIntoView, None)
    }

    /// Type text character-by-character on the matched element.
    pub fn type_text(&self, text: &str) -> Result<()> {
        self.perform(Action::TypeText, Some(ActionData::Value(text.to_string())))
    }

    /// Select a text range within the matched element.
    pub fn select_text(&self, start: u32, end: u32) -> Result<()> {
        self.perform(
            Action::SetTextSelection,
            Some(ActionData::TextSelection { start, end }),
        )
    }

    /// Remove keyboard focus from the matched element.
    pub fn blur(&self) -> Result<()> {
        self.perform(Action::Blur, None)
    }

    /// Scroll the matched element upward.
    ///
    /// `amount` is in logical scroll units (≈ one mouse wheel notch).
    pub fn scroll_up(&self, amount: f64) -> Result<()> {
        self.perform(Action::ScrollDown, Some(ActionData::ScrollAmount(-amount)))
    }

    /// Scroll the matched element downward.
    ///
    /// `amount` is in logical scroll units (≈ one mouse wheel notch).
    pub fn scroll_down(&self, amount: f64) -> Result<()> {
        self.perform(Action::ScrollDown, Some(ActionData::ScrollAmount(amount)))
    }

    /// Scroll the matched element leftward.
    ///
    /// `amount` is in logical scroll units (≈ one mouse wheel notch).
    pub fn scroll_left(&self, amount: f64) -> Result<()> {
        self.perform(Action::ScrollRight, Some(ActionData::ScrollAmount(-amount)))
    }

    /// Scroll the matched element rightward.
    ///
    /// `amount` is in logical scroll units (≈ one mouse wheel notch).
    pub fn scroll_right(&self, amount: f64) -> Result<()> {
        self.perform(Action::ScrollRight, Some(ActionData::ScrollAmount(amount)))
    }

    // ── Wait operations ─────────────────────────────────────────────

    /// Wait until the element is visible, polling with fresh snapshots.
    pub fn wait_visible(&self, timeout: Duration) -> Result<Element> {
        self.wait_for_state(ElementState::Visible, timeout)
            .map(|opt| opt.expect("visible wait must return an element"))
    }

    /// Wait until the element exists in the tree.
    pub fn wait_attached(&self, timeout: Duration) -> Result<Element> {
        self.wait_for_state(ElementState::Attached, timeout)
            .map(|opt| opt.expect("attached wait must return an element"))
    }

    /// Wait until the element is removed from the tree.
    pub fn wait_detached(&self, timeout: Duration) -> Result<()> {
        self.wait_for_state(ElementState::Detached, timeout)
            .map(|_| ())
    }

    /// Wait until the element is enabled.
    pub fn wait_enabled(&self, timeout: Duration) -> Result<Element> {
        self.wait_for_state(ElementState::Enabled, timeout)
            .map(|opt| opt.expect("enabled wait must return an element"))
    }

    /// Wait until the element is disabled (exists but not enabled).
    pub fn wait_disabled(&self, timeout: Duration) -> Result<Element> {
        self.wait_for_state(ElementState::Disabled, timeout)
            .map(|opt| opt.expect("disabled wait must return an element"))
    }

    /// Wait until the element is hidden or removed.
    pub fn wait_hidden(&self, timeout: Duration) -> Result<()> {
        self.wait_for_state(ElementState::Hidden, timeout)
            .map(|_| ())
    }

    /// Wait until the element has keyboard focus.
    pub fn wait_focused(&self, timeout: Duration) -> Result<Element> {
        self.wait_for_state(ElementState::Focused, timeout)
            .map(|opt| opt.expect("focused wait must return an element"))
    }

    /// Wait until the element does not have keyboard focus.
    pub fn wait_unfocused(&self, timeout: Duration) -> Result<Element> {
        self.wait_for_state(ElementState::Unfocused, timeout)
            .map(|opt| opt.expect("unfocused wait must return an element"))
    }

    /// Wait for an [`ElementState`] condition to be met.
    pub fn wait_for_state(
        &self,
        state: ElementState,
        timeout: Duration,
    ) -> Result<Option<Element>> {
        self.poll_until(|element| state.is_met(element), timeout)
    }

    /// Wait until an arbitrary predicate is satisfied, polling with fresh
    /// snapshots at ~100 ms intervals.
    ///
    /// The predicate receives `Some(&ElementData)` when the selector matches, or
    /// `None` when no element matches. Return `true` to stop waiting.
    pub fn wait_until(
        &self,
        predicate: impl Fn(Option<&ElementData>) -> bool,
        timeout: Duration,
    ) -> Result<Option<Element>> {
        self.poll_until(&predicate, timeout)
    }

    /// Core polling loop shared by `wait_for_state` and `wait_until`.
    fn poll_until(
        &self,
        predicate: impl Fn(Option<&ElementData>) -> bool,
        timeout: Duration,
    ) -> Result<Option<Element>> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(100);

        loop {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(Error::Timeout { elapsed });
            }

            let tree = self.provider.get_tree(self.pid)?;
            let matches = tree.query(&self.selector).ok();
            let idx = self.nth.unwrap_or(0);
            let matched_index = matches.as_ref().and_then(|m| m.get(idx).map(|e| e.index));
            let element_ref = matched_index.and_then(|i| tree.get_data(i));

            if predicate(element_ref) {
                return Ok(matched_index.map(|i| Element::new(Arc::new(tree), i)));
            }

            std::thread::sleep(poll_interval);
        }
    }
}
