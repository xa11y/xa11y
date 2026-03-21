use std::time::Duration;

use crate::action::{Action, ActionData};
use crate::error::{Error, Result};
use crate::event::ElementState;
use crate::node::{Node, Rect, StateSet};
use crate::provider::{AppTarget, Provider, QueryOptions};
use crate::role::Role;
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
/// # fn example(provider: &dyn Provider) -> Result<()> {
/// let target = AppTarget::ByName("MyApp".into());
/// let save_btn = Locator::new(provider, target, "button[name=\"Save\"]");
/// save_btn.press()?;           // snapshot → query → press
/// save_btn.wait_visible(Duration::from_secs(5))?; // poll until visible
/// save_btn.press()?;           // re-resolves against fresh snapshot
/// # Ok(())
/// # }
/// ```
pub struct Locator<'p> {
    provider: &'p dyn Provider,
    target: AppTarget,
    selector: String,
    opts: QueryOptions,
    /// Which match to select (0-based). `None` means first match.
    nth: Option<usize>,
}

/// Snapshot produced by a single resolve call.
/// Bundles the tree and the index of the matched node so that callers
/// can use both without lifetime issues.
struct Resolved {
    tree: Tree,
    node_index: u32,
}

impl<'p> Locator<'p> {
    /// Create a new Locator with default query options.
    pub fn new(provider: &'p dyn Provider, target: AppTarget, selector: &str) -> Self {
        Self {
            provider,
            target,
            selector: selector.to_string(),
            opts: QueryOptions::default(),
            nth: None,
        }
    }

