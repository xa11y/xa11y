use std::sync::Arc;
use std::time::Duration;

use crate::element::{Element, ElementData};
use crate::error::{Error, Result};
use crate::event::ElementState;
use crate::provider::Provider;
use crate::selector::{chain_combinator, matches_simple, SelectorGroup};

/// A lazy element descriptor that re-resolves against a fresh accessibility
/// tree on every operation.
///
/// Inspired by Playwright's `Locator` pattern: a Locator never holds a live
/// reference to a UI element. Instead, it stores a selector and resolves it
/// on demand, making it immune to staleness.
///
/// # Example
/// ```ignore
/// # use std::time::Duration;
/// # use xa11y::*;
/// # fn example() -> Result<()> {
/// let app = App::by_name("MyApp", Duration::from_secs(5))?;
/// let save_btn = app.locator(r#"button[name="Save"]"#);
/// save_btn.press()?;
/// # Ok(())
/// # }
/// ```
/// Default auto-wait timeout for Locator action methods (5 seconds).
const DEFAULT_ACTION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct Locator {
    provider: Arc<dyn Provider>,
    /// Root element for scoped searches. `None` = system root (all apps).
    root: Option<ElementData>,
    selector: String,
    /// Which match to select (0-based). `None` means first match.
    nth: Option<usize>,
    /// Timeout for auto-wait before action methods.
    timeout: Duration,
}

impl Locator {
    /// Create a new Locator.
    ///
    /// Pass `root: None` to search the entire accessibility tree, or
    /// `Some(element)` to scope the search to that element's subtree.
    pub fn new(provider: Arc<dyn Provider>, root: Option<ElementData>, selector: &str) -> Self {
        Self {
            provider,
            root,
            selector: selector.to_string(),
            nth: None,
            timeout: DEFAULT_ACTION_TIMEOUT,
        }
    }

    /// Return a new Locator with a custom auto-wait timeout for action methods.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Return a new Locator that selects the nth match (1-based).
    ///
    /// # Panics
    /// Panics if `n` is 0. Use `.first()` or `.nth(1)` for the first match.
    pub fn nth(mut self, n: usize) -> Self {
        assert!(n > 0, "Locator::nth() is 1-based, got 0");
        self.nth = Some(n - 1); // store 0-based internally
        self
    }

    /// Return a new Locator that selects the first match.
    pub fn first(self) -> Self {
        self.nth(1)
    }

    /// Return a new Locator scoped to a direct child matching `child_selector`.
    ///
    /// Appends ` > {child_selector}` to the current selector. If either side
    /// is a comma-separated selector group, the combinator distributes over
    /// every clause: e.g. `"a, b".child("c") => "a > c, b > c"`.
    pub fn child(mut self, child_selector: &str) -> Self {
        self.selector = chain_combinator(&self.selector, " > ", child_selector);
        self.nth = None;
        self
    }

