use std::sync::Arc;
use std::time::Duration;

use crate::action::{Action, ActionData};
use crate::element::{Element, ElementData};
use crate::error::{Error, Result};
use crate::event::ElementState;
use crate::provider::Provider;
use crate::selector::Selector;

/// A lazy element descriptor that re-resolves against a fresh accessibility
/// tree on every operation.
///
/// Inspired by Playwright's `Locator` pattern: a Locator never holds a live
/// reference to a UI element. Instead, it stores a selector and resolves it
/// on demand, making it immune to staleness.
///
/// # Example
/// ```no_run
/// # use xa11y_core::*;
/// # use std::sync::Arc;
/// # fn example(provider: Arc<dyn Provider>) -> Result<()> {
/// let app = App::by_name(provider, "MyApp")?;
/// let save_btn = app.locator(r#"button[name="Save"]"#);
/// save_btn.press()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Locator {
    provider: Arc<dyn Provider>,
    /// Root element for scoped searches. `None` = system root (all apps).
    root: Option<ElementData>,
    selector: String,
    /// Which match to select (0-based). `None` means first match.
    nth: Option<usize>,
}

impl Locator {
    /// Create a new Locator scoped to a root element.
    ///
    /// Use [`App::locator`](crate::App::locator) to create locators.
    pub(crate) fn new(
        provider: Arc<dyn Provider>,
        root: Option<ElementData>,
        selector: &str,
    ) -> Self {
        Self {
            provider,
            root,
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

    /// Get the root element data, if scoped.
    #[doc(hidden)]
    pub fn root(&self) -> Option<&ElementData> {
        self.root.as_ref()
    }

    /// Get the nth index, if set.
    #[doc(hidden)]
    pub fn nth_index(&self) -> Option<usize> {
        self.nth
    }

    // ── Internal resolution ─────────────────────────────────────────

    /// Resolve the selector to a single ElementData.
    fn resolve_data(&self) -> Result<ElementData> {
        let selector = Selector::parse(&self.selector)?;
        let matches = self.provider.find_elements(
            self.root.as_ref(),
            &selector,
            // Fetch enough to satisfy nth
            Some(self.nth.unwrap_or(0) + 1),
            None,
        )?;
        let idx = self.nth.unwrap_or(0);
        matches
            .into_iter()
            .nth(idx)
            .ok_or_else(|| Error::SelectorNotMatched {
                selector: self.selector.clone(),
            })
    }

    // ── Queries (each re-queries the provider) ─────────────────────

    /// Check if a matching element exists.
    pub fn exists(&self) -> Result<bool> {
        match self.resolve_data() {
            Ok(_) => Ok(true),
            Err(Error::SelectorNotMatched { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Count matching elements.
    pub fn count(&self) -> Result<usize> {
        let selector = Selector::parse(&self.selector)?;
        let matches = self
            .provider
            .find_elements(self.root.as_ref(), &selector, None, None)?;
        Ok(matches.len())
    }

    /// Get a single [`Element`] handle.
    pub fn element(&self) -> Result<Element> {
        let data = self.resolve_data()?;
        Ok(Element::new(data, Arc::clone(&self.provider)))
    }

    /// Get all matching elements.
    pub fn elements(&self) -> Result<Vec<Element>> {
        let selector = Selector::parse(&self.selector)?;
        let matches = self
            .provider
            .find_elements(self.root.as_ref(), &selector, None, None)?;
        Ok(matches
            .into_iter()
            .map(|d| Element::new(d, Arc::clone(&self.provider)))
            .collect())
    }

    // ── Actions (each re-queries the provider) ─────────────────────

    /// Perform an action on the matched element (internal dispatch).
    fn perform(&self, action: Action, data: Option<ActionData>) -> Result<()> {
        if let Some(ref d) = data {
            d.validate(action)?;
        }
        let element = self.resolve_data()?;
        self.provider.perform_action(&element, action, data)
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
        self.perform(Action::ScrollDown, Some(ActionData::ScrollAmount(-amount)))
    }

    /// Scroll the matched element downward.
    pub fn scroll_down(&self, amount: f64) -> Result<()> {
        self.perform(Action::ScrollDown, Some(ActionData::ScrollAmount(amount)))
    }

    /// Scroll the matched element leftward.
    pub fn scroll_left(&self, amount: f64) -> Result<()> {
        self.perform(Action::ScrollRight, Some(ActionData::ScrollAmount(-amount)))
    }

    /// Scroll the matched element rightward.
    pub fn scroll_right(&self, amount: f64) -> Result<()> {
        self.perform(Action::ScrollRight, Some(ActionData::ScrollAmount(amount)))
    }

    // ── Wait operations ─────────────────────────────────────────────

    /// Wait until the element is visible, polling the provider.
    pub fn wait_visible(&self, timeout: Duration) -> Result<Element> {
        self.wait_for_state(ElementState::Visible, timeout)
            .map(|opt| opt.expect("visible wait must return an element"))
    }

    /// Wait until the element exists.
    pub fn wait_attached(&self, timeout: Duration) -> Result<Element> {
        self.wait_for_state(ElementState::Attached, timeout)
            .map(|opt| opt.expect("attached wait must return an element"))
    }

    /// Wait until the element is removed.
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

    /// Wait until an arbitrary predicate is satisfied, polling at ~100 ms intervals.
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

            let matched = match self.resolve_data() {
                Ok(data) => Some(data),
                Err(Error::SelectorNotMatched { .. }) => None,
                Err(e) => return Err(e),
            };

            if predicate(matched.as_ref()) {
                return Ok(matched.map(|data| Element::new(data, Arc::clone(&self.provider))));
            }

            std::thread::sleep(poll_interval);
        }
    }
}