    /// Create a new Locator with custom query options.
    pub fn with_opts(
        provider: &'p dyn Provider,
        target: AppTarget,
        selector: &str,
        opts: QueryOptions,
    ) -> Self {
        Self {
            provider,
            target,
            selector: selector.to_string(),
            opts,
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

    // ── Internal resolution ─────────────────────────────────────────

    /// Snapshot the tree and resolve the selector to a single node.
    fn resolve(&self) -> Result<Resolved> {
        let tree = self.provider.get_app_tree(&self.target, &self.opts)?;
        let matches = tree.query(&self.selector)?;
        let idx = self.nth.unwrap_or(0);
        let node = matches.get(idx).ok_or_else(|| Error::SelectorNotMatched {
            selector: self.selector.clone(),
        })?;
        Ok(Resolved {
            node_index: node.index,
            tree,
        })
    }

    /// Resolve and return a clone of the matched node.
    /// This is the safe way to get node data out without lifetime issues.
    fn resolve_node(&self) -> Result<Node> {
        let r = self.resolve()?;
        Ok(r.tree
            .get(r.node_index)
            .expect("node_index valid after resolve")
            .clone())
    }

    // ── Queries (each takes a fresh snapshot) ───────────────────────

    /// Get the matched element's role.
    pub fn role(&self) -> Result<Role> {
        Ok(self.resolve_node()?.role)
    }

    /// Get the matched element's name.
    pub fn name(&self) -> Result<Option<String>> {
        Ok(self.resolve_node()?.name)
    }

    /// Get the matched element's value.
    pub fn value(&self) -> Result<Option<String>> {
        Ok(self.resolve_node()?.value)
    }

    /// Get the matched element's description.
    pub fn description(&self) -> Result<Option<String>> {
        Ok(self.resolve_node()?.description)
    }

    /// Get the matched element's bounding rectangle.
    pub fn bounds(&self) -> Result<Option<Rect>> {
        Ok(self.resolve_node()?.bounds)
    }

    /// Get the matched element's state flags.
    pub fn states(&self) -> Result<StateSet> {
        Ok(self.resolve_node()?.states)
    }

    /// Get the matched element's numeric value (for range controls).
    pub fn numeric_value(&self) -> Result<Option<f64>> {
        Ok(self.resolve_node()?.numeric_value)
    }

    /// Check if the matched element is visible.
    pub fn is_visible(&self) -> Result<bool> {
        Ok(self.resolve_node()?.states.visible)
    }

    /// Check if the matched element is enabled.
    pub fn is_enabled(&self) -> Result<bool> {
        Ok(self.resolve_node()?.states.enabled)
    }

    /// Check if the matched element is focused.
    pub fn is_focused(&self) -> Result<bool> {
        Ok(self.resolve_node()?.states.focused)
    }

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
        let tree = self.provider.get_app_tree(&self.target, &self.opts)?;
        let matches = tree.query(&self.selector)?;
        Ok(matches.len())
    }

    /// Get a clone of the matched node from a fresh snapshot.
    pub fn get(&self) -> Result<Node> {
        self.resolve_node()
    }

    // ── Actions (each takes a fresh snapshot) ───────────────────────

    /// Perform an arbitrary action on the matched element.
    pub fn perform(&self, action: Action, data: Option<ActionData>) -> Result<()> {
        let r = self.resolve()?;
        let node = r
            .tree
            .get(r.node_index)
            .expect("node_index valid after resolve");
        self.provider.perform_action(&r.tree, node, action, data)
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

    // ── Wait operations ─────────────────────────────────────────────

    /// Wait until the element is visible, polling with fresh snapshots.
    ///
    /// If the provider implements `EventProvider`, delegates to its
    /// `wait_for` method. Otherwise, polls at ~100ms intervals.
    pub fn wait_visible(&self, timeout: Duration) -> Result<Node> {
        self.wait_for_state(ElementState::Visible, timeout)
    }

    /// Wait until the element exists in the tree.
    pub fn wait_attached(&self, timeout: Duration) -> Result<Node> {
        self.wait_for_state(ElementState::Attached, timeout)
    }

    /// Wait until the element is removed from the tree.
    pub fn wait_detached(&self, timeout: Duration) -> Result<Node> {
        self.wait_for_state(ElementState::Detached, timeout)
    }

    /// Wait until the element is enabled.
    pub fn wait_enabled(&self, timeout: Duration) -> Result<Node> {
        self.wait_for_state(ElementState::Enabled, timeout)
    }

    /// Wait until the element is hidden or removed.
    pub fn wait_hidden(&self, timeout: Duration) -> Result<Node> {
        self.wait_for_state(ElementState::Hidden, timeout)
    }

    fn wait_for_state(&self, state: ElementState, timeout: Duration) -> Result<Node> {
        // Try to downcast to EventProvider for native wait support.
        // Since we can't downcast trait objects, fall back to polling.
        self.poll_for_state(state, timeout)
    }

    /// Poll-based wait: repeatedly snapshot + check until state is reached or timeout.
    fn poll_for_state(&self, state: ElementState, timeout: Duration) -> Result<Node> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(100);

        loop {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(Error::Timeout { elapsed });
            }

            let tree = self.provider.get_app_tree(&self.target, &self.opts)?;
            let matches = tree.query(&self.selector).ok();
            let idx = self.nth.unwrap_or(0);
            let node = matches.as_ref().and_then(|m| m.get(idx).copied());

            let condition_met = match state {
                ElementState::Attached => node.is_some(),
                ElementState::Detached => node.is_none(),
                ElementState::Visible => node.is_some_and(|n| n.states.visible),
                ElementState::Hidden => node.is_none() || node.is_some_and(|n| !n.states.visible),
                ElementState::Enabled => node.is_some_and(|n| n.states.enabled),
            };

            if condition_met {
                return match state {
                    ElementState::Detached | ElementState::Hidden => {
                        // For "gone" states, return the last known node or a synthetic one
                        Ok(node.cloned().unwrap_or_else(|| Node {
                            role: Role::Unknown,
                            name: None,
                            value: None,
                            description: None,
                            bounds: None,
                            bounds_normalized: None,
                            actions: vec![],
                            states: StateSet::default(),
                            depth: 0,
                            numeric_value: None,
                            min_value: None,
                            max_value: None,
                            stable_id: None,
                            raw: None,
                            index: 0,
                            children_indices: vec![],
                            parent_index: None,
                        }))
                    }
                    _ => Ok(node
                        .expect("node exists for attached/visible/enabled")
                        .clone()),
                };
            }

            std::thread::sleep(poll_interval);
        }
    }
}

/// Extension trait to create Locators directly from a Provider.
pub trait ProviderExt {
    /// Create a Locator targeting a specific application.
    fn locator(&self, target: AppTarget, selector: &str) -> Locator<'_>;

    /// Create a Locator with custom query options.
    fn locator_with_opts(
        &self,
        target: AppTarget,
        selector: &str,
        opts: QueryOptions,
    ) -> Locator<'_>;
}

impl ProviderExt for dyn Provider + '_ {
    fn locator(&self, target: AppTarget, selector: &str) -> Locator<'_> {
        Locator::new(self, target, selector)
    }

    fn locator_with_opts(
        &self,
        target: AppTarget,
        selector: &str,
        opts: QueryOptions,
    ) -> Locator<'_> {
        Locator::with_opts(self, target, selector, opts)
    }
}

impl<P: Provider> ProviderExt for P {
    fn locator(&self, target: AppTarget, selector: &str) -> Locator<'_> {
        Locator::new(self, target, selector)
    }

    fn locator_with_opts(
        &self,
        target: AppTarget,
        selector: &str,
        opts: QueryOptions,
    ) -> Locator<'_> {
        Locator::with_opts(self, target, selector, opts)
    }
}