    /// Return a new Locator scoped to a descendant matching `desc_selector`.
    ///
    /// Appends ` {desc_selector}` to the current selector. If either side is
    /// a comma-separated selector group, the combinator distributes over
    /// every clause: e.g. `"a, b".descendant("c") => "a c, b c"`.
    pub fn descendant(mut self, desc_selector: &str) -> Self {
        self.selector = chain_combinator(&self.selector, " ", desc_selector);
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

    /// Resolve `group` to a flat, doc-order, deduped list of matches.
    ///
    /// When `self.root.is_some()`, this delegates straight to
    /// `Provider::find_elements_group`. When `self.root.is_none()`, the
    /// `Provider` trait no longer supports a rootless search — discovery and
    /// search are now separate primitives. We replicate the legacy
    /// "search across all apps" semantics here:
    ///
    /// 1. Enumerate apps via `Provider::list_apps()`.
    /// 2. For each app, in app-enumeration order:
    ///    a. If any clause's first segment matches the app element itself,
    ///    include the app (single-segment clauses) or run that clause's
    ///    remaining segments anchored at the app (multi-segment). This
    ///    preserves the prior semantics of `find_elements(None, …)`, where
    ///    `get_children(None)` returned the app and a search rooted at the
    ///    system root matched it.
    ///    b. Run `find_elements_group(&app, group, …)` to collect descendant
    ///    matches inside the app's subtree.
    /// 3. Dedup by `ElementData.handle` and apply the outer limit.
    fn resolve_group(
        &self,
        group: &SelectorGroup,
        limit: Option<usize>,
    ) -> Result<Vec<ElementData>> {
        if let Some(root) = self.root.as_ref() {
            return self.provider.find_elements_group(root, group, limit, None);
        }

        // Rootless: search across all apps. We can't push the outer `limit`
        // down to each per-app call because matches from a later app would
        // be wrongly excluded; truncate after the merge instead. (Scoped
        // searches via the single-app fast path above still get phase-1
        // limit pushdown inside the native backend.)
        let apps = self.provider.list_apps()?;
        let mut out: Vec<ElementData> = Vec::new();
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for app in &apps {
            // (2a) The app element itself may match the selector — e.g.
            // `application` or `application button`. `find_elements_group`
            // only emits *descendants* of its root, so we test the app
            // against each clause separately.
            for clause in &group.clauses {
                let first = match clause.segments.first() {
                    Some(s) => s,
                    None => continue,
                };
                if !matches_simple(app, &first.simple) {
                    continue;
                }
                if clause.segments.len() == 1 {
                    if seen.insert(app.handle) {
                        out.push(app.clone());
                    }
                    continue;
                }
                // Multi-segment: app is a phase-1 anchor; narrow through
                // the remaining segments. This mirrors the
                // `find_elements_in_tree`-style phase-1/phase-2 split, but
                // anchored at the app rather than a candidate from a walk.
                let max_depth_val = crate::MAX_TREE_DEPTH;
                let narrowed = self.provider.narrow_multi_segment(
                    vec![app.clone()],
                    &clause.segments[1..],
                    max_depth_val,
                    None,
                )?;
                for d in narrowed {
                    if seen.insert(d.handle) {
                        out.push(d);
                    }
                }
            }

            // (2b) Descendant matches inside the app's subtree.
            let per_app = self.provider.find_elements_group(app, group, None, None)?;
            for d in per_app {
                if seen.insert(d.handle) {
                    out.push(d);
                }
            }
        }
        if let Some(l) = limit {
            out.truncate(l);
        }
        Ok(out)
    }

    /// Resolve the selector to a single ElementData.
    fn resolve_data(&self) -> Result<ElementData> {
        let group = SelectorGroup::parse(&self.selector)?;
        // For multi-clause groups we can't safely truncate at the provider
        // call to `nth+1` — a low-priority clause's match might come
        // *before* a high-priority clause's in document order, so we need
        // the full union to apply `nth` correctly.
        let provider_limit = if group.is_single() {
            Some(self.nth.unwrap_or(0) + 1)
        } else {
            None
        };
        let matches = self.resolve_group(&group, provider_limit)?;
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
        let group = SelectorGroup::parse(&self.selector)?;
        let matches = self.resolve_group(&group, None)?;
        Ok(matches.len())
    }

    /// Get a single [`Element`] handle.
    pub fn element(&self) -> Result<Element> {
        let data = self.resolve_data()?;
        Ok(Element::new(data, Arc::clone(&self.provider)))
    }

    /// Get all matching elements.
    pub fn elements(&self) -> Result<Vec<Element>> {
        let group = SelectorGroup::parse(&self.selector)?;
        let matches = self.resolve_group(&group, None)?;
        Ok(matches
            .into_iter()
            .map(|d| Element::new(d, Arc::clone(&self.provider)))
            .collect())
    }

    /// Capture the subtree rooted at the matched element as a recursive
    /// snapshot. Resolves the selector once (no auto-wait — inspection ops
    /// should fail fast on selector miss). See [`Element::tree`] for
    /// `max_depth` semantics.
    pub fn tree(&self, max_depth: Option<usize>) -> Result<crate::element::TreeNode> {
        self.element()?.tree(max_depth)
    }

    /// Render the subtree rooted at the matched element as an indented
    /// string. Resolves the selector once (no auto-wait). See
    /// [`Element::dump`] for the output format.
    pub fn dump(&self, max_depth: Option<usize>) -> Result<String> {
        self.element()?.dump(max_depth)
    }

    // ── Auto-wait ──────────────────────────────────────────────────

    /// Poll until the element is attached, visible, and enabled, returning a
    /// live [`Element`] handle. Used by the action methods below to provide
    /// resilience against transient unactionable states.
    fn auto_wait(&self) -> Result<Element> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(100);

        loop {
            let elapsed = start.elapsed();
            if elapsed >= self.timeout {
                return Err(Error::Timeout { elapsed });
            }

            match self.resolve_data() {
                Ok(data) if data.states.visible && data.states.enabled => {
                    return Ok(Element::new(data, Arc::clone(&self.provider)));
                }
                Ok(_) | Err(Error::SelectorNotMatched { .. }) => {
                    // Not yet actionable — poll again
                }
                Err(e) => return Err(e),
            }

            std::thread::sleep(poll_interval);
        }
    }

