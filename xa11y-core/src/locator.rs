use std::sync::Arc;
use std::time::Duration;

use crate::action::{Action, ActionData, ScrollDirection};
use crate::error::{Error, Result};
use crate::event::ElementState;
use crate::node::{Node, RawNode, Rect, StateSet, TreeData};
use crate::provider::{AppTarget, Provider, QueryOptions};
use crate::role::Role;
use crate::selector::Selector;

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
/// let target = AppTarget::ByName("MyApp".into());
/// let save_btn = Locator::new(provider, target, "button[name=\"Save\"]");
/// save_btn.press()?;           // snapshot → query → press
/// save_btn.wait_visible(Duration::from_secs(5))?; // poll until visible
/// save_btn.press()?;           // re-resolves against fresh snapshot
/// # Ok(())
/// # }
/// ```
pub struct Locator {
    provider: Arc<dyn Provider>,
    target: AppTarget,
    selector: String,
    opts: QueryOptions,
    /// Which match to select (0-based). `None` means first match.
    nth: Option<usize>,
}

/// Snapshot produced by a single resolve call (wraps in Arc<TreeData>).
struct Resolved {
    data: Arc<TreeData>,
    node_index: u32,
}

/// Raw resolution for action dispatch (avoids Arc wrapping).
struct ResolvedRaw {
    tree: crate::tree::Tree,
    node_index: u32,
}

impl Locator {
    /// Create a new Locator with default query options.
    pub fn new(provider: Arc<dyn Provider>, target: AppTarget, selector: &str) -> Self {
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
        provider: Arc<dyn Provider>,
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
        let data = Arc::new(TreeData {
            app_name: tree.app_name,
            pid: tree.pid,
            screen_size: tree.screen_size,
            nodes: tree.nodes,
            provider: Some(Arc::clone(&self.provider)),
            target: Some(self.target.clone()),
        });

        let selector = Selector::parse(&self.selector)?;
        let matches = selector.match_raw_nodes(&data.nodes);
        let idx = self.nth.unwrap_or(0);
        let &node_index = matches.get(idx).ok_or_else(|| Error::SelectorNotMatched {
            selector: self.selector.clone(),
        })?;
        Ok(Resolved { data, node_index })
    }

    /// Resolve for action dispatch (returns raw Tree, no Arc wrapping).
    fn resolve_raw(&self) -> Result<ResolvedRaw> {
        let tree = self.provider.get_app_tree(&self.target, &self.opts)?;
        let selector = Selector::parse(&self.selector)?;
        let matches = selector.match_raw_nodes(&tree.nodes);
        let idx = self.nth.unwrap_or(0);
        let &node_index = matches.get(idx).ok_or_else(|| Error::SelectorNotMatched {
            selector: self.selector.clone(),
        })?;
        Ok(ResolvedRaw { tree, node_index })
    }

    /// Resolve and return a Node cursor for the matched element.
    fn resolve_node(&self) -> Result<Node> {
        let r = self.resolve()?;
        Ok(Node::new(r.data, r.node_index))
    }

    // ── Queries (each takes a fresh snapshot) ───────────────────────

    /// Get the matched element's role.
    pub fn role(&self) -> Result<Role> {
        Ok(self.resolve_node()?.role())
    }

    /// Get the matched element's name.
    pub fn name(&self) -> Result<Option<String>> {
        Ok(self.resolve_node()?.name().map(|s| s.to_string()))
    }

    /// Get the matched element's value.
    pub fn value(&self) -> Result<Option<String>> {
        Ok(self.resolve_node()?.value().map(|s| s.to_string()))
    }

    /// Get the matched element's description.
    pub fn description(&self) -> Result<Option<String>> {
        Ok(self.resolve_node()?.description().map(|s| s.to_string()))
    }

    /// Get the matched element's bounding rectangle.
    pub fn bounds(&self) -> Result<Option<Rect>> {
        Ok(self.resolve_node()?.bounds())
    }

    /// Get the matched element's state flags.
    pub fn states(&self) -> Result<StateSet> {
        Ok(self.resolve_node()?.states().clone())
    }

    /// Get the matched element's numeric value (for range controls).
    pub fn numeric_value(&self) -> Result<Option<f64>> {
        Ok(self.resolve_node()?.numeric_value())
    }

    /// Check if the matched element is visible.
    pub fn is_visible(&self) -> Result<bool> {
        Ok(self.resolve_node()?.states().visible)
    }

    /// Check if the matched element is enabled.
    pub fn is_enabled(&self) -> Result<bool> {
        Ok(self.resolve_node()?.states().enabled)
    }

    /// Check if the matched element is focused.
    pub fn is_focused(&self) -> Result<bool> {
        Ok(self.resolve_node()?.states().focused)
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
        let selector = Selector::parse(&self.selector)?;
        let matches = selector.match_raw_nodes(&tree.nodes);
        Ok(matches.len())
    }

    /// Resolve and return the first matching element as a [`Node`] snapshot.
    pub fn first(&self) -> Result<Node> {
        self.resolve_node()
    }

    /// Resolve and return all matching elements as [`Node`] snapshots.
    pub fn all(&self) -> Result<Vec<Node>> {
        let tree = self.provider.get_app_tree(&self.target, &self.opts)?;
        let data = Arc::new(TreeData {
            app_name: tree.app_name,
            pid: tree.pid,
            screen_size: tree.screen_size,
            nodes: tree.nodes,
            provider: Some(Arc::clone(&self.provider)),
            target: Some(self.target.clone()),
        });

        let selector = Selector::parse(&self.selector)?;
        let indices = selector.match_raw_nodes(&data.nodes);
        Ok(indices
            .into_iter()
            .map(|idx| Node::new(Arc::clone(&data), idx))
            .collect())
    }

    // ── Actions (each takes a fresh snapshot) ───────────────────────

    /// Perform an action on the matched element (internal dispatch).
    fn perform(&self, action: Action, data: Option<ActionData>) -> Result<()> {
        if let Some(ref d) = data {
            d.validate(action)?;
        }
        let r = self.resolve_raw()?;
        self.provider
            .perform_action_raw(&r.tree, r.node_index, action, data)
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
    pub fn scroll_up(&self, amount: f64) -> Result<()> {
        self.perform(
            Action::Scroll,
            Some(ActionData::ScrollAmount {
                direction: ScrollDirection::Up,
                amount,
            }),
        )
    }

    /// Scroll the matched element downward.
    pub fn scroll_down(&self, amount: f64) -> Result<()> {
        self.perform(
            Action::Scroll,
            Some(ActionData::ScrollAmount {
                direction: ScrollDirection::Down,
                amount,
            }),
        )
    }

    /// Scroll the matched element leftward.
    pub fn scroll_left(&self, amount: f64) -> Result<()> {
        self.perform(
            Action::Scroll,
            Some(ActionData::ScrollAmount {
                direction: ScrollDirection::Left,
                amount,
            }),
        )
    }

    /// Scroll the matched element rightward.
    pub fn scroll_right(&self, amount: f64) -> Result<()> {
        self.perform(
            Action::Scroll,
            Some(ActionData::ScrollAmount {
                direction: ScrollDirection::Right,
                amount,
            }),
        )
    }

    // ── Wait operations ─────────────────────────────────────────────

    /// Wait until the element is visible, polling with fresh snapshots.
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

    /// Wait until the element is disabled (exists but not enabled).
    pub fn wait_disabled(&self, timeout: Duration) -> Result<Node> {
        self.wait_for_state(ElementState::Disabled, timeout)
    }

    /// Wait until the element is hidden or removed.
    pub fn wait_hidden(&self, timeout: Duration) -> Result<Node> {
        self.wait_for_state(ElementState::Hidden, timeout)
    }

    /// Wait until the element has keyboard focus.
    pub fn wait_focused(&self, timeout: Duration) -> Result<Node> {
        self.wait_for_state(ElementState::Focused, timeout)
    }

    /// Wait until the element does not have keyboard focus.
    pub fn wait_unfocused(&self, timeout: Duration) -> Result<Node> {
        self.wait_for_state(ElementState::Unfocused, timeout)
    }

    /// Wait for an [`ElementState`] condition to be met.
    pub fn wait_for_state(&self, state: ElementState, timeout: Duration) -> Result<Node> {
        self.poll_until(|node| state.is_met(node), state.is_absence_state(), timeout)
    }

    /// Wait until an arbitrary predicate is satisfied, polling with fresh
    /// snapshots at ~100 ms intervals.
    ///
    /// The predicate receives `Some(&RawNode)` when the selector matches, or
    /// `None` when no element matches. Return `true` to stop waiting.
    pub fn wait_until(
        &self,
        predicate: impl Fn(Option<&RawNode>) -> bool,
        timeout: Duration,
    ) -> Result<Node> {
        self.poll_until(&predicate, false, timeout)
    }

    /// Core polling loop shared by `wait_for_state` and `wait_until`.
    fn poll_until(
        &self,
        predicate: impl Fn(Option<&RawNode>) -> bool,
        allow_absent: bool,
        timeout: Duration,
    ) -> Result<Node> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(100);

        loop {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(Error::Timeout { elapsed });
            }

            let tree = self.provider.get_app_tree(&self.target, &self.opts)?;
            let selector = Selector::parse(&self.selector).ok();
            let matches = selector.as_ref().map(|s| s.match_raw_nodes(&tree.nodes));
            let idx = self.nth.unwrap_or(0);
            let node_index = matches.as_ref().and_then(|m| m.get(idx).copied());
            let node = node_index.and_then(|i| tree.nodes.get(i as usize));

            if predicate(node) {
                let data = Arc::new(TreeData {
                    app_name: tree.app_name,
                    pid: tree.pid,
                    screen_size: tree.screen_size,
                    nodes: tree.nodes,
                    provider: Some(Arc::clone(&self.provider)),
                    target: Some(self.target.clone()),
                });

                return if allow_absent {
                    Ok(node_index
                        .map(|idx| Node::new(Arc::clone(&data), idx))
                        .unwrap_or_else(|| {
                            // Can't mutate Arc'd data, so create a standalone node
                            Node::new(
                                Arc::new(TreeData {
                                    app_name: data.app_name.clone(),
                                    pid: data.pid,
                                    screen_size: data.screen_size,
                                    nodes: vec![RawNode::synthetic_empty()],
                                    provider: Some(Arc::clone(&self.provider)),
                                    target: Some(self.target.clone()),
                                }),
                                0,
                            )
                        }))
                } else {
                    Ok(Node::new(
                        data,
                        node_index.expect("predicate requires node to exist"),
                    ))
                };
            }

            std::thread::sleep(poll_interval);
        }
    }
}