    // ── Actions ────────────────────────────────────────────────────
    //
    // Locator actions auto-wait for the element to be visible and enabled
    // (re-resolving the selector on each poll), then delegate to the
    // [`Element`] action of the same name. For snapshot-bound actions that
    // do not re-resolve, capture an [`Element`] via [`Locator::element`]
    // and call its action methods directly.

    /// Click / invoke the matched element.
    pub fn press(&self) -> Result<()> {
        self.auto_wait()?.press()
    }

    /// Set keyboard focus on the matched element.
    pub fn focus(&self) -> Result<()> {
        self.auto_wait()?.focus()
    }

    /// Remove keyboard focus from the matched element.
    pub fn blur(&self) -> Result<()> {
        self.auto_wait()?.blur()
    }

    /// Toggle the matched element (checkbox, switch).
    pub fn toggle(&self) -> Result<()> {
        self.auto_wait()?.toggle()
    }

    /// Select the matched element (list item, etc.).
    pub fn select(&self) -> Result<()> {
        self.auto_wait()?.select()
    }

    /// Expand the matched element.
    pub fn expand(&self) -> Result<()> {
        self.auto_wait()?.expand()
    }

    /// Collapse the matched element.
    pub fn collapse(&self) -> Result<()> {
        self.auto_wait()?.collapse()
    }

    /// Show the context menu for the matched element.
    pub fn show_menu(&self) -> Result<()> {
        self.auto_wait()?.show_menu()
    }

    /// Increment the matched element (slider, spinner).
    pub fn increment(&self) -> Result<()> {
        self.auto_wait()?.increment()
    }

    /// Decrement the matched element (slider, spinner).
    pub fn decrement(&self) -> Result<()> {
        self.auto_wait()?.decrement()
    }

    /// Scroll the matched element into view.
    pub fn scroll_into_view(&self) -> Result<()> {
        self.auto_wait()?.scroll_into_view()
    }

    /// Set the text value of the matched element.
    pub fn set_value(&self, value: &str) -> Result<()> {
        self.auto_wait()?.set_value(value)
    }

    /// Set the numeric value of the matched element (slider, spinner).
    pub fn set_numeric_value(&self, value: f64) -> Result<()> {
        // Validate up-front so callers fail fast on NaN/inf without burning
        // the auto-wait timeout.
        if !value.is_finite() {
            return Err(Error::InvalidActionData {
                message: format!("set_numeric_value requires a finite value, got {}", value),
            });
        }
        self.auto_wait()?.set_numeric_value(value)
    }

    /// Type text at the current cursor position on the matched element.
    pub fn type_text(&self, text: &str) -> Result<()> {
        self.auto_wait()?.type_text(text)
    }

    /// Select a text range within the matched element.
    pub fn select_text(&self, start: u32, end: u32) -> Result<()> {
        if start > end {
            return Err(Error::InvalidActionData {
                message: format!("select_text start ({}) must be <= end ({})", start, end),
            });
        }
        self.auto_wait()?.select_text(start, end)
    }

    /// Perform an action by name (with auto-wait).
    ///
    /// This is the escape hatch for platform-specific actions not covered
    /// by the named methods above. Also works for well-known action names.
    pub fn perform_action(&self, action: &str) -> Result<()> {
        self.auto_wait()?.perform_action(action)
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

#[cfg(test)]
mod tests {
    //! End-to-end tests for [`Locator`] against the in-memory mock provider.
    //!
    //! Covers:
    //! - selector-group (comma alternation): union, dedup, document order
    //!   across clauses; `count()`, `elements()`, `nth()`, `element()`;
    //!   chained `.descendant()` / `.child()` distributing per clause.
    //! - tree/dump inspection helpers (subtree capture, depth limits,
    //!   fail-fast on miss).
    //!
    //! The mock tree topology is documented on [`crate::mock`].

    use super::*;
    use crate::mock::build_provider;

    fn root_locator(selector: &str) -> Locator {
        let provider = build_provider();
        let provider_dyn: Arc<dyn Provider> = provider;
        Locator::new(provider_dyn, None, selector)
    }

    fn names(elements: &[Element]) -> Vec<String> {
        elements
            .iter()
            .map(|e| e.data().name.clone().unwrap_or_default())
            .collect()
    }

    #[test]
    fn group_count_returns_union_size_across_clauses() {
        // Two non-overlapping clauses; count is the sum.
        let loc = root_locator("check_box, slider");
        assert_eq!(loc.count().unwrap(), 2);
    }

    #[test]
    fn group_count_dedups_overlapping_clauses() {
        // `button` matches "Back" and "Forward"; `[name="Back"]` overlaps
        // with "Back". The union must dedupe to 2 unique elements.
        let loc = root_locator(r#"button, [name="Back"]"#);
        assert_eq!(loc.count().unwrap(), 2);
    }

    #[test]
    fn group_elements_returned_in_document_order() {
        // Mock tree DFS order through Navigation/Content:
        //   Back, Forward (toolbar) → Search (text_field) → ... → Item 2.
        // The result of `button, text_field` must interleave by document
        // position, not group by clause.
        let loc = root_locator("button, text_field");
        let names = names(&loc.elements().unwrap());
        assert_eq!(names, vec!["Back", "Forward", "Search"]);
    }

    #[test]
    fn group_element_returns_first_match_in_document_order() {
        // `text_field, button` would naively return Search first (the first
        // text_field clause matches first by clause). Document order makes
        // "Back" win.
        let loc = root_locator("text_field, button");
        let el = loc.element().expect("element must resolve");
        assert_eq!(el.data().name.as_deref(), Some("Back"));
    }

    #[test]
    fn group_nth_picks_across_full_union() {
        // `.nth(2)` on the document-ordered union [Back, Forward, Search]
        // must be "Forward", not the 2nd element of any single clause.
        let loc = root_locator("button, text_field").nth(2);
        let el = loc.element().unwrap();
        assert_eq!(el.data().name.as_deref(), Some("Forward"));
    }

    #[test]
    fn group_single_clause_behaves_identically() {
        // Sanity: no-comma selectors are unaffected by SelectorGroup parsing.
        let single = root_locator("button");
        assert_eq!(single.count().unwrap(), 2);
        assert_eq!(names(&single.elements().unwrap()), vec!["Back", "Forward"],);
    }

    #[test]
    fn descendant_distributes_over_clauses() {
        // `.descendant("button")` on a group of (toolbar, group) parents
        // must apply to *both* parents. Direct child buttons exist only
        // under "toolbar"; if distribution failed, we'd miss them. Inverse
        // case is harder to construct in this fixture, but we verify the
        // generated string round-trips and matches buttons under each
        // clause's subtree.
        let loc = root_locator("toolbar, group").descendant("button");
        // After chaining, the stored selector must distribute per clause:
        assert_eq!(loc.selector(), "toolbar button, group button");
        // And resolve to the two buttons (both under toolbar; the group
        // subtree has no buttons in this fixture).
        let names = names(&loc.elements().unwrap());
        assert_eq!(names, vec!["Back", "Forward"]);
    }

    #[test]
    fn child_distributes_over_clauses() {
        // `.child("button")` on a (toolbar, group) parent group should
        // distribute the `>` combinator over each clause.
        let loc = root_locator("toolbar, group").child("button");
        assert_eq!(loc.selector(), "toolbar > button, group > button");
        let names = names(&loc.elements().unwrap());
        assert_eq!(names, vec!["Back", "Forward"]);
    }

    #[test]
    fn descendant_after_group_then_another_group_cross_products() {
        // Repeated chained navigation keeps distributing — and a group
        // suffix multiplies clauses (cross product). Verify the stored
        // selector form is what we expect; semantic resolution is covered
        // by the other tests.
        let loc = root_locator("toolbar, group").descendant("button, text_field");
        assert_eq!(
            loc.selector(),
            "toolbar button, toolbar text_field, group button, group text_field",
        );
    }

    #[test]
    fn group_exists_true_when_any_clause_matches() {
        // First clause matches nothing, second matches; existence is union.
        let loc = root_locator(r#"button[name="Nope"], slider"#);
        assert!(loc.exists().unwrap());
    }

    #[test]
    fn group_exists_false_when_no_clause_matches() {
        let loc = root_locator(r#"button[name="Nope"], text_field[name="AlsoNope"]"#);
        assert!(!loc.exists().unwrap());
    }

    // ── Rootless search across apps ─────────────────────────────────

    #[test]
    fn rootless_group_matches_match_app_scoped_search() {
        // `Locator::new(provider, None, …)` enumerates apps via
        // `list_apps()` and unions per-app searches. For a single-app
        // mock tree, the result must equal what `App::locator(…)` would
        // produce (modulo the `application` root which only the rootless
        // search can match, since app-scoped searches walk descendants).
        let provider = build_provider();
        let provider_dyn: Arc<dyn Provider> = provider;
        let rootless = Locator::new(provider_dyn.clone(), None, "button, text_field");
        let app_root = provider_dyn
            .list_apps()
            .expect("list_apps must succeed")
            .into_iter()
            .next()
            .expect("mock provider must expose an application root");
        let scoped = Locator::new(provider_dyn, Some(app_root), "button, text_field");
        assert_eq!(
            names(&rootless.elements().unwrap()),
            vec!["Back", "Forward", "Search"]
        );
        assert_eq!(
            names(&rootless.elements().unwrap()),
            names(&scoped.elements().unwrap())
        );
    }

    #[test]
    fn list_apps_returns_mock_application_root() {
        // The mock provider's `list_apps` must surface the single
        // top-level Application element — the discovery primitive that
        // `App::list_with` / `App::by_name_with` now use.
        let provider = build_provider();
        let provider_dyn: Arc<dyn Provider> = provider;
        let apps = provider_dyn.list_apps().expect("list_apps must succeed");
        assert_eq!(apps.len(), 1, "mock tree has exactly one application");
        assert_eq!(apps[0].role, crate::role::Role::Application);
        assert_eq!(apps[0].name.as_deref(), Some("TestApp"));
    }

    // ── tree() / dump() ─────────────────────────────────────────────

    #[test]
    fn locator_tree_returns_subtree_rooted_at_match() {
        let node = root_locator("application")
            .tree(None)
            .expect("tree must succeed");
        assert_eq!(node.role, "application");
        assert_eq!(node.name.as_deref(), Some("TestApp"));
        assert!(!node.children.is_empty());
    }

    #[test]
    fn locator_tree_respects_max_depth() {
        let node = root_locator("application")
            .tree(Some(0))
            .expect("tree must succeed");
        assert!(node.children.is_empty(), "max_depth=0 should drop children");
    }

    #[test]
    fn locator_dump_renders_selector_subtree() {
        let s = root_locator("application")
            .dump(None)
            .expect("dump must succeed");
        assert!(
            s.contains(r#"application "TestApp""#),
            "dump should render the matched root: {s}"
        );
    }

    #[test]
    fn locator_dump_max_depth_zero_is_one_line() {
        let s = root_locator("application")
            .dump(Some(0))
            .expect("dump must succeed");
        let non_empty: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(non_empty.len(), 1);
    }

    #[test]
    fn locator_tree_no_match_returns_selector_not_matched() {
        let err = root_locator(r#"button[name="DoesNotExist"]"#)
            .tree(None)
            .expect_err("tree must fail on miss");
        assert!(
            matches!(err, Error::SelectorNotMatched { .. }),
            "expected SelectorNotMatched, got {err:?}"
        );
    }

    #[test]
    fn locator_dump_does_not_auto_wait() {
        // Locator dump/tree are inspection ops — they must fail fast, not poll.
        let locator = root_locator(r#"button[name="DoesNotExist"]"#);
        let start = std::time::Instant::now();
        let _ = locator.dump(None);
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "dump should fail fast, took {elapsed:?}"
        );
    }

    // ── Fresh-handle regression tests ───────────────────────────────────
    //
    // Real platform providers (Windows/UIA, macOS/AX, Linux/AT-SPI2) allocate
    // a fresh `handle` for every element returned by every `get_children`
    // call — see e.g. `WindowsProvider::cache_element`. The doc-order merge
    // for multi-clause `SelectorGroup`s used to identify elements by handle
    // to dedup the per-clause walks against a re-walk of the tree, which
    // missed every lookup on real providers and returned 0 results. These
    // tests wrap the standard `MockProvider` with a per-call handle-rewriter
    // so the failure mode is reproducible without a real platform backend.

    /// Wraps a `MockProvider`-like child source and rewrites every returned
    /// `ElementData.handle` to a fresh atomic-incremented value. A backing
    /// map remembers what the freshly-minted handle pointed to so subsequent
    /// `get_children(Some(parent))` calls can look up the original mock-side
    /// handle and delegate.
    struct FreshHandleProvider {
        inner: Arc<crate::mock::MockProvider>,
        next: std::sync::atomic::AtomicU64,
        // Map from freshly-minted handle → the inner provider's stable handle.
        rewrite: std::sync::Mutex<std::collections::HashMap<u64, u64>>,
    }

    impl FreshHandleProvider {
        fn wrap(inner: Arc<crate::mock::MockProvider>) -> Arc<dyn Provider> {
            Arc::new(FreshHandleProvider {
                inner,
                next: std::sync::atomic::AtomicU64::new(1_000_000),
                rewrite: std::sync::Mutex::new(std::collections::HashMap::new()),
            })
        }

        fn translate(&self, fresh: u64) -> u64 {
            self.rewrite
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&fresh)
                .copied()
                // Top-level calls pass through verbatim; only freshly-minted
                // child handles need translation back to the inner mock's
                // stable handle.
                .unwrap_or(fresh)
        }
    }

    impl Provider for FreshHandleProvider {
        fn list_apps(&self) -> Result<Vec<crate::element::ElementData>> {
            // App discovery routes through `get_children(None)` here so the
            // freshly-minted handles flow through the same rewriter that
            // `get_children` uses — keeping the "no handle is ever reused
            // across calls" simulation honest for the list_apps path too.
            self.get_children(None)
        }

        fn get_children(
            &self,
            parent: Option<&crate::element::ElementData>,
        ) -> Result<Vec<crate::element::ElementData>> {
            let translated = parent.map(|p| {
                let inner = self.translate(p.handle);
                let mut clone = p.clone();
                clone.handle = inner;
                clone
            });
            let children = self.inner.get_children(translated.as_ref())?;
            let mut out = Vec::with_capacity(children.len());
            for mut child in children {
                let inner_handle = child.handle;
                let fresh = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.rewrite
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(fresh, inner_handle);
                child.handle = fresh;
                out.push(child);
            }
            Ok(out)
        }

        fn get_parent(
            &self,
            element: &crate::element::ElementData,
        ) -> Result<Option<crate::element::ElementData>> {
            let mut clone = element.clone();
            clone.handle = self.translate(element.handle);
            self.inner.get_parent(&clone)
        }

        // Action methods: irrelevant to selector resolution — delegate verbatim.
        fn press(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.press(e)
        }
        fn focus(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.focus(e)
        }
        fn blur(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.blur(e)
        }
        fn toggle(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.toggle(e)
        }
        fn select(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.select(e)
        }
        fn expand(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.expand(e)
        }
        fn collapse(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.collapse(e)
        }
        fn show_menu(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.show_menu(e)
        }
        fn increment(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.increment(e)
        }
        fn decrement(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.decrement(e)
        }
        fn scroll_into_view(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.scroll_into_view(e)
        }
        fn set_value(&self, e: &crate::element::ElementData, v: &str) -> Result<()> {
            self.inner.set_value(e, v)
        }
        fn set_numeric_value(&self, e: &crate::element::ElementData, v: f64) -> Result<()> {
            self.inner.set_numeric_value(e, v)
        }
        fn type_text(&self, e: &crate::element::ElementData, t: &str) -> Result<()> {
            self.inner.type_text(e, t)
        }
        fn set_text_selection(
            &self,
            e: &crate::element::ElementData,
            start: u32,
            end: u32,
        ) -> Result<()> {
            self.inner.set_text_selection(e, start, end)
        }
        fn perform_action(&self, e: &crate::element::ElementData, action: &str) -> Result<()> {
            self.inner.perform_action(e, action)
        }
        fn subscribe(
            &self,
            e: &crate::element::ElementData,
        ) -> Result<crate::event_provider::Subscription> {
            self.inner.subscribe(e)
        }
    }

    fn root_locator_fresh_handles(selector: &str) -> Locator {
        let inner = build_provider();
        let provider = FreshHandleProvider::wrap(inner);
        Locator::new(provider, None, selector)
    }

    #[test]
    fn group_fresh_handles_elements_in_document_order() {
        // Regression: pre-fix, this returned 0 elements because the doc-order
        // merge looked elements up by handle in a HashMap, and the per-clause
        // walks vs. the merge walk produced disjoint handle sets.
        let loc = root_locator_fresh_handles("button, text_field");
        assert_eq!(
            names(&loc.elements().unwrap()),
            vec!["Back", "Forward", "Search"],
        );
    }

    #[test]
    fn group_fresh_handles_count_matches_stable_handles() {
        // The count via fresh-handle provider must equal the count via the
        // stable-handle mock — both should yield 2 (Back + Forward) for a
        // dedup-overlapping group.
        let stable = root_locator(r#"button, [name="Back"]"#);
        let fresh = root_locator_fresh_handles(r#"button, [name="Back"]"#);
        assert_eq!(stable.count().unwrap(), fresh.count().unwrap());
        assert_eq!(fresh.count().unwrap(), 2);
    }

    #[test]
    fn group_fresh_handles_exists_true_when_any_clause_matches() {
        let loc = root_locator_fresh_handles(r#"button[name="Nope"], slider"#);
        assert!(loc.exists().unwrap());
    }

    #[test]
    fn group_fresh_handles_nth_picks_across_full_union() {
        // `.nth(2)` on `button, text_field` over the fresh-handle provider
        // must still return "Forward" — the 2nd of [Back, Forward, Search]
        // in document order. Pre-fix, .nth(2) returned a "selector not
        // matched" error because the underlying group resolution returned
        // an empty union.
        let loc = root_locator_fresh_handles("button, text_field").nth(2);
        let el = loc.element().unwrap();
        assert_eq!(el.data().name.as_deref(), Some("Forward"));
    }

    #[test]
    fn group_fresh_handles_descendant_chain_resolves() {
        // `.descendant("button")` on the (toolbar, group) group locator,
        // run through the fresh-handle provider. Pre-fix, this chained
        // selector returned 0 results.
        let loc = root_locator_fresh_handles("toolbar, group").descendant("button");
        assert_eq!(loc.selector(), "toolbar button, group button");
        assert_eq!(names(&loc.elements().unwrap()), vec!["Back", "Forward"]);
    }

    #[test]
    fn group_fresh_handles_three_clauses_doc_order() {
        // Three-clause group over a fresh-handle provider — guards the
        // bug-fix's invariant that the BTreeMap-by-path merge handles >2
        // clauses correctly.
        let loc = root_locator_fresh_handles("toolbar, slider, check_box");
        // Document order through Navigation/Content: Navigation (toolbar),
        // then Agree (check_box), then Volume (slider).
        assert_eq!(
            names(&loc.elements().unwrap()),
            vec!["Navigation", "Agree", "Volume"]
        );
    }
}
