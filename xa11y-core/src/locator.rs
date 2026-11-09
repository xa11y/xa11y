use std::convert::Infallible;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use crate::element::{Element, ElementData, HiddenReason, Rect};
use crate::error::{AutoScrollOutcome, Delivery, Diagnosis, Error, Result};
use crate::event::ElementState;
use crate::provider::Provider;
use crate::selector::{
    chain_combinator, matches_simple, Combinator, Selector, SelectorGroup, SelectorSegment,
    SimpleSelector,
};

// ── Diagnosis bounds (tenet 6) ──────────────────────────────────────
//
// Diagnosis collection runs only on the failure path, after a wait has
// already burned its timeout or a fail-fast query missed — but it must stay
// size-bounded so a miss against a huge tree doesn't produce a megabyte
// error message or a multi-second stall on top of the failure.

/// Maximum number of same-role near-miss candidates listed in a diagnosis.
const DIAG_CANDIDATE_LIMIT: usize = 8;
/// Maximum tree depth captured for the scope snapshot in a diagnosis.
const DIAG_SCOPE_MAX_DEPTH: usize = 5;
/// Maximum number of lines (tree dump) or entries (application list) kept in
/// the scope snapshot; the remainder is summarized as a truncation marker.
const DIAG_SCOPE_MAX_LINES: usize = 40;

/// Short element rendering for candidate lists: `button "Export"`.
fn short_describe(data: &ElementData) -> String {
    match &data.name {
        Some(n) => format!("{} \"{n}\"", data.role.to_snake_case()),
        None => format!("{} (unnamed)", data.role.to_snake_case()),
    }
}

/// Element rendering for `last observed` clauses, including the states the
/// wait conditions are defined over.
fn describe_observed(data: &ElementData) -> String {
    format!(
        "matched {} (visible={}, enabled={}, focused={})",
        short_describe(data),
        data.states.visible,
        data.states.enabled,
        data.states.focused
    )
}

/// Interval between polls of the `wait_*` methods.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Exceeds Chromium's 500 ms publication interval for nonfocused bounds.
/// Equal samples one poll apart can otherwise be the same native snapshot
/// while the element is still moving (issue #633).
const STABLE_OBSERVATION_WINDOW: Duration = Duration::from_millis(750);

/// The condition named in a [`Locator::wait_stable`] timeout diagnosis.
const STABLE_CONDITION: &str = "stable (visible bounds observed unchanged for at least 750 ms)";

#[derive(Default)]
struct StableBounds {
    candidate: Option<(Rect, Duration)>,
    last_change: Option<(Rect, Rect)>,
}

impl StableBounds {
    /// `elapsed` is sampled after each fresh resolve, so time spent in the
    /// initial resolve cannot count toward the observation window.
    fn observe(&mut self, bounds: Option<Rect>, elapsed: Duration) -> bool {
        match (self.candidate, bounds) {
            (Some((before, since)), Some(after)) if before == after => {
                elapsed.saturating_sub(since) >= STABLE_OBSERVATION_WINDOW
            }
            (previous, Some(after)) => {
                self.last_change = previous.map(|(before, _)| (before, after));
                self.candidate = Some((after, elapsed));
                false
            }
            (_, None) => {
                self.candidate = None;
                self.last_change = None;
                false
            }
        }
    }

    fn unchanged_for(&self, elapsed: Duration) -> Option<Duration> {
        self.candidate
            .map(|(_, since)| elapsed.saturating_sub(since))
    }
}

/// The bounds a poll contributes to [`Locator::wait_stable`]: present only
/// when the element is visible and reports bounds. Anything else resets the
/// comparison.
fn stable_candidate(data: &ElementData) -> Option<Rect> {
    if data.states.visible {
        data.bounds
    } else {
        None
    }
}

/// `{x=10, y=20, w=100, h=30}`, for bounds in a diagnosis.
fn describe_rect(r: &Rect) -> String {
    format!("{{x={}, y={}, w={}, h={}}}", r.x, r.y, r.width, r.height)
}

/// Cap a multi-line string at `max_lines`, appending a truncation marker
/// that says how much was dropped.
fn truncate_lines(s: &str, max_lines: usize) -> String {
    let mut lines = s.lines();
    let kept: Vec<&str> = lines.by_ref().take(max_lines).collect();
    let remaining = lines.count();
    if remaining == 0 {
        kept.join("\n")
    } else {
        format!("{}\n… (+{remaining} more lines truncated)", kept.join("\n"))
    }
}

/// Human label for a wait condition, used in timeout diagnoses.
fn state_label(state: ElementState) -> &'static str {
    match state {
        ElementState::Attached => "attached",
        ElementState::Detached => "detached",
        ElementState::Visible => "visible",
        ElementState::Hidden => "hidden",
        ElementState::Enabled => "enabled",
        ElementState::Disabled => "disabled",
        ElementState::Focused => "focused",
        ElementState::Unfocused => "unfocused",
    }
}

/// The state conditions an action's auto-wait requires before it will act.
///
/// Every locator action requires the target to be `enabled` — a disabled
/// control cannot be operated through any accessibility API. Most verbs also
/// require `visible`, matching the input-simulation heritage where an action
/// needs on-screen coordinates. `scroll_into_view` is the exception: gating it
/// on `visible` is circular, since making an off-screen element visible is its
/// whole purpose (issue #350).
#[derive(Clone, Copy)]
struct Actionability {
    /// Require `states.enabled`.
    require_enabled: bool,
    /// Require `states.visible` in addition to `enabled`.
    require_visible: bool,
}

impl Actionability {
    /// The default gate: the target must be both visible and enabled.
    const VISIBLE_AND_ENABLED: Self = Self {
        require_enabled: true,
        require_visible: true,
    };
    /// Gate for `scroll_into_view`: enabled only, no visibility requirement.
    const ENABLED: Self = Self {
        require_enabled: true,
        require_visible: false,
    };
    /// Gate for reads: the element only has to exist. A disabled or
    /// read-only field, or text scrolled out of view, is still readable.
    const ATTACHED: Self = Self {
        require_enabled: false,
        require_visible: false,
    };

    /// Whether `data` satisfies this gate.
    fn is_met(self, data: &ElementData) -> bool {
        (!self.require_enabled || data.states.enabled)
            && (!self.require_visible || data.states.visible)
    }

    /// Human label for the gate, used in timeout diagnoses.
    fn label(self) -> &'static str {
        match (self.require_enabled, self.require_visible) {
            (true, true) => "visible && enabled",
            (true, false) => "enabled",
            (false, true) => "visible",
            (false, false) => "attached",
        }
    }

    /// The gate the locator action named `action` waits on.
    ///
    /// One table rather than a gate argument at each call site, so the
    /// named action methods and [`Locator::auto_wait_unique`] — which is
    /// handed a verb by name — cannot disagree about a verb's gate.
    ///
    /// Every name is listed: an unknown or misspelled one is refused with
    /// [`Error::InvalidActionData`] rather than quietly given the default gate.
    fn for_action(action: &str) -> Result<Self> {
        Ok(match action {
            // Making an off-screen element visible is this verb's purpose.
            "scroll_into_view" => Self::ENABLED,
            // Reading text needs no interaction, so only presence is waited for.
            "read_text" => Self::ATTACHED,
            // A minimized window is legitimately invisible; see the window
            // management section below.
            "activate" | "minimize" | "maximize" | "enter_fullscreen" | "restore" | "close"
            | "move_to" | "resize_to" => Self::ENABLED,
            "press" | "focus" | "blur" | "toggle" | "select" | "expand" | "collapse"
            | "show_menu" | "increment" | "decrement" | "set_value" | "set_numeric_value"
            | "type_text" | "select_text" | "perform_action" => Self::VISIBLE_AND_ENABLED,
            other => {
                return Err(Error::InvalidActionData {
                    message: format!("no auto-wait gate for unknown action {other:?}"),
                })
            }
        })
    }
}

/// How many times one auto-wait calls `scroll_into_view` on a target that
/// stays scrolled out after each call.
///
/// More than one because a call can succeed and leave the target still out of
/// view: a virtualizing list that realizes rows as it scrolls can need a second
/// pass once the layout settles. Bounded because some calls succeed and change
/// nothing at all. A collapsed tree item on Windows that is also clipped by its
/// scroller reads `scrolled_out`, and `ScrollItem.ScrollIntoView` on it is
/// accepted without expanding the parent. Retrying that every 100 ms for the
/// whole timeout would only repeat a call already known to have no effect.
const AUTO_SCROLL_ATTEMPTS: u32 = 3;

/// What an auto-wait has done about a target that is hidden only because it
/// is scrolled out of view (issue #590).
///
/// Actionability includes scrolling: when the one thing standing between a
/// visible-gated action and its target is [`HiddenReason::ScrolledOut`], the
/// wait calls the element's own `scroll_into_view` (the platform's semantic
/// scroll, never simulated input) and re-checks on the next poll. Where the
/// platform has no such call for the target, the wait keeps polling as before
/// and the timeout diagnosis says the scroll was refused, so a caller can tell
/// "nothing can scroll this" from "the scroll did not help".
///
/// A refusal is sticky: the platform is asked once per wait. An accepted call
/// is repeated at most [`AUTO_SCROLL_ATTEMPTS`] times while the target stays
/// scrolled out. With a zero timeout an accepted call earns one more poll,
/// so the single attempt is judged on what the scroll did; any other timeout
/// is a hard deadline (issue #531), and the scroll is re-checked only if time
/// is left. The outcome reaches the caller as [`Diagnosis::auto_scroll`] on
/// the timeout, and describes the latest scroll a poll observed: one the
/// deadline cut off before its re-check is not reflected in it.
#[derive(Default)]
struct AutoScroll {
    /// `scroll_into_view` calls the platform accepted.
    accepted: u32,
    /// Why the platform refused `scroll_into_view`, once it has. No further
    /// attempt is made in this wait: a refusal is an answer about the element
    /// and its toolkit, not about this instant.
    refused: Option<String>,
    /// A call was accepted and no poll has observed the element since. The
    /// next poll's observation decides `seen`, and no outcome is claimed
    /// without one; if the deadline ends the wait first, `seen` keeps the
    /// previous scroll's outcome (or none).
    unobserved: bool,
    /// What the first poll after the latest accepted call saw: `Revealed`
    /// when it read the element visible, `NoEffect` otherwise.
    seen: Option<AutoScrollOutcome>,
    /// The element's `hidden_reason` on that poll, when it was not visible.
    still_hidden: Option<HiddenReason>,
}

impl AutoScroll {
    /// Record what a poll observed, if a scroll is waiting to be checked.
    ///
    /// Only a poll that reads the element visible counts as `Revealed`. One
    /// that is no longer `scrolled_out` but still hidden for another reason
    /// is `NoEffect`: the scroll did not make it visible, and the note names
    /// the reason it was hidden instead.
    fn observe(&mut self, data: &ElementData) {
        if std::mem::take(&mut self.unobserved) {
            if data.states.visible {
                self.seen = Some(AutoScrollOutcome::Revealed);
                self.still_hidden = None;
            } else {
                self.seen = Some(AutoScrollOutcome::NoEffect);
                self.still_hidden = data.states.hidden_reason;
            }
        }
    }

    /// Scroll `data` into view if the gate is blocked only by its being
    /// scrolled out, and there are attempts left. Returns whether the
    /// platform accepted a call, in which case a zero-timeout wait polls
    /// once more before it times out.
    ///
    /// The target must be enabled: a disabled control is not actionable
    /// wherever it is, so scrolling it would move the UI for nothing.
    ///
    /// A stale handle is left for the next poll, which re-resolves the
    /// selector and tries again with the fresh one. Any other failure is
    /// returned: it is not "this cannot be scrolled", and polling on past it
    /// would hide a real error behind a timeout (tenet 1).
    fn attempt(
        &mut self,
        gate: Actionability,
        data: &ElementData,
        provider: &dyn Provider,
    ) -> Result<bool> {
        let blocked_only_by_scroll = gate.require_visible
            && data.states.enabled
            && !data.states.visible
            && data.states.hidden_reason == Some(HiddenReason::ScrolledOut);
        if !blocked_only_by_scroll
            || self.refused.is_some()
            || self.accepted >= AUTO_SCROLL_ATTEMPTS
        {
            return Ok(false);
        }
        match provider.scroll_into_view(data) {
            Ok(()) => {
                self.accepted += 1;
                self.unobserved = true;
                return Ok(true);
            }
            Err(e @ (Error::ActionNotSupported { .. } | Error::Unsupported { .. })) => {
                self.refused = Some(e.to_string());
            }
            Err(Error::ElementStale { .. }) => {}
            Err(e) => return Err(e),
        }
        Ok(false)
    }

    /// The structured outcome, for [`Diagnosis::auto_scroll`].
    fn outcome(&self) -> Option<AutoScrollOutcome> {
        if self.refused.is_some() {
            Some(AutoScrollOutcome::Refused)
        } else {
            self.seen
        }
    }

    /// The clause the timeout diagnosis appends to `last_observed`.
    fn note(&self) -> Option<String> {
        let n = self.accepted;
        Some(match self.outcome()? {
            AutoScrollOutcome::Refused => format!(
                "auto-scroll: scroll_into_view was refused ({}), so nothing could bring \
                 it into view",
                self.refused.as_deref().unwrap_or_default()
            ),
            AutoScrollOutcome::NoEffect => match self.still_hidden {
                Some(HiddenReason::ScrolledOut) => format!(
                    "auto-scroll: scroll_into_view was accepted {n} time(s) but the element \
                     stayed scrolled out"
                ),
                Some(reason) => format!(
                    "auto-scroll: scroll_into_view was accepted {n} time(s); the element was \
                     no longer scrolled out but still not visible ({})",
                    reason.to_snake_case()
                ),
                None => format!(
                    "auto-scroll: scroll_into_view was accepted {n} time(s) but the element \
                     was still not visible"
                ),
            },
            AutoScrollOutcome::Revealed => format!(
                "auto-scroll: scroll_into_view was accepted {n} time(s) and the next check \
                 read the element visible"
            ),
        })
    }
}

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
#[derive(Clone)]
pub struct Locator {
    provider: Arc<dyn Provider>,
    /// Root element for scoped searches. `None` = system root (all apps).
    root: Option<ElementData>,
    /// Additional native roots in the same application process.
    additional_roots: Vec<ElementData>,
    process_scope: bool,
    selector: String,
    /// Which match to select (0-based). `None` means first match.
    nth: Option<usize>,
    /// Timeout for auto-wait before action methods. `None` means "resolve
    /// the process-wide default at use time" (see [`crate::config`]), so
    /// `set_default_timeout` affects locators created before it was called.
    timeout: Option<Duration>,
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
            additional_roots: Vec::new(),
            process_scope: false,
            selector: selector.to_string(),
            nth: None,
            timeout: None,
        }
    }

    /// Create a locator scoped to a process represented by several native
    /// application roots.
    #[doc(hidden)]
    pub fn new_for_roots(
        provider: Arc<dyn Provider>,
        roots: Vec<ElementData>,
        selector: &str,
    ) -> Self {
        let root = roots.first().cloned();
        Self {
            provider,
            root,
            additional_roots: roots,
            process_scope: false,
            selector: selector.to_string(),
            nth: None,
            timeout: None,
        }
    }

    /// Create a locator whose scope is the whole process owning `root`.
    #[doc(hidden)]
    pub fn new_for_app(provider: Arc<dyn Provider>, root: ElementData, selector: &str) -> Self {
        let mut locator = Self::new(provider, Some(root), selector);
        locator.process_scope = true;
        locator
    }

    /// Return a new Locator with a custom auto-wait timeout for action
    /// methods. Takes precedence over the process-wide default set via
    /// [`crate::config::set_default_timeout`] / `XA11Y_DEFAULT_TIMEOUT`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Return a new Locator that selects the nth match (**1-based**, like the
    /// selector grammar's `:nth(N)` pseudo-class): `nth(1)` is the first
    /// match, `nth(2)` the second, and so on.
    ///
    /// # Panics
    ///
    /// Panics if `n` is 0 — there is no zeroth match in 1-based indexing.
    /// Use [`first()`](Self::first) or `nth(1)` for the first match.
    ///
    /// This is a deliberate API decision: changing the signature to return
    /// `Result` would be a breaking change, so the 0 case stays a panic and
    /// is documented here instead. Language bindings validate `n` before
    /// calling, so they surface this as a normal error rather than a panic.
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
            let process_roots;
            let roots: &[ElementData] = if self.process_scope {
                process_roots = self.provider.app_roots(root)?;
                &process_roots
            } else {
                &self.additional_roots
            };
            if roots.is_empty() {
                return self.provider.find_elements_group(root, group, limit, None);
            }
            if roots.len() == 1 {
                // Preserve the native backend's limit pushdown for the common
                // single-root case (including first-match auto-wait actions).
                return self
                    .provider
                    .find_elements_group(&roots[0], group, limit, None);
            }
            // Evaluate selectors once over the process's combined children.
            // Applying :nth independently to each registration returns the
            // wrong match set, e.g. two first windows and no second window.
            let mut children = Vec::new();
            for native_root in roots {
                for child in self.provider.get_children(Some(native_root))? {
                    if !children
                        .iter()
                        .any(|existing| crate::app::same_window_identity(existing, &child))
                    {
                        children.push(child);
                    }
                }
            }
            return crate::selector::find_elements_in_tree_group(
                |element| {
                    if element.is_some_and(|element| element.handle == root.handle) {
                        Ok(children.clone())
                    } else {
                        self.provider.get_children(element)
                    }
                },
                Some(root),
                group,
                limit,
                None,
            );
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
        let total = matches.len();
        matches.into_iter().nth(idx).ok_or_else(|| {
            let err = Error::selector_not_matched(self.selector.clone());
            if total > 0 {
                // The selector itself matched but not enough times for `nth`.
                // This is exact even under limit pushdown: the limit is
                // `idx + 1`, and we only get here when fewer were returned.
                err.diagnose(Diagnosis {
                    last_observed: Some(format!(
                        "selector matched {total} element(s); nth({}) requested",
                        idx + 1
                    )),
                    ..Diagnosis::default()
                })
            } else {
                // Bare miss: poll loops use this as their retry signal, so
                // it stays cheap. Terminal sites enrich via `enrich_miss`.
                err
            }
        })
    }

    // ── Diagnosis collection (tenet 6, failure path only) ───────────

    /// Attach bounded scope context (same-role candidates + scope snapshot)
    /// to a `SelectorNotMatched` that is about to reach the caller. Other
    /// errors pass through unchanged.
    fn enrich_miss(&self, err: Error) -> Error {
        match err {
            Error::SelectorNotMatched {
                selector,
                diagnosis,
            } => {
                let mut d = diagnosis.map(|b| *b).unwrap_or_default();
                self.fill_miss_context(&mut d);
                Error::selector_not_matched(selector).diagnose(d)
            }
            other => other,
        }
    }

    /// Populate the expensive miss-context fields of a diagnosis (candidate
    /// list, scope snapshot) if they are not already set.
    fn fill_miss_context(&self, d: &mut Diagnosis) {
        if d.candidates.is_empty() {
            d.candidates = self.role_candidates();
        }
        if d.scope.is_none() {
            d.scope = Some(self.scope_summary());
        }
        if let Some(hint) = self.web_accessibility_hint() {
            d.last_observed = Some(match d.last_observed.take() {
                Some(observed) => format!("{observed}; {hint}"),
                None => hint,
            });
        }
    }

    /// A hint for a miss inside a web view whose content the app has not
    /// built an accessibility tree for. Chromium and Electron do that only on
    /// request, and the symptom is a `web_area` with no children, which
    /// reads like a wrong selector. Bounded by [`DIAG_CANDIDATE_LIMIT`].
    ///
    /// A scan that fails is reported in the hint rather than dropped, the
    /// way [`role_candidates`](Self::role_candidates) records a failing
    /// collection (tenet 1). The miss error being enriched keeps its own
    /// variant, so the hint is the one place the failure can surface.
    ///
    /// The advice names the platforms where a client can act on it: macOS
    /// has a request to make and Linux a launch flag. Windows Chromium builds
    /// the tree itself, so the hint says nothing specific there.
    fn web_accessibility_hint(&self) -> Option<String> {
        let root = self.root.as_ref()?;
        let selector = match Selector::parse("web_area") {
            Ok(s) => s,
            Err(e) => return Some(format!("web_area check failed: {e}")),
        };
        let areas =
            match self
                .provider
                .find_elements(root, &selector, Some(DIAG_CANDIDATE_LIMIT), None)
            {
                Ok(areas) => areas,
                Err(e) => return Some(format!("web_area check failed: {e}")),
            };
        // Fire only when every web area is empty: one that has children
        // proves the app builds its tree, so an empty sibling (an iframe, a
        // placeholder) is not the reason for the miss.
        let mut all_empty = !areas.is_empty();
        for area in &areas {
            match self.provider.get_children(Some(area)) {
                Ok(children) => all_empty &= children.is_empty(),
                Err(e) => return Some(format!("web_area check failed: {e}")),
            }
        }
        all_empty.then(|| {
            "the scope has only web_areas with no children: a Chromium or Electron app builds \
             its web content's accessibility tree only on request. On macOS, enable web \
             accessibility for the app (App.enable_web_accessibility in Rust and Python, \
             enableWebAccessibility in JS, `xa11y enable-web-accessibility`, or the \
             `enable_web_accessibility` MCP tool). On Linux, relaunch the app with \
             --force-renderer-accessibility"
                .to_string()
        })
    }

    /// Near-miss candidates: elements in scope whose role matches the
    /// selector's target role (last segment of the first clause) regardless
    /// of attribute filters. Bounded by [`DIAG_CANDIDATE_LIMIT`].
    ///
    /// A failure to *collect* candidates is recorded as a candidate entry
    /// rather than dropped, so the original error keeps surfacing while the
    /// collection failure stays visible (tenet 1: no silent fallbacks).
    fn role_candidates(&self) -> Vec<String> {
        let Ok(group) = SelectorGroup::parse(&self.selector) else {
            // Unparseable selectors fail earlier with `InvalidSelector`;
            // a miss can't be reached without a parsed selector.
            return Vec::new();
        };
        let role = group
            .clauses
            .first()
            .and_then(|c| c.segments.last())
            .and_then(|s| s.simple.role.clone());
        let Some(role) = role else {
            // Role-less selectors (pure attribute filters) have no
            // meaningful "same role" candidate set.
            return Vec::new();
        };
        let role_only = SelectorGroup {
            clauses: vec![Selector {
                segments: vec![SelectorSegment {
                    combinator: Combinator::Root,
                    simple: SimpleSelector {
                        role: Some(role),
                        filters: Vec::new(),
                        nth: None,
                    },
                }],
            }],
        };
        match self.resolve_group(&role_only, Some(DIAG_CANDIDATE_LIMIT)) {
            Ok(matches) => matches.iter().map(short_describe).collect(),
            Err(e) => vec![format!("(candidate search failed: {e})")],
        }
    }

    /// Bounded rendering of the search scope: a depth-limited, line-capped
    /// dump of the scope root, or the application list for rootless
    /// locators. Collection failures are recorded in place of the snapshot.
    fn scope_summary(&self) -> String {
        match self.root.as_ref() {
            Some(root) => {
                let el = Element::new(root.clone(), Arc::clone(&self.provider));
                match el.dump(Some(DIAG_SCOPE_MAX_DEPTH)) {
                    Ok(dump) => truncate_lines(&dump, DIAG_SCOPE_MAX_LINES),
                    Err(e) => format!("(scope dump failed: {e})"),
                }
            }
            None => match self.provider.list_apps() {
                Ok(apps) => {
                    let total = apps.len();
                    let mut lines: Vec<String> = apps
                        .iter()
                        .take(DIAG_SCOPE_MAX_LINES)
                        .map(|a| {
                            let pid = a.pid.map(|p| format!(" (pid={p})")).unwrap_or_default();
                            format!(
                                "application \"{}\"{pid}",
                                a.name.clone().unwrap_or_default()
                            )
                        })
                        .collect();
                    if total > DIAG_SCOPE_MAX_LINES {
                        lines.push(format!(
                            "… (+{} more applications)",
                            total - DIAG_SCOPE_MAX_LINES
                        ));
                    }
                    lines.join("\n")
                }
                Err(e) => format!("(application enumeration failed: {e})"),
            },
        }
    }

    /// Build the diagnosed timeout error shared by `auto_wait` and
    /// `poll_until`. `last` is the most recent poll's observation; the
    /// expensive miss context is collected only when the selector never
    /// matched during the entire wait.
    ///
    /// `auto_scroll` is what an action's auto-wait did about a scrolled-out
    /// target; `poll_until` passes `None`, since a plain wait never scrolls.
    fn diagnose_timeout(
        &self,
        elapsed: Duration,
        condition: String,
        last: Option<&ElementData>,
        ever_matched: bool,
        auto_scroll: Option<&AutoScroll>,
    ) -> Error {
        self.diagnose_wait(elapsed, condition, last, ever_matched, auto_scroll, true)
    }

    /// [`Self::diagnose_timeout`], with the miss context (a search of the
    /// scope for candidates) collected only when `miss_context` is set.
    fn diagnose_wait(
        &self,
        elapsed: Duration,
        condition: String,
        last: Option<&ElementData>,
        ever_matched: bool,
        auto_scroll: Option<&AutoScroll>,
        miss_context: bool,
    ) -> Error {
        let mut d = Diagnosis {
            condition: Some(condition),
            selector: Some(self.selector.clone()),
            // A wait gives up before it acts, so nothing was dispatched.
            delivery: Some(Delivery::NotDelivered),
            auto_scroll: auto_scroll.and_then(AutoScroll::outcome),
            ..Diagnosis::default()
        };
        match last {
            Some(data) => {
                let mut observed = describe_observed(data);
                if let Some(note) = auto_scroll.and_then(AutoScroll::note) {
                    observed = format!("{observed}; {note}");
                }
                d.last_observed = Some(observed);
                // `visible=false` alone does not say what would fix it (issue
                // #590): a scrolled-out element needs `scroll_into_view`, a
                // minimized window needs restoring.
                d.hidden_reason = data.states.hidden_reason;
            }
            None if ever_matched => {
                d.last_observed =
                    Some("selector matched earlier in the wait but no longer matches".to_string());
            }
            None => {
                d.last_observed = Some("selector never matched".to_string());
                if miss_context {
                    crate::deadline::for_diagnosis(|| self.fill_miss_context(&mut d));
                }
            }
        }
        Error::timeout(elapsed).diagnose(d)
    }

    /// The error a wait returns when a provider read fails.
    ///
    /// A read cut short by the wait's own deadline becomes the wait's
    /// timeout, diagnosed from the last poll that completed and naming the
    /// read that was still in flight ([`Diagnosis::pending`]), so the error
    /// says which selector and condition it gave up on as well as which call
    /// was pending. Every other error passes through unchanged.
    ///
    /// A "never matched" timeout still carries its candidates and scope
    /// snapshot (tenet 6), collected under the short
    /// [`CUT_DIAGNOSIS_BUDGET`](crate::deadline::CUT_DIAGNOSIS_BUDGET) rather
    /// than the full diagnosis budget: the read that was cut may mean the
    /// application is hung, and the wait must still end near its timeout. A
    /// snapshot read that runs out of time is recorded in the snapshot.
    fn read_failed(
        &self,
        err: Error,
        elapsed: Duration,
        condition: impl FnOnce() -> String,
        last: Option<&ElementData>,
        ever_matched: bool,
        auto_scroll: Option<&AutoScroll>,
    ) -> Error {
        let Some(pending) = crate::deadline::pending_of(&err) else {
            return err;
        };
        let mut timeout =
            self.diagnose_wait(elapsed, condition(), last, ever_matched, auto_scroll, false);
        if let Error::Timeout {
            diagnosis: Some(d), ..
        } = &mut timeout
        {
            d.pending = pending;
            if last.is_none() && !ever_matched {
                crate::deadline::for_cut_diagnosis(|| self.fill_miss_context(d));
            }
        }
        timeout
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
    ///
    /// Fails fast with [`Error::SelectorNotMatched`] on a miss; the error
    /// carries a bounded [`Diagnosis`] of the search scope (same-role
    /// candidates and a depth-limited scope snapshot) so the caller doesn't
    /// need to re-run the failure under manual tree dumps.
    pub fn element(&self) -> Result<Element> {
        match self.resolve_data() {
            Ok(data) => Ok(Element::new(data, Arc::clone(&self.provider))),
            // Terminal site: this miss reaches the caller, so spend the
            // bounded diagnosis cost here (not in `resolve_data`, which
            // poll loops call on every tick).
            Err(e) => Err(self.enrich_miss(e)),
        }
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

    /// Read the subtree rooted at the matched element into a bounded
    /// [`Capture`](crate::capture::Capture). Resolves the selector once (no
    /// auto-wait), like [`Self::tree`].
    pub fn capture(
        &self,
        options: &crate::capture::CaptureOptions,
    ) -> Result<crate::capture::Capture> {
        self.element()?.capture(options)
    }

    /// Render the subtree rooted at the matched element as an indented
    /// string. Resolves the selector once (no auto-wait). See
    /// [`Element::dump`] for the output format.
    pub fn dump(&self, max_depth: Option<usize>) -> Result<String> {
        self.element()?.dump(max_depth)
    }

    /// Read the matched element's text by range. See
    /// [`crate::text_api::Text`].
    ///
    /// The returned [`Text`](crate::text_api::Text) re-resolves this locator
    /// on every read, waiting (up to the locator's timeout) until the selector
    /// matches. Reads do not wait for the element to be visible or enabled.
    pub fn text(&self) -> crate::text_api::Text {
        crate::text_api::Text::for_locator(self.clone())
    }

    /// Auto-wait for `action`'s gate and return the element, for reads that
    /// re-resolve per call ([`crate::text_api::Text`]).
    pub(crate) fn resolve_for_read(&self, action: &str) -> Result<Element> {
        self.auto_wait(action)
    }

    // ── Auto-wait ──────────────────────────────────────────────────

    /// Effective auto-wait timeout: the explicit [`with_timeout`] value if
    /// set, otherwise the process-wide default (resolved at use time so
    /// [`crate::config::set_default_timeout`] affects existing locators).
    fn effective_timeout(&self) -> Result<Duration> {
        match self.timeout {
            Some(t) => Ok(t),
            None => crate::config::default_timeout(),
        }
    }

    /// Poll until the element satisfies `gate`, returning a live [`Element`]
    /// handle. Used by the action methods below to provide resilience against
    /// transient unactionable states.
    ///
    /// `action` names the calling action for the timeout diagnosis, so a
    /// failed `press()` reports *what* was being attempted and what the
    /// last poll observed.
    ///
    /// `action` also selects the gate ([`Actionability::for_action`]): the
    /// conditions the action requires. Most verbs use
    /// [`Actionability::VISIBLE_AND_ENABLED`]; `scroll_into_view` uses
    /// [`Actionability::ENABLED`], because gating it on `visible` is circular —
    /// it is the one verb whose purpose is to make an off-screen element
    /// visible, so requiring visibility means it can never act on exactly the
    /// elements that need it (issue #350).
    ///
    /// A target that fails a visible-requiring gate only because it is
    /// scrolled out is scrolled into view with the platform's semantic
    /// `scroll_into_view` while the wait runs; see [`AutoScroll`].
    ///
    /// Always performs at least one attempt before checking the deadline, so
    /// `Duration::ZERO` means "single attempt, no polling" — the same
    /// contract as `poll_lookup` in `app.rs`.
    fn auto_wait(&self, action: &str) -> Result<Element> {
        let first_match = || match self.resolve_data() {
            Ok(data) => Ok(ControlFlow::Continue(Some(data))),
            // Not yet present — poll again.
            Err(Error::SelectorNotMatched { .. }) => Ok(ControlFlow::Continue(None)),
            Err(e) => Err(e),
        };
        match self.auto_wait_with::<Infallible>(action, first_match)? {
            ControlFlow::Continue(element) => Ok(element),
            ControlFlow::Break(never) => match never {},
        }
    }

    /// Auto-wait for `action` as the named action methods do, but over the
    /// whole match set, and hand back the element to act on instead of
    /// acting.
    ///
    /// Returns `Ok(Ok(element))` once the selector matches exactly one
    /// element and that element passes the action's gate, and `Ok(Err(all))`
    /// as soon as a poll sees more than one match. The caller then acts on
    /// the returned [`Element`] itself, so the element that was counted is
    /// the element acted on: counting with [`Self::elements`] and then
    /// calling an action method re-resolves in between, and a UI that changes
    /// in that gap gets the action on an element nobody counted. Zero matches
    /// and a not-yet-actionable match keep polling, and the timeout carries
    /// the same diagnosis as the action methods'.
    ///
    /// `action` is the action method's name (`press`, `show_menu`,
    /// `move_to`, …), which selects its gate. An explicit [`Self::nth`]
    /// already picks one match, so it is honoured rather than checked.
    ///
    /// Hidden: this exists for consumers that promise "exactly one" (the MCP
    /// `action` tool), and is not part of the binding surface.
    #[doc(hidden)]
    pub fn auto_wait_unique(
        &self,
        action: &str,
    ) -> Result<std::result::Result<Element, Vec<Element>>> {
        if self.nth.is_some() {
            return self.auto_wait(action).map(Ok);
        }
        let only_match = || {
            let group = SelectorGroup::parse(&self.selector)?;
            // Two matches are enough to know the answer is "several", so
            // each poll asks for at most two and keeps the backend's limit
            // pushdown. Only a refusal pays for the full list, which is what
            // its candidate diagnosis needs; the element acted on still comes
            // from the single resolution that counted it.
            let mut first_two = self.resolve_group(&group, Some(2))?;
            if first_two.len() > 1 {
                return Ok(ControlFlow::Break(self.resolve_group(&group, None)?));
            }
            Ok(ControlFlow::Continue(first_two.pop()))
        };
        Ok(match self.auto_wait_with(action, only_match)? {
            ControlFlow::Continue(element) => Ok(element),
            ControlFlow::Break(all) => Err(all
                .into_iter()
                .map(|d| Element::new(d, Arc::clone(&self.provider)))
                .collect()),
        })
    }

    /// The poll loop behind [`Self::auto_wait`] and [`Self::auto_wait_unique`].
    ///
    /// `resolve` is one poll: `Continue(Some(data))` is a candidate to gate,
    /// `Continue(None)` is "not present yet", and `Break(b)` ends the wait
    /// with `b` immediately.
    fn auto_wait_with<B>(
        &self,
        action: &str,
        mut resolve: impl FnMut() -> Result<ControlFlow<B, Option<ElementData>>>,
    ) -> Result<ControlFlow<B, Element>> {
        let gate = Actionability::for_action(action)?;
        let timeout = self.effective_timeout()?;
        crate::deadline::within(timeout, || {
            self.auto_wait_loop(action, gate, timeout, &mut resolve)
        })
    }

    /// The body of [`Self::auto_wait_with`], run inside its deadline.
    fn auto_wait_loop<B>(
        &self,
        action: &str,
        gate: Actionability,
        timeout: Duration,
        resolve: &mut impl FnMut() -> Result<ControlFlow<B, Option<ElementData>>>,
    ) -> Result<ControlFlow<B, Element>> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(100);
        let mut ever_matched = false;
        let mut auto_scroll = AutoScroll::default();
        let condition = || format!("{action} target actionable ({})", gate.label());
        // The previous poll's observation, for a timeout that lands inside
        // a read and so has none of its own.
        let mut previous: Option<ElementData> = None;

        loop {
            let resolved = match crate::deadline::distrust(resolve()) {
                Ok(ControlFlow::Continue(resolved)) => resolved,
                Ok(ControlFlow::Break(b)) => return Ok(ControlFlow::Break(b)),
                Err(e) => {
                    return Err(self.read_failed(
                        e,
                        start.elapsed(),
                        condition,
                        previous.as_ref(),
                        ever_matched,
                        Some(&auto_scroll),
                    ))
                }
            };

            // This poll's observation, for the timeout diagnosis.
            let mut scrolled = false;
            let observed = match resolved {
                Some(data) if gate.is_met(&data) => {
                    return Ok(ControlFlow::Continue(Element::new(
                        data,
                        Arc::clone(&self.provider),
                    )));
                }
                Some(data) => {
                    // Matched but not yet actionable. If only scrolling stands
                    // in the way, ask the platform to scroll; either way the
                    // next poll re-reads the element and re-checks the gate.
                    ever_matched = true;
                    auto_scroll.observe(&data);
                    scrolled = match crate::deadline::distrust(auto_scroll.attempt(
                        gate,
                        &data,
                        self.provider.as_ref(),
                    )) {
                        Ok(scrolled) => scrolled,
                        Err(e) => {
                            return Err(self.read_failed(
                                e,
                                start.elapsed(),
                                condition,
                                Some(&data),
                                ever_matched,
                                Some(&auto_scroll),
                            ))
                        }
                    };
                    Some(data)
                }
                None => None,
            };

            // With `Duration::ZERO` a scroll the platform just accepted is
            // followed by one more poll: otherwise the single attempt would
            // time out on the observation taken before the scroll, which says
            // nothing about whether it worked. A non-zero timeout is a hard
            // deadline (issue #531), so there the scroll gets its re-check
            // only if time is left. A timeout before that re-check reports
            // the previous scroll's outcome, or none for the first, since no
            // poll observed what this one did.
            let elapsed = start.elapsed();
            if scrolled && timeout.is_zero() {
                std::thread::sleep(poll_interval);
            } else if elapsed >= timeout || !crate::deadline::pause(start, timeout, poll_interval) {
                return Err(self.diagnose_timeout(
                    start.elapsed(),
                    condition(),
                    observed.as_ref(),
                    ever_matched,
                    Some(&auto_scroll),
                ));
            }
            previous = observed;
        }
    }

    /// Auto-wait for `action`, then perform it with `perform`, all under one
    /// deadline of the locator's timeout: a hung application cannot stretch
    /// the action past it, in the wait or in the platform call (issue #531).
    fn act(&self, action: &str, perform: impl FnOnce(&Element) -> Result<()>) -> Result<()> {
        let timeout = self.effective_timeout()?;
        crate::deadline::within(timeout, || {
            let target = self.auto_wait(action)?;
            perform(&target).map_err(crate::deadline::action_error)
        })
    }

    // ── Actions ────────────────────────────────────────────────────
    //
    // Locator actions auto-wait for the element to be visible and enabled
    // (re-resolving the selector on each poll), then delegate to the
    // [`Element`] action of the same name. A target hidden only by being
    // scrolled out is scrolled into view during the wait ([`AutoScroll`]). For snapshot-bound actions that
    // do not re-resolve, capture an [`Element`] via [`Locator::element`]
    // and call its action methods directly.

    /// Click / invoke the matched element.
    pub fn press(&self) -> Result<()> {
        self.act("press", |e| e.press())
    }

    /// Set keyboard focus on the matched element.
    pub fn focus(&self) -> Result<()> {
        self.act("focus", |e| e.focus())
    }

    /// Remove keyboard focus from the matched element.
    pub fn blur(&self) -> Result<()> {
        self.act("blur", |e| e.blur())
    }

    /// Toggle the matched element (checkbox, switch).
    pub fn toggle(&self) -> Result<()> {
        self.act("toggle", |e| e.toggle())
    }

    /// Select the matched element (list item, etc.).
    pub fn select(&self) -> Result<()> {
        self.act("select", |e| e.select())
    }

    /// Expand the matched element.
    pub fn expand(&self) -> Result<()> {
        self.act("expand", |e| e.expand())
    }

    /// Collapse the matched element.
    pub fn collapse(&self) -> Result<()> {
        self.act("collapse", |e| e.collapse())
    }

    /// Show the context menu for the matched element.
    pub fn show_menu(&self) -> Result<()> {
        self.act("show_menu", |e| e.show_menu())
    }

    /// Increment the matched element (slider, spinner).
    pub fn increment(&self) -> Result<()> {
        self.act("increment", |e| e.increment())
    }

    /// Decrement the matched element (slider, spinner).
    pub fn decrement(&self) -> Result<()> {
        self.act("decrement", |e| e.decrement())
    }

    /// Scroll the matched element into view.
    ///
    /// Unlike the other verbs, this waits only for the target to be attached
    /// and `enabled`, **not** `visible`: an off-screen element is precisely
    /// what `scroll_into_view` exists to bring on-screen, so gating on
    /// visibility would be circular and would time out on the elements that
    /// most need scrolling (issue #350).
    pub fn scroll_into_view(&self) -> Result<()> {
        self.act("scroll_into_view", |e| e.scroll_into_view())
    }

    /// Set the text value of the matched element.
    pub fn set_value(&self, value: &str) -> Result<()> {
        self.act("set_value", |e| e.set_value(value))
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
        self.act("set_numeric_value", |e| e.set_numeric_value(value))
    }

    /// Type text at the current cursor position on the matched element.
    pub fn type_text(&self, text: &str) -> Result<()> {
        self.act("type_text", |e| e.type_text(text))
    }

    /// Select a text range within the matched element.
    pub fn select_text(&self, start: u32, end: u32) -> Result<()> {
        crate::text_api::TextRange::new(start, end)?;
        self.act("select_text", |e| e.select_text(start, end))
    }

    /// Perform an action by name (with auto-wait).
    ///
    /// This is the escape hatch for platform-specific actions not covered
    /// by the named methods above. Also works for well-known action names.
    pub fn perform_action(&self, action: &str) -> Result<()> {
        // The nullary window verbs route through their typed methods so the
        // enabled-only gate applies on this path too: a minimized window is
        // legitimately invisible, and `perform_action("restore")` must reach
        // it exactly as `restore()` does (the generic gate below would wait
        // for visible AND enabled and time out on the very window that needs
        // restoring). Mirrors `Element::perform_action`, which routes the
        // same names before consulting the provider. `move_to` / `resize_to`
        // carry payloads the generic path cannot hand them, so they stay
        // here — the providers reject them surfaceably before any OS call.
        match action {
            "activate" => return self.activate(),
            "minimize" => return self.minimize(),
            "maximize" => return self.maximize(),
            "enter_fullscreen" => return self.enter_fullscreen(),
            "restore" => return self.restore(),
            "close" => return self.close(),
            _ => {}
        }
        self.act("perform_action", |e| e.perform_action(action))
    }

    // ── Window management ─────────────────────────────────────────
    //
    // Window verbs wait only on `enabled`, not `visible` (we reuse
    // [`Actionability::ENABLED`]): a minimized window is legitimately
    // not visible, and gate the very verbs that must act on it —
    // `minimize`, `restore`, and `activate` all need to reach an invisible
    // window. The same argument that relaxed `scroll_into_view` (issue
    // #350) applies here, so the gate is documented once on
    // [`Actionability::ENABLED`].

    /// Activate the matched window: bring it to the foreground and give it
    /// focus.
    pub fn activate(&self) -> Result<()> {
        self.act("activate", |e| e.activate())
    }

    /// Minimize the matched window.
    pub fn minimize(&self) -> Result<()> {
        self.act("minimize", |e| e.minimize())
    }

    /// Maximize the matched window.
    ///
    /// Distinct from [`Self::enter_fullscreen`]: this drives the platform's
    /// maximized state, not native fullscreen.
    pub fn maximize(&self) -> Result<()> {
        self.act("maximize", |e| e.maximize())
    }

    /// Put the matched window in native fullscreen.
    ///
    /// Distinct from [`Self::maximize`]: fullscreen is the platform's native
    /// fullscreen state (macOS `AXFullScreen`); [`Self::restore`] leaves it.
    pub fn enter_fullscreen(&self) -> Result<()> {
        self.act("enter_fullscreen", |e| e.enter_fullscreen())
    }

    /// Restore the matched window to its normal state (from minimized,
    /// maximized, or fullscreen).
    ///
    /// This is the inverse of [`Self::minimize`], [`Self::maximize`], and
    /// [`Self::enter_fullscreen`]: it clears every special state the platform
    /// can clear. There is deliberately no separate "exit fullscreen" verb —
    /// leaving fullscreen is the same absolute state write this performs, not
    /// a distinct operation.
    pub fn restore(&self) -> Result<()> {
        self.act("restore", |e| e.restore())
    }

    /// Close the matched window.
    pub fn close(&self) -> Result<()> {
        self.act("close", |e| e.close())
    }

    /// Move the matched window to the given **logical** screen coordinates.
    pub fn move_to(&self, x: i32, y: i32) -> Result<()> {
        self.act("move_to", |e| e.move_to(x, y))
    }

    /// Resize the matched window to the given **logical** dimensions.
    ///
    /// Returns [`Error::InvalidActionData`] if either dimension is 0,
    /// before any auto-wait polling begins.
    pub fn resize_to(&self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidActionData {
                message: format!(
                    "resize_to requires positive width and height, got {width}x{height}"
                ),
            });
        }
        self.act("resize_to", |e| e.resize_to(width, height))
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
        self.poll_until(|element| state.is_met(element), timeout, state_label(state))
    }

    /// Wait until an arbitrary predicate is satisfied, polling at ~100 ms intervals.
    pub fn wait_until(
        &self,
        predicate: impl Fn(Option<&ElementData>) -> bool,
        timeout: Duration,
    ) -> Result<Option<Element>> {
        self.poll_until(&predicate, timeout, "custom predicate")
    }

    /// Wait until visible bounds have been observed unchanged for at least
    /// 750 ms across repeated fresh polls, then return the element.
    ///
    /// "Stable" is geometric: it has nothing to do with
    /// [`ElementData::stable_id`]. Use it before a pointer action
    /// ([`crate::InputSim`]) whose target is still animating — sliding in,
    /// resizing, or settling after a layout change — so the click lands
    /// where the element ends up rather than where it was mid-motion:
    ///
    /// ```ignore
    /// sim.mouse().click(&locator.wait_stable(timeout)?)?;
    /// ```
    ///
    /// Each poll re-resolves the selector (~100 ms apart, like the other
    /// `wait_*` methods). Even a still element needs at least 750 ms after
    /// the first observation. A changed, missing, hidden, or bounds-less
    /// observation resets that window. This is observed geometric stability:
    /// native accessibility snapshots can lag the screen, and a pause does
    /// not guarantee that an animation will never resume. Semantic actions
    /// (`press`, `toggle`, …) do not need this:
    /// they do not depend on position.
    ///
    /// Only bounds are compared, not identity: if the selector resolves to a
    /// different element whose bounds equal the previous one's, that element
    /// continues the observation window. For pointer targeting this is
    /// harmless — the point clicked is the same either way.
    ///
    /// On timeout the error's [`Diagnosis`] says what blocked stability: the
    /// last two differing rectangles and the observed unchanged duration,
    /// a match that is not visible, absent bounds, or no match.
    pub fn wait_stable(&self, timeout: Duration) -> Result<Element> {
        crate::deadline::within(timeout, || self.wait_stable_loop(timeout))
    }

    /// The body of [`Self::wait_stable`], run inside its deadline.
    fn wait_stable_loop(&self, timeout: Duration) -> Result<Element> {
        let start = std::time::Instant::now();
        let mut ever_matched = false;
        let mut stability = StableBounds::default();
        let mut previous: Option<ElementData> = None;
        let mut previous_elapsed = Duration::ZERO;

        loop {
            let matched = match crate::deadline::distrust(self.resolve_data()) {
                Ok(data) => Some(data),
                Err(Error::SelectorNotMatched { .. }) => None,
                Err(e) => {
                    // A read the deadline cut ends the wait on the previous
                    // poll's observation, which still says whether the
                    // bounds were moving. Without one, `read_failed` says
                    // the selector never matched.
                    if let (Some(_), Some(pending)) = (&previous, crate::deadline::pending_of(&e)) {
                        let elapsed = start.elapsed();
                        let mut err = self.diagnose_unstable(
                            elapsed,
                            previous.as_ref(),
                            ever_matched,
                            stability.last_change,
                            stability.unchanged_for(previous_elapsed),
                        );
                        if let Error::Timeout {
                            diagnosis: Some(d), ..
                        } = &mut err
                        {
                            d.pending = pending;
                        }
                        return Err(err);
                    }
                    return Err(self.read_failed(
                        e,
                        start.elapsed(),
                        || STABLE_CONDITION.to_string(),
                        previous.as_ref(),
                        ever_matched,
                        None,
                    ));
                }
            };
            if matched.is_some() {
                ever_matched = true;
            }

            let elapsed = start.elapsed();
            if stability.observe(matched.as_ref().and_then(stable_candidate), elapsed) {
                let data = matched.expect("stable bounds come from a visible match");
                return Ok(Element::new(data, Arc::clone(&self.provider)));
            }
            // Preserve the other wait methods' convention: evaluate the
            // observation before checking the overall timeout.
            if elapsed >= timeout || !crate::deadline::pause(start, timeout, WAIT_POLL_INTERVAL) {
                // "Unchanged for" runs to the last observation, not to now:
                // nothing was observed in the pause that ended the wait.
                return Err(self.diagnose_unstable(
                    start.elapsed(),
                    matched.as_ref(),
                    ever_matched,
                    stability.last_change,
                    stability.unchanged_for(elapsed),
                ));
            }
            previous = matched;
            previous_elapsed = elapsed;
        }
    }

    /// Timeout error for [`Locator::wait_stable`]. Reuses
    /// [`Locator::diagnose_timeout`] for the "never matched" / "not visible"
    /// cases and names the moving bounds otherwise.
    fn diagnose_unstable(
        &self,
        elapsed: Duration,
        last: Option<&ElementData>,
        ever_matched: bool,
        last_change: Option<(Rect, Rect)>,
        unchanged_for: Option<Duration>,
    ) -> Error {
        let condition = STABLE_CONDITION.to_string();
        let Some(data) = last else {
            return self.diagnose_timeout(elapsed, condition, None, ever_matched, None);
        };
        let observed = if !data.states.visible {
            format!("{}; matched but not visible", describe_observed(data))
        } else {
            match (data.bounds, last_change) {
                (None, _) => format!("{} reports no bounds", describe_observed(data)),
                (Some(_), Some((before, after))) => format!(
                    "bounds still changing: {} \u{2192} {} on {}; latest bounds observed \
                     unchanged for {} ms (750 ms required)",
                    describe_rect(&before),
                    describe_rect(&after),
                    short_describe(data),
                    unchanged_for.unwrap_or_default().as_millis()
                ),
                (Some(b), None) => format!(
                    "bounds {} on {} observed unchanged for {} ms; stability needs \
                     at least 750 ms of unchanged visible bounds",
                    describe_rect(&b),
                    short_describe(data),
                    unchanged_for.unwrap_or_default().as_millis()
                ),
            }
        };
        Error::timeout(elapsed).diagnose(Diagnosis {
            condition: Some(condition),
            selector: Some(self.selector.clone()),
            last_observed: Some(observed),
            delivery: Some(Delivery::NotDelivered),
            ..Diagnosis::default()
        })
    }

    /// Core polling loop shared by `wait_for_state` and `wait_until`.
    ///
    /// `condition` names what is being waited for; on timeout it is embedded
    /// in the error's [`Diagnosis`] together with the last observation.
    ///
    /// Always evaluates the predicate at least once before checking the
    /// deadline, so `Duration::ZERO` means "single check, no polling".
    fn poll_until(
        &self,
        predicate: impl Fn(Option<&ElementData>) -> bool,
        timeout: Duration,
        condition: &str,
    ) -> Result<Option<Element>> {
        crate::deadline::within(timeout, || self.poll_loop(&predicate, timeout, condition))
    }

    /// The body of [`Self::poll_until`], run inside its deadline.
    fn poll_loop(
        &self,
        predicate: &impl Fn(Option<&ElementData>) -> bool,
        timeout: Duration,
        condition: &str,
    ) -> Result<Option<Element>> {
        let start = std::time::Instant::now();
        let mut ever_matched = false;
        let mut previous: Option<ElementData> = None;

        loop {
            // This poll's observation, for the timeout diagnosis.
            let matched = match crate::deadline::distrust(self.resolve_data()) {
                Ok(data) => Some(data),
                Err(Error::SelectorNotMatched { .. }) => None,
                Err(e) => {
                    return Err(self.read_failed(
                        e,
                        start.elapsed(),
                        || condition.to_string(),
                        previous.as_ref(),
                        ever_matched,
                        None,
                    ))
                }
            };
            if matched.is_some() {
                ever_matched = true;
            }

            if predicate(matched.as_ref()) {
                return Ok(matched.map(|data| Element::new(data, Arc::clone(&self.provider))));
            }

            if start.elapsed() >= timeout
                || !crate::deadline::pause(start, timeout, WAIT_POLL_INTERVAL)
            {
                return Err(self.diagnose_timeout(
                    start.elapsed(),
                    condition.to_string(),
                    matched.as_ref(),
                    ever_matched,
                    None,
                ));
            }
            previous = matched;
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

    // ── Web accessibility hint ──────────────────────────────────────

    fn miss_last_observed(filled: &[bool]) -> Option<String> {
        let provider: Arc<dyn Provider> = crate::mock::build_web_area_provider(filled);
        let root = provider
            .get_children(None)
            .expect("the mock lists its app")
            .remove(0);
        let err = Locator::new_for_app(provider, root, r#"button[name="Nope"]"#)
            .with_timeout(Duration::ZERO)
            .element()
            .expect_err("the button does not exist");
        err.diagnosis().and_then(|d| d.last_observed.clone())
    }

    #[test]
    fn hint_fires_when_every_web_area_is_empty() {
        let observed = miss_last_observed(&[false, false]).expect("a hint");
        assert!(observed.contains("enable_web_accessibility"), "{observed}");
    }

    #[test]
    fn hint_is_absent_when_any_web_area_has_content() {
        // An iframe's empty web area beside a built page is not an unbuilt tree.
        let observed = miss_last_observed(&[true, false]).unwrap_or_default();
        assert!(!observed.contains("enable_web_accessibility"), "{observed}");
    }

    #[test]
    fn hint_is_absent_without_web_areas() {
        let observed = miss_last_observed(&[]).unwrap_or_default();
        assert!(!observed.contains("enable_web_accessibility"), "{observed}");
    }

    // ── Auto-wait / wait timeout semantics ──────────────────────────

    #[test]
    fn auto_wait_zero_timeout_performs_single_attempt() {
        // `Duration::ZERO` must still perform exactly one resolve attempt
        // (attempt-then-check, matching `poll_lookup` in app.rs) — a visible,
        // enabled element is acted on immediately, not failed with Timeout.
        let loc = root_locator(r#"button[name="Back"]"#).with_timeout(Duration::ZERO);
        loc.press()
            .expect("zero timeout must still allow one successful attempt");
    }

    #[test]
    fn auto_wait_zero_timeout_fails_fast_on_missing_element() {
        let loc = root_locator(r#"button[name="DoesNotExist"]"#).with_timeout(Duration::ZERO);
        let start = std::time::Instant::now();
        let err = loc.press().expect_err("press must time out on a miss");
        assert!(matches!(err, Error::Timeout { .. }), "got {err:?}");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "zero timeout must not poll; took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn wait_visible_zero_timeout_performs_single_check() {
        let el = root_locator(r#"button[name="Back"]"#)
            .wait_visible(Duration::ZERO)
            .expect("zero timeout must still allow one successful check");
        assert_eq!(el.data().name.as_deref(), Some("Back"));
    }

    #[test]
    fn wait_detached_zero_timeout_succeeds_when_already_absent() {
        root_locator(r#"button[name="DoesNotExist"]"#)
            .wait_detached(Duration::ZERO)
            .expect("an absent element is already detached");
    }

    // ── scroll_into_view actionability gate (issue #350) ─────────────

    #[test]
    fn scroll_into_view_acts_on_offscreen_element() {
        // static_text "Status" is enabled but visible=false — the exact state
        // scroll_into_view exists to fix. Gating it on `visible` (as every
        // other verb is) would be circular and time out; it must act instead.
        let provider = build_provider();
        let handle = Arc::clone(&provider);
        let provider_dyn: Arc<dyn Provider> = provider;
        let loc = Locator::new(provider_dyn, None, r#"static_text[name="Status"]"#)
            .with_timeout(Duration::ZERO);
        loc.scroll_into_view()
            .expect("scroll_into_view must act on an off-screen (visible=false) element");
        // And it must reach the provider, not short-circuit.
        assert!(
            handle
                .actions()
                .iter()
                .any(|(_, action, _)| action == "scroll_into_view"),
            "scroll_into_view must delegate to the provider"
        );
    }

    #[test]
    fn visible_gated_verb_still_blocks_on_offscreen_element() {
        // The gate relaxation is scoped to scroll_into_view: press() on the
        // same off-screen element must still wait on `visible` and time out.
        let loc = root_locator(r#"static_text[name="Status"]"#).with_timeout(Duration::ZERO);
        let err = loc
            .press()
            .expect_err("press must still require visibility");
        assert!(matches!(err, Error::Timeout { .. }), "got {err:?}");
        let d = err.diagnosis().expect("timeout must carry a diagnosis");
        assert_eq!(
            d.condition.as_deref(),
            Some("press target actionable (visible && enabled)")
        );
    }

    #[test]
    fn scroll_into_view_still_requires_enabled() {
        // The relaxed gate drops `visible`, not `enabled`: a disabled target
        // has no operable accessibility action, so scroll_into_view times out
        // with a diagnosis naming the enabled-only condition.
        let loc = root_locator(r#"button[name="Forward"]"#).with_timeout(SHORT_WAIT);
        let err = loc
            .scroll_into_view()
            .expect_err("disabled element must time out");
        let d = err.diagnosis().expect("timeout must carry a diagnosis");
        assert_eq!(
            d.condition.as_deref(),
            Some("scroll_into_view target actionable (enabled)")
        );
        let last = d.last_observed.as_deref().unwrap_or_default();
        assert!(
            last.contains("enabled=false"),
            "last observed should expose the blocking state, got: {last}"
        );
    }

    // ── Window management gate (enabled-only) ───────────────────────

    #[test]
    fn window_verbs_act_on_minimized_invisible_window() {
        // Window verbs use the enabled-only gate, not visible&&enabled: a
        // minimized window is visible=false, and minimizing it is the thing
        // `restore` must then reverse. If the gate required `visible`, this
        // zero-timeout restore would fail with a Timeout.
        let provider = build_provider();
        let handle = Arc::clone(&provider);
        let provider_dyn: Arc<dyn Provider> = provider;
        let min = Locator::new(provider_dyn.clone(), None, "window").with_timeout(Duration::ZERO);
        min.minimize()
            .expect("minimize must act on the (initially visible) window");
        // The window is now minimized (visible=false); restore still acts.
        let restore = Locator::new(provider_dyn, None, "window").with_timeout(Duration::ZERO);
        restore
            .restore()
            .expect("restore must act on a minimized window");
        assert!(
            handle
                .actions()
                .iter()
                .any(|(_, action, _)| action == "restore"),
            "restore must delegate to the provider"
        );
    }

    #[test]
    fn perform_action_routes_window_verbs_through_the_enabled_only_gate() {
        // `Locator::perform_action("restore")` must behave like `restore()`:
        // the generic name-based path auto-waits for visible&&enabled, which
        // times out on the minimized window it was asked to restore — the
        // bindings expose this generic path. Same routing rule as
        // `Element::perform_action`: window verbs take the typed route.
        let provider = build_provider();
        let handle = Arc::clone(&provider);
        let provider_dyn: Arc<dyn Provider> = provider;
        let min = Locator::new(provider_dyn.clone(), None, "window").with_timeout(Duration::ZERO);
        min.minimize()
            .expect("minimize must act on the (initially visible) window");
        let restore = Locator::new(provider_dyn, None, "window").with_timeout(Duration::ZERO);
        restore
            .perform_action("restore")
            .expect("perform_action(\"restore\") must reach a minimized window");
        assert!(
            handle
                .actions()
                .iter()
                .any(|(_, action, _)| action == "restore"),
            "restore must delegate to the provider"
        );
    }

    #[test]
    fn locator_fullscreen_minimize_sequence_uses_the_enabled_only_gate() {
        // The `max, min, max, min` sequence the integration suites drive:
        // every step must act directly on the window the previous step left
        // behind (fullscreen is not visible-gated either), and the name-based
        // path must route `enter_fullscreen` exactly like the typed method.
        let provider = build_provider();
        let handle = Arc::clone(&provider);
        let provider_dyn: Arc<dyn Provider> = provider;
        let loc = Locator::new(provider_dyn.clone(), None, "window").with_timeout(Duration::ZERO);
        loc.perform_action("enter_fullscreen")
            .expect("perform_action(\"enter_fullscreen\") must route to the typed method");
        assert!(
            handle
                .actions()
                .iter()
                .any(|(_, action, _)| action == "enter_fullscreen"),
            "enter_fullscreen must delegate to the provider"
        );
        // A fullscreen window is visible, but the next step must not depend on
        // that: minimize acts via the enabled-only gate and exits fullscreen
        // first (the mock mirrors the macOS hand-off).
        let loc = Locator::new(provider_dyn.clone(), None, "window").with_timeout(Duration::ZERO);
        loc.minimize()
            .expect("minimize must act on a fullscreen window");
        let loc = Locator::new(provider_dyn, None, "window").with_timeout(Duration::ZERO);
        loc.restore()
            .expect("restore must act on the minimized window it left");
    }

    #[test]
    fn perform_action_keeps_the_visible_gate_for_non_window_names() {
        // The routing above is scoped to the nullary window verbs: a name the
        // generic path still handles keeps its visible&&enabled gate, so an
        // invisible window is not silently actionable through it.
        let provider = build_provider();
        let provider_dyn: Arc<dyn Provider> = provider;
        let min = Locator::new(provider_dyn.clone(), None, "window").with_timeout(Duration::ZERO);
        min.minimize()
            .expect("minimize must act on the (initially visible) window");
        let focus = Locator::new(provider_dyn, None, "window").with_timeout(Duration::ZERO);
        let err = focus
            .perform_action("focus")
            .expect_err("the generic path must still require visibility");
        assert!(matches!(err, Error::Timeout { .. }), "got {err:?}");
    }

    #[test]
    fn locator_resize_to_rejects_zero_before_auto_wait() {
        // Locator validates payloads before entering its 5s auto-wait poll;
        // the zero-size rejection must produce InvalidActionData immediately,
        // not a Timeout after polling a never-matching selector.
        let provider = build_provider();
        let provider_dyn: Arc<dyn Provider> = provider;
        let locator = Locator::new(provider_dyn, None, r#"window[name="never-matches"]"#);
        let started = std::time::Instant::now();
        let err = locator.resize_to(0, 100).unwrap_err();
        assert!(matches!(err, Error::InvalidActionData { .. }));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "validation must short-circuit auto-wait",
        );
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

        fn focused_app(&self) -> Result<crate::element::ElementData> {
            // Handle freshness is irrelevant to focus resolution in this test
            // double; defer to the inner provider's notion of the foreground
            // app unchanged.
            self.inner.focused_app()
        }

        fn list_shell_surfaces(
            &self,
        ) -> Result<Vec<(crate::shell::ShellSurfaceKind, crate::element::ElementData)>> {
            self.inner.list_shell_surfaces()
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
        fn activate(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.activate(e)
        }
        fn minimize(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.minimize(e)
        }
        fn maximize(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.maximize(e)
        }
        fn enter_fullscreen(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.enter_fullscreen(e)
        }
        fn restore(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.restore(e)
        }
        fn close(&self, e: &crate::element::ElementData) -> Result<()> {
            self.inner.close(e)
        }
        fn move_to(&self, e: &crate::element::ElementData, x: i32, y: i32) -> Result<()> {
            self.inner.move_to(e, x, y)
        }
        fn resize_to(&self, e: &crate::element::ElementData, w: u32, h: u32) -> Result<()> {
            self.inner.resize_to(e, w, h)
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

    // ── Error diagnosis (tenet 6) ────────────────────────────────────
    //
    // Failure-path errors must carry what the operation was waiting for,
    // what it last observed, and bounded scope context — without the caller
    // wrapping the call in manual tree dumps.

    /// Short wait that exceeds at least one 100 ms poll tick.
    /// Long enough for a scroll and the poll that observes it on a loaded
    /// runner: no poll runs past the deadline (#531), and one poll of the
    /// mock tree has been seen to take ~100 ms on macOS CI.
    const SHORT_WAIT: Duration = Duration::from_secs(1);

    #[test]
    fn wait_visible_timeout_diagnosis_never_matched() {
        let loc = root_locator(r#"button[name="DoesNotExist"]"#);
        let err = loc
            .wait_visible(SHORT_WAIT)
            .expect_err("missing selector must time out");
        let d = err.diagnosis().expect("timeout must carry a diagnosis");
        assert_eq!(d.condition.as_deref(), Some("visible"));
        assert_eq!(
            d.selector.as_deref(),
            Some(r#"button[name="DoesNotExist"]"#)
        );
        assert_eq!(d.last_observed.as_deref(), Some("selector never matched"));
        // Same-role candidates: the mock tree's two buttons.
        assert_eq!(
            d.candidates,
            vec![r#"button "Back""#, r#"button "Forward""#]
        );
        // Scope for a rootless locator is the application list.
        assert!(
            d.scope.as_deref().unwrap_or_default().contains("TestApp"),
            "scope should list running applications, got: {:?}",
            d.scope
        );
        // And everything must render into the message — the consumer's
        // first contact with the diagnosis is the exception text.
        let msg = format!("{err}");
        assert!(msg.contains("waiting for: visible"), "{msg}");
        assert!(msg.contains("selector never matched"), "{msg}");
        assert!(msg.contains(r#"button "Back""#), "{msg}");
    }

    #[test]
    fn wait_visible_timeout_diagnosis_matched_but_invisible() {
        // static_text "Status" exists but visible=false: the diagnosis must
        // distinguish "matched in the wrong state" from "never matched".
        let loc = root_locator(r#"static_text[name="Status"]"#);
        let err = loc
            .wait_visible(SHORT_WAIT)
            .expect_err("invisible element must time out");
        let d = err.diagnosis().expect("timeout must carry a diagnosis");
        let last = d.last_observed.as_deref().unwrap_or_default();
        assert!(
            last.contains(r#"static_text "Status""#) && last.contains("visible=false"),
            "last observed should describe the matched element's state, got: {last}"
        );
        // And say why it is hidden, structurally and in the message (#590).
        assert_eq!(d.hidden_reason, Some(crate::HiddenReason::NotShowing));
        let msg = format!("{err}");
        assert!(msg.contains("hidden because: not_showing"), "{msg}");
        // A matched element needs no candidate/scope sweep.
        assert!(d.candidates.is_empty(), "{:?}", d.candidates);
        assert!(d.scope.is_none(), "{:?}", d.scope);
    }

    #[test]
    fn action_timeout_diagnosis_names_action_and_blocking_state() {
        // button "Forward" is disabled: press() must time out with a
        // diagnosis naming the attempted action and enabled=false.
        let loc = root_locator(r#"button[name="Forward"]"#).with_timeout(SHORT_WAIT);
        let err = loc.press().expect_err("disabled button must time out");
        let d = err.diagnosis().expect("timeout must carry a diagnosis");
        assert_eq!(
            d.condition.as_deref(),
            Some("press target actionable (visible && enabled)")
        );
        let last = d.last_observed.as_deref().unwrap_or_default();
        assert!(
            last.contains("enabled=false"),
            "last observed should expose the blocking state, got: {last}"
        );
    }

    #[test]
    fn action_timeout_and_miss_report_not_delivered() {
        // The auto-wait gives up before dispatching anything, so a retry is
        // safe. Without the override a Timeout would read as Unknown.
        let blocked = root_locator(r#"button[name="Forward"]"#).with_timeout(SHORT_WAIT);
        let err = blocked.press().expect_err("disabled button must time out");
        assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
        assert_eq!(err.delivery(), crate::Delivery::NotDelivered);

        let missing = root_locator(r#"button[name="Nope"]"#).with_timeout(SHORT_WAIT);
        let err = missing.press().expect_err("missing button must fail");
        assert_eq!(err.delivery(), crate::Delivery::NotDelivered, "{err:?}");
    }

    #[test]
    fn element_miss_diagnosis_includes_candidates_and_scope_dump() {
        // Scoped miss: the scope snapshot is a depth-limited dump of the
        // search root, so the caller doesn't need print(app.dump()).
        let provider = build_provider();
        let provider_dyn: Arc<dyn Provider> = provider;
        let app_root = provider_dyn.list_apps().expect("list_apps")[0].clone();
        let loc = Locator::new(provider_dyn, Some(app_root), r#"button[name="Exprot"]"#);
        let err = loc.element().expect_err("miss must fail fast");
        assert!(matches!(err, Error::SelectorNotMatched { .. }), "{err:?}");
        let d = err
            .diagnosis()
            .expect("terminal miss must carry a diagnosis");
        assert_eq!(
            d.candidates,
            vec![r#"button "Back""#, r#"button "Forward""#]
        );
        let scope = d.scope.as_deref().expect("miss must include scope");
        assert!(
            scope.contains("window") && scope.contains("Main Window"),
            "scope should be a tree dump of the search root, got: {scope}"
        );
    }

    #[test]
    fn nth_miss_diagnosis_reports_match_count() {
        let loc = root_locator("button").nth(5);
        let err = loc.element().expect_err("nth(5) of 2 buttons must miss");
        let d = err.diagnosis().expect("nth miss must carry a diagnosis");
        assert_eq!(
            d.last_observed.as_deref(),
            Some("selector matched 2 element(s); nth(5) requested")
        );
    }

    #[test]
    fn exists_stays_cheap_and_unenriched() {
        // exists() consumes the miss as a boolean — it must not pay the
        // diagnosis cost, and must keep returning Ok(false).
        let loc = root_locator(r#"button[name="DoesNotExist"]"#);
        assert!(!loc.exists().unwrap());
    }

    #[test]
    fn truncate_lines_caps_and_counts() {
        let text = (0..10)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let capped = truncate_lines(&text, 3);
        assert!(capped.starts_with("line0\nline1\nline2\n"), "{capped}");
        assert!(capped.ends_with("… (+7 more lines truncated)"), "{capped}");
        // Under the cap: unchanged, no marker.
        assert_eq!(truncate_lines("a\nb", 3), "a\nb");
    }

    // ── auto_wait_unique ───────────────────────────────────────────

    /// A provider whose tree changes after the first resolution: from the
    /// second `list_apps` on, a second, unrelated "Back" button sits ahead of
    /// the real one in the toolbar. A caller that counts in one resolution
    /// and acts in another acts on the intruder.
    struct ShiftingProvider {
        inner: Arc<crate::mock::MockProvider>,
        resolutions: std::sync::atomic::AtomicUsize,
    }

    const INTRUDER_HANDLE: u64 = 9_999;

    macro_rules! forward {
        ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
            $(fn $name(&self, e: &ElementData $(, $arg: $ty)*) -> Result<$ret> {
                self.inner.$name(e $(, $arg)*)
            })*
        };
    }

    impl Provider for ShiftingProvider {
        fn list_apps(&self) -> Result<Vec<ElementData>> {
            self.resolutions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.list_apps()
        }
        fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
            let mut children = self.inner.get_children(element)?;
            let shifted = self.resolutions.load(std::sync::atomic::Ordering::SeqCst) > 1;
            if shifted && element.is_some_and(|e| e.name.as_deref() == Some("Navigation")) {
                let mut intruder = children[0].clone();
                intruder.handle = INTRUDER_HANDLE;
                children.insert(0, intruder);
            }
            Ok(children)
        }
        fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
            self.inner.get_parent(element)
        }
        fn focused_app(&self) -> Result<ElementData> {
            self.inner.focused_app()
        }
        fn list_shell_surfaces(
            &self,
        ) -> Result<Vec<(crate::shell::ShellSurfaceKind, ElementData)>> {
            self.inner.list_shell_surfaces()
        }
        forward! {
            press() -> (); focus() -> (); blur() -> (); toggle() -> (); select() -> ();
            expand() -> (); collapse() -> (); show_menu() -> (); increment() -> ();
            decrement() -> (); scroll_into_view() -> (); set_value(v: &str) -> ();
            set_numeric_value(v: f64) -> (); type_text(t: &str) -> ();
            set_text_selection(s: u32, f: u32) -> (); perform_action(a: &str) -> ();
            activate() -> (); minimize() -> (); maximize() -> (); enter_fullscreen() -> ();
            restore() -> (); close() -> (); move_to(x: i32, y: i32) -> ();
            resize_to(w: u32, h: u32) -> ();
            subscribe() -> crate::event_provider::Subscription;
        }
    }

    #[test]
    fn auto_wait_unique_counts_and_returns_from_one_resolution() {
        let provider = Arc::new(ShiftingProvider {
            inner: build_provider(),
            resolutions: std::sync::atomic::AtomicUsize::new(0),
        });
        let loc = Locator::new(provider.clone(), None, r#"button[name="Back"]"#)
            .with_timeout(Duration::ZERO);
        let element = loc
            .auto_wait_unique("press")
            .expect("the wait succeeds")
            .expect("one Back button when it was counted");
        assert_ne!(
            element.handle, INTRUDER_HANDLE,
            "acted on an uncounted element"
        );
        assert_eq!(
            provider
                .resolutions
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the count and the element to act on must come from one resolution"
        );
        // The hazard is real: the tree now holds two, and a re-resolve sees both.
        assert_eq!(loc.count().expect("count"), 2);
    }

    #[test]
    fn auto_wait_unique_refuses_several_matches_without_waiting() {
        let loc = root_locator("button").with_timeout(Duration::from_secs(30));
        let start = std::time::Instant::now();
        let all = loc
            .auto_wait_unique("press")
            .expect("not an error")
            .expect_err("two buttons match");
        assert_eq!(names(&all), vec!["Back", "Forward"]);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "must not wait it out"
        );
    }

    #[test]
    fn auto_wait_unique_applies_the_named_actions_gate() {
        // "Status" is invisible: `press` waits on visibility and times out,
        // `scroll_into_view` does not wait on it and gets the element.
        let loc = root_locator(r#"static_text[name="Status"]"#).with_timeout(Duration::ZERO);
        let err = loc.auto_wait_unique("press").expect_err("invisible");
        let Error::Timeout { .. } = &err else {
            panic!("expected a timeout, got {err:?}");
        };
        let d = err.diagnosis().expect("timeouts carry a diagnosis");
        assert_eq!(
            d.condition.as_deref(),
            Some("press target actionable (visible && enabled)")
        );
        assert!(
            d.last_observed
                .as_deref()
                .is_some_and(|o| o.contains("visible=false")),
            "{d:?}"
        );
        assert_eq!(d.hidden_reason, Some(crate::HiddenReason::NotShowing));
        loc.auto_wait_unique("scroll_into_view")
            .expect("enabled is enough")
            .expect("one match");
    }

    #[test]
    fn auto_wait_unique_times_out_with_a_diagnosis_when_nothing_matches() {
        let loc = root_locator(r#"button[name="Nope"]"#).with_timeout(Duration::ZERO);
        let err = loc.auto_wait_unique("press").expect_err("no match");
        assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
        let d = err.diagnosis().expect("diagnosis");
        assert_eq!(d.last_observed.as_deref(), Some("selector never matched"));
        assert!(!d.candidates.is_empty(), "near misses are listed: {d:?}");
    }

    #[test]
    fn an_unknown_action_name_is_refused_rather_than_given_a_default_gate() {
        let loc = root_locator(r#"button[name="Back"]"#).with_timeout(Duration::ZERO);
        let err = loc.auto_wait_unique("prees").expect_err("misspelled");
        assert!(matches!(err, Error::InvalidActionData { .. }), "{err:?}");
    }

    #[test]
    fn auto_wait_unique_honours_an_explicit_nth() {
        let el = root_locator("button")
            .nth(2)
            .with_timeout(Duration::ZERO)
            .auto_wait_unique("focus")
            .map(|r| r.map(|e| e.name.clone()));
        // `Forward` is disabled, so the second button never becomes actionable.
        assert!(matches!(el, Err(Error::Timeout { .. })), "{el:?}");
        let el = root_locator("button")
            .nth(1)
            .with_timeout(Duration::ZERO)
            .auto_wait_unique("press")
            .expect("the first button is actionable")
            .expect("nth picks one");
        assert_eq!(el.name.as_deref(), Some("Back"));
    }

    // ── Auto-scroll (issue #590) ────────────────────────────────────

    /// What [`ScrollingProvider`]'s `scroll_into_view` does.
    #[derive(Clone, Copy, PartialEq)]
    enum ScrollMode {
        /// Accepts, and the target is in view from the next read on.
        Reveals,
        /// Refuses with `ActionNotSupported`, as a toolkit without a
        /// scroll-into-view call does.
        Refuses,
        /// Accepts and changes nothing.
        NoEffect,
        /// Reports a stale handle once, then behaves as `Reveals`.
        StaleOnce,
        /// Accepts, and the target is no longer scrolled out but still
        /// hidden for another reason (`not_showing`).
        Uncovers,
        /// Accepts and brings the target into view, where it reads disabled.
        RevealsDisabled,
        /// Fails with a platform error.
        Fails,
    }

    /// The mock tree with `list_item "Item 2"` scrolled out of its list
    /// (or, with `reason`, hidden for some other reason) until a
    /// `scroll_into_view` call reveals it.
    struct ScrollingProvider {
        inner: Arc<crate::mock::MockProvider>,
        mode: ScrollMode,
        reason: HiddenReason,
        disabled: bool,
        calls: std::sync::atomic::AtomicU32,
        revealed: std::sync::atomic::AtomicBool,
    }

    impl ScrollingProvider {
        fn locator(mode: ScrollMode, reason: HiddenReason) -> (Arc<Self>, Locator) {
            Self::locator_with(mode, reason, false)
        }

        /// As [`Self::locator`], with the target also disabled when
        /// `disabled` is set.
        fn locator_with(
            mode: ScrollMode,
            reason: HiddenReason,
            disabled: bool,
        ) -> (Arc<Self>, Locator) {
            let provider = Arc::new(Self {
                inner: build_provider(),
                mode,
                reason,
                disabled,
                calls: std::sync::atomic::AtomicU32::new(0),
                revealed: std::sync::atomic::AtomicBool::new(false),
            });
            let dyn_provider: Arc<dyn Provider> = provider.clone();
            let loc = Locator::new(dyn_provider, None, r#"list_item[name="Item 2"]"#)
                .with_timeout(SHORT_WAIT);
            (provider, loc)
        }

        fn calls(&self) -> u32 {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Provider for ScrollingProvider {
        fn list_apps(&self) -> Result<Vec<ElementData>> {
            self.inner.list_apps()
        }
        fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
            let mut children = self.inner.get_children(element)?;
            let revealed = self.revealed.load(std::sync::atomic::Ordering::SeqCst);
            let reason = match (revealed, self.mode) {
                (false, _) => Some(self.reason),
                (true, ScrollMode::Uncovers) => Some(HiddenReason::NotShowing),
                (true, _) => None,
            };
            for child in &mut children {
                if child.name.as_deref() != Some("Item 2") {
                    continue;
                }
                if let Some(reason) = reason {
                    child.states.visible = false;
                    child.states.hidden_reason = Some(reason);
                }
                if self.disabled || (revealed && self.mode == ScrollMode::RevealsDisabled) {
                    child.states.enabled = false;
                }
            }
            Ok(children)
        }
        fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
            self.inner.get_parent(element)
        }
        fn focused_app(&self) -> Result<ElementData> {
            self.inner.focused_app()
        }
        fn list_shell_surfaces(
            &self,
        ) -> Result<Vec<(crate::shell::ShellSurfaceKind, ElementData)>> {
            self.inner.list_shell_surfaces()
        }
        fn scroll_into_view(&self, element: &ElementData) -> Result<()> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let reveal = || {
                self.revealed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            };
            match self.mode {
                ScrollMode::Reveals | ScrollMode::Uncovers | ScrollMode::RevealsDisabled => {
                    reveal()
                }
                ScrollMode::Refuses => Err(Error::ActionNotSupported {
                    action: "scroll_into_view".into(),
                    role: element.role,
                }),
                ScrollMode::NoEffect => Ok(()),
                ScrollMode::StaleOnce if call == 0 => Err(Error::stale(
                    crate::error::StaleReason::Removed,
                    "list_item",
                )),
                ScrollMode::StaleOnce => reveal(),
                ScrollMode::Fails => Err(Error::Platform {
                    code: -1,
                    message: "ScrollTo failed: org.freedesktop.DBus.Error.Failed".into(),
                }),
            }
        }
        forward! {
            press() -> (); focus() -> (); blur() -> (); toggle() -> (); select() -> ();
            expand() -> (); collapse() -> (); show_menu() -> (); increment() -> ();
            decrement() -> (); set_value(v: &str) -> ();
            set_numeric_value(v: f64) -> (); type_text(t: &str) -> ();
            set_text_selection(s: u32, f: u32) -> (); perform_action(a: &str) -> ();
            activate() -> (); minimize() -> (); maximize() -> (); enter_fullscreen() -> ();
            restore() -> (); close() -> (); move_to(x: i32, y: i32) -> ();
            resize_to(w: u32, h: u32) -> ();
            subscribe() -> crate::event_provider::Subscription;
        }
    }

    fn acted(provider: &ScrollingProvider, action: &str) -> bool {
        provider
            .inner
            .actions()
            .iter()
            .any(|(_, name, _)| name == action)
    }

    #[test]
    fn an_action_on_a_scrolled_out_target_scrolls_it_into_view_first() {
        let (provider, loc) =
            ScrollingProvider::locator(ScrollMode::Reveals, HiddenReason::ScrolledOut);
        loc.select()
            .expect("the wait scrolls the target into view, then selects it");
        assert_eq!(provider.calls(), 1);
        assert!(acted(&provider, "select"));
    }

    #[test]
    fn a_stale_handle_during_auto_scroll_is_retried_on_the_next_poll() {
        let (provider, loc) =
            ScrollingProvider::locator(ScrollMode::StaleOnce, HiddenReason::ScrolledOut);
        loc.press().expect("the second poll's fresh handle scrolls");
        assert_eq!(provider.calls(), 2);
        assert!(acted(&provider, "press"));
    }

    #[test]
    fn a_refused_auto_scroll_keeps_waiting_and_says_why_in_the_diagnosis() {
        let (provider, loc) =
            ScrollingProvider::locator(ScrollMode::Refuses, HiddenReason::ScrolledOut);
        let err = loc.press().expect_err("nothing can bring it into view");
        assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
        // A refusal is an answer: it is asked once per wait, not every poll.
        assert_eq!(provider.calls(), 1);
        assert!(!acted(&provider, "press"));
        let d = err.diagnosis().expect("timeout must carry a diagnosis");
        assert_eq!(d.hidden_reason, Some(HiddenReason::ScrolledOut));
        assert_eq!(d.auto_scroll, Some(AutoScrollOutcome::Refused));
        let last = d.last_observed.as_deref().unwrap_or_default();
        assert!(
            last.contains("scroll_into_view was refused")
                && last.contains("Action scroll_into_view not supported"),
            "{last}"
        );
        assert!(
            format!("{err}").contains("scroll_into_view was refused"),
            "{err}"
        );
    }

    #[test]
    fn an_auto_scroll_that_changes_nothing_is_bounded_and_reported() {
        let (provider, loc) =
            ScrollingProvider::locator(ScrollMode::NoEffect, HiddenReason::ScrolledOut);
        let err = loc
            .with_timeout(Duration::from_millis(600))
            .press()
            .expect_err("it never comes into view");
        assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
        assert_eq!(provider.calls(), AUTO_SCROLL_ATTEMPTS);
        let d = err.diagnosis().expect("timeout must carry a diagnosis");
        assert_eq!(d.auto_scroll, Some(AutoScrollOutcome::NoEffect));
        let last = d.last_observed.clone().unwrap_or_default();
        assert!(
            last.contains("accepted 3 time(s) but the element stayed scrolled out"),
            "{last}"
        );
    }

    #[test]
    fn an_auto_scroll_platform_failure_is_returned_not_waited_out() {
        let (provider, loc) =
            ScrollingProvider::locator(ScrollMode::Fails, HiddenReason::ScrolledOut);
        let err = loc.press().expect_err("the failure propagates");
        assert!(matches!(err, Error::Platform { .. }), "{err:?}");
        assert_eq!(provider.calls(), 1);
    }

    #[test]
    fn only_a_scrolled_out_target_is_auto_scrolled() {
        for reason in [
            HiddenReason::NotShowing,
            HiddenReason::WindowMinimized,
            HiddenReason::Unknown,
        ] {
            let (provider, loc) = ScrollingProvider::locator(ScrollMode::Reveals, reason);
            let err = loc
                .press()
                .expect_err("only a scroll could have revealed it");
            assert!(matches!(err, Error::Timeout { .. }), "{reason}: {err:?}");
            assert_eq!(provider.calls(), 0, "{reason}");
            let last = err
                .diagnosis()
                .and_then(|d| d.last_observed.clone())
                .unwrap_or_default();
            assert!(!last.contains("auto-scroll"), "{reason}: {last}");
            assert_eq!(
                err.diagnosis().and_then(|d| d.auto_scroll),
                None,
                "{reason}"
            );
        }
    }

    #[test]
    fn verbs_that_do_not_need_visibility_never_auto_scroll() {
        // `scroll_into_view` itself passes its enabled-only gate at once, so
        // the one call it makes is its own, not the wait's.
        let (provider, loc) =
            ScrollingProvider::locator(ScrollMode::Reveals, HiddenReason::ScrolledOut);
        loc.scroll_into_view().expect("enabled is enough");
        assert_eq!(provider.calls(), 1);
    }

    #[test]
    fn a_plain_wait_never_scrolls() {
        let (provider, loc) =
            ScrollingProvider::locator(ScrollMode::Reveals, HiddenReason::ScrolledOut);
        let err = loc
            .wait_visible(SHORT_WAIT)
            .expect_err("a wait only watches");
        assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
        assert_eq!(provider.calls(), 0);
    }

    #[test]
    fn a_scroll_on_the_last_poll_is_observed_before_the_wait_gives_up() {
        // With no time left, the poll that scrolls is also the last one the
        // deadline allows. The wait re-reads once more, sees the target in
        // view, and acts: it does not time out on the pre-scroll reading.
        let (provider, loc) =
            ScrollingProvider::locator(ScrollMode::Reveals, HiddenReason::ScrolledOut);
        loc.with_timeout(Duration::ZERO)
            .select()
            .expect("the post-scroll poll sees the target in view");
        assert_eq!(provider.calls(), 1);
        assert!(acted(&provider, "select"));
    }

    #[test]
    fn a_no_effect_claim_needs_a_poll_that_saw_it() {
        let (provider, loc) =
            ScrollingProvider::locator(ScrollMode::NoEffect, HiddenReason::ScrolledOut);
        let err = loc
            .with_timeout(Duration::ZERO)
            .press()
            .expect_err("it never comes into view");
        // Each accepted call earns one more poll, which observes the target
        // still hidden and may try again, up to the attempt bound.
        assert_eq!(provider.calls(), AUTO_SCROLL_ATTEMPTS);
        let d = err.diagnosis().expect("timeout must carry a diagnosis");
        assert_eq!(d.auto_scroll, Some(AutoScrollOutcome::NoEffect));
        let last = d.last_observed.as_deref().unwrap_or_default();
        assert!(
            last.contains("accepted 3 time(s) but the element stayed scrolled out"),
            "{last}"
        );
    }

    #[test]
    fn a_scroll_that_worked_is_reported_when_something_else_blocks_the_action() {
        let (provider, loc) =
            ScrollingProvider::locator(ScrollMode::RevealsDisabled, HiddenReason::ScrolledOut);
        let err = loc.press().expect_err("in view, but disabled");
        assert_eq!(provider.calls(), 1);
        let d = err.diagnosis().expect("timeout must carry a diagnosis");
        assert_eq!(d.auto_scroll, Some(AutoScrollOutcome::Revealed));
        assert_eq!(d.hidden_reason, None);
        assert!(format!("{err}").contains("auto-scroll: revealed"), "{err}");
    }

    #[test]
    fn a_scroll_that_left_the_target_hidden_otherwise_is_not_called_revealed() {
        // No longer scrolled out, but not visible either: the scroll did not
        // bring it into view, whatever the reason now is.
        let (provider, loc) =
            ScrollingProvider::locator(ScrollMode::Uncovers, HiddenReason::ScrolledOut);
        let err = loc.press().expect_err("still hidden after the scroll");
        assert_eq!(provider.calls(), 1);
        let d = err.diagnosis().expect("timeout must carry a diagnosis");
        assert_eq!(d.auto_scroll, Some(AutoScrollOutcome::NoEffect));
        assert_eq!(d.hidden_reason, Some(HiddenReason::NotShowing));
        let last = d.last_observed.as_deref().unwrap_or_default();
        assert!(
            last.contains("no longer scrolled out but still not visible (not_showing)"),
            "{last}"
        );
    }

    #[test]
    fn a_disabled_scrolled_out_target_is_not_scrolled() {
        // A disabled control is not actionable wherever it is, so the wait
        // leaves the UI where it is.
        let (provider, loc) =
            ScrollingProvider::locator_with(ScrollMode::Reveals, HiddenReason::ScrolledOut, true);
        let err = loc.press().expect_err("never enabled");
        assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
        assert_eq!(provider.calls(), 0);
        assert_eq!(err.diagnosis().and_then(|d| d.auto_scroll), None);
    }
}

#[cfg(test)]
mod wait_stable_tests {
    //! [`Locator::wait_stable`] against the mock provider, whose
    //! [`crate::mock::MockProvider::animate`] scripts what each poll sees.

    use super::*;
    use crate::mock::{build_provider, MockFrame, MockProvider};
    use std::time::Instant;

    const BACK: &str = r#"button[name="Back"]"#;

    fn rect(x: i32) -> Rect {
        Rect {
            x,
            y: 60,
            width: 50,
            height: 30,
        }
    }

    fn animated(frames: Vec<MockFrame>) -> (Arc<MockProvider>, Locator) {
        let provider = build_provider();
        if !frames.is_empty() {
            provider.animate("Back", frames);
        }
        let dyn_provider: Arc<dyn Provider> = provider.clone();
        (provider, Locator::new(dyn_provider, None, BACK))
    }

    fn observed(err: &Error) -> String {
        assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
        let d = err
            .diagnosis()
            .expect("a stability timeout carries a diagnosis");
        assert_eq!(d.condition.as_deref(), Some(STABLE_CONDITION));
        d.last_observed.clone().expect("last observation recorded")
    }

    #[test]
    fn a_cut_second_read_does_not_count_unobserved_time_as_unchanged() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct CutSecondRead {
            inner: Arc<MockProvider>,
            reads: AtomicUsize,
        }
        macro_rules! delegate {
            ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
                $(fn $name(&self, e: &ElementData $(, $arg: $ty)*) -> Result<$ret> {
                    self.inner.$name(e $(, $arg)*)
                })*
            };
        }
        impl Provider for CutSecondRead {
            fn focused_app(&self) -> Result<ElementData> {
                self.inner.focused_app()
            }
            fn list_shell_surfaces(
                &self,
            ) -> Result<Vec<(crate::shell::ShellSurfaceKind, ElementData)>> {
                self.inner.list_shell_surfaces()
            }

            fn list_apps(&self) -> Result<Vec<ElementData>> {
                self.inner.list_apps()
            }
            fn get_children(&self, e: Option<&ElementData>) -> Result<Vec<ElementData>> {
                self.inner.get_children(e)
            }
            fn get_parent(&self, e: &ElementData) -> Result<Option<ElementData>> {
                self.inner.get_parent(e)
            }
            fn find_elements_group(
                &self,
                root: &ElementData,
                group: &SelectorGroup,
                limit: Option<usize>,
                depth: Option<u32>,
            ) -> Result<Vec<ElementData>> {
                if self.reads.fetch_add(1, Ordering::SeqCst) > 0 {
                    let deadline = crate::Deadline::current().expect("wait opens a deadline");
                    std::thread::sleep(deadline.remaining());
                    return Err(deadline.expired(Some("scripted second read".into())));
                }
                self.inner.find_elements_group(root, group, limit, depth)
            }
            delegate! {
                press() -> (); focus() -> (); blur() -> (); toggle() -> (); select() -> ();
                expand() -> (); collapse() -> (); show_menu() -> (); increment() -> ();
                decrement() -> (); scroll_into_view() -> (); set_value(v: &str) -> ();
                set_numeric_value(v: f64) -> (); type_text(t: &str) -> ();
                set_text_selection(s: u32, f: u32) -> (); perform_action(a: &str) -> ();
                activate() -> (); minimize() -> (); maximize() -> (); enter_fullscreen() -> ();
                restore() -> (); close() -> (); move_to(x: i32, y: i32) -> ();
                resize_to(w: u32, h: u32) -> ();
                subscribe() -> crate::event_provider::Subscription;
            }
        }
        let inner = build_provider();
        let root = inner
            .list_apps()
            .expect("mock apps")
            .into_iter()
            .next()
            .expect("mock app exists");
        let provider = Arc::new(CutSecondRead {
            inner,
            reads: AtomicUsize::new(0),
        });
        let loc = Locator::new_for_app(provider, root, BACK);
        let err = loc
            .wait_stable(Duration::from_secs(3))
            .expect_err("second read is cut");
        assert!(
            observed(&err).contains("observed unchanged for 0 ms"),
            "{err}"
        );
        assert_eq!(
            err.diagnosis()
                .expect("diagnosed timeout")
                .pending
                .as_deref(),
            Some("scripted second read")
        );
    }

    #[test]
    fn a_still_element_needs_the_observation_window() {
        let (_p, loc) = animated(vec![]);
        let start = Instant::now();
        let el = loc
            .wait_stable(Duration::from_secs(2))
            .expect("unmoving element");
        assert_eq!(el.bounds, Some(rect(110)));
        assert!(
            start.elapsed() >= STABLE_OBSERVATION_WINDOW,
            "stability needs the full observation window"
        );
    }

    #[test]
    fn returns_the_settled_bounds_once_the_motion_stops() {
        let (_p, loc) = animated(vec![
            (Some(rect(0)), true),
            (Some(rect(40)), true),
            (Some(rect(80)), true),
        ]);
        let el = loc.wait_stable(Duration::from_secs(2)).expect("settles");
        assert_eq!(el.bounds, Some(rect(80)));
    }

    #[test]
    fn a_hidden_poll_resets_the_comparison() {
        // The observation window starts again after the hidden poll.
        let (_p, loc) = animated(vec![
            (Some(rect(5)), true),
            (Some(rect(5)), false),
            (Some(rect(5)), true),
        ]);
        let start = Instant::now();
        loc.wait_stable(Duration::from_secs(2)).expect("settles");
        assert!(start.elapsed() >= STABLE_OBSERVATION_WINDOW + WAIT_POLL_INTERVAL * 2);
    }

    #[test]
    fn timeout_names_the_last_two_rectangles_while_still_moving() {
        let frames = (0..200).map(|i| (Some(rect(i * 10)), true)).collect();
        let (_p, loc) = animated(frames);
        let err = loc
            .wait_stable(Duration::from_secs(1))
            .expect_err("never settles");
        let obs = observed(&err);
        assert!(obs.starts_with("bounds still changing: {x="), "{obs}");
        assert!(obs.contains('\u{2192}'), "{obs}");
        assert!(obs.contains(r#"on button "Back""#), "{obs}");
        assert!(obs.contains("750 ms required"), "{obs}");
    }

    #[test]
    fn timeout_says_not_visible() {
        let (_p, loc) = animated(vec![(Some(rect(5)), false)]);
        let err = loc
            .wait_stable(Duration::from_millis(250))
            .expect_err("hidden element is never stable");
        let obs = observed(&err);
        assert!(obs.contains("matched but not visible"), "{obs}");
    }

    #[test]
    fn timeout_says_no_bounds() {
        let (_p, loc) = animated(vec![(None, true)]);
        let err = loc
            .wait_stable(Duration::from_millis(250))
            .expect_err("an element without bounds is never stable");
        assert!(observed(&err).contains("reports no bounds"));
    }

    #[test]
    fn timeout_says_never_matched() {
        let loc = Locator::new(build_provider(), None, r#"button[name="Nope"]"#);
        let err = loc
            .wait_stable(Duration::from_millis(150))
            .expect_err("no such element");
        assert_eq!(observed(&err), "selector never matched");
    }

    #[test]
    fn a_zero_timeout_explains_the_unobserved_stability_window() {
        let (_p, loc) = animated(vec![]);
        let err = loc.wait_stable(Duration::ZERO).expect_err("one poll only");
        assert!(observed(&err).contains("observed unchanged for 0 ms"));
        assert!(observed(&err).contains("at least 750 ms"));
    }

    #[test]
    fn a_timeout_after_a_hidden_poll_reports_the_restarted_window() {
        // Driven on the window itself, not through wall-clock polls: no poll
        // runs past the deadline (#531), so how many polls fit a short wait
        // depends on the runner's load. The diagnosis wording for a 0 ms
        // window is covered by `a_zero_timeout_explains_...`.
        let mut stability = StableBounds::default();
        let at = Duration::from_millis;
        assert!(!stability.observe(Some(rect(110)), at(0)));
        // Hidden: earlier visible time cannot count as unchanged bounds.
        assert!(!stability.observe(None, at(400)));
        assert!(
            !stability.observe(Some(rect(110)), at(900)),
            "900 ms since the first sighting, but the hidden poll restarted the window"
        );
        assert_eq!(stability.unchanged_for(at(900)), Some(Duration::ZERO));
        assert!(stability.observe(Some(rect(110)), at(1650)));
    }

    #[test]
    fn browser_publication_plateaus_do_not_finish_a_moving_wait() {
        // Chromium publishes nonfocused location changes at 500 ms intervals.
        // Several equal fresh queries in each interval must not return early.
        // Three polls per plateau, not five: a plateau's wall time is its poll
        // count times the 100 ms interval *as the host schedules it*, and
        // five polls left only 250 ms of slack under the 750 ms window, which
        // a loaded CI runner (macos-latest) used up, ending the wait on the
        // plateau. Three still repeat the same fresh bounds.
        let mut frames = vec![(Some(rect(0)), true); 3];
        frames.extend(vec![(Some(rect(40)), true); 3]);
        frames.push((Some(rect(80)), true));
        let (_p, loc) = animated(frames);
        let el = loc.wait_stable(Duration::from_secs(5)).expect("settles");
        assert_eq!(el.bounds, Some(rect(80)), "never return a cached plateau");
    }

    #[test]
    fn the_window_is_measured_from_the_first_completed_observation() {
        let mut stability = StableBounds::default();
        let sample = Some(rect(5));
        assert!(!stability.observe(sample, Duration::from_secs(3)));
        assert!(!stability.observe(sample, Duration::from_millis(3749)));
        assert!(stability.observe(sample, Duration::from_millis(3750)));
    }

    #[test]
    fn timeout_on_a_still_match_explains_the_short_observation_window() {
        let (_p, loc) = animated(vec![]);
        let err = loc
            .wait_stable(Duration::from_millis(200))
            .expect_err("too short");
        let observation = observed(&err);
        assert!(
            observation.contains("observed unchanged for"),
            "{observation}"
        );
        assert!(observation.contains("at least 750 ms"), "{observation}");
        assert!(
            !observation.contains("bounds still changing"),
            "{observation}"
        );
    }

    #[test]
    fn plateau_timeout_keeps_the_last_change_and_current_window() {
        // 600 ms: room for the second poll, and still short of the 750 ms
        // the settled rect(40) would need to count as stable.
        let (_p, loc) = animated(vec![(Some(rect(0)), true), (Some(rect(40)), true)]);
        let err = loc
            .wait_stable(Duration::from_millis(600))
            .expect_err("too short");
        let observation = observed(&err);
        assert!(
            observation.starts_with("bounds still changing: {x=0"),
            "{observation}"
        );
        assert!(
            observation.contains("latest bounds observed unchanged for"),
            "{observation}"
        );
        assert!(observation.contains("750 ms required"), "{observation}");
    }

    #[test]
    fn changes_and_invalid_candidates_reset_the_whole_window() {
        for invalid in [
            None,
            stable_candidate(&ElementData {
                states: crate::element::StateSet {
                    visible: false,
                    ..Default::default()
                },
                bounds: Some(rect(5)),
                ..Default::default()
            }),
            stable_candidate(&ElementData {
                states: crate::element::StateSet {
                    visible: true,
                    ..Default::default()
                },
                bounds: None,
                ..Default::default()
            }),
        ] {
            let mut stability = StableBounds::default();
            assert!(!stability.observe(Some(rect(5)), Duration::ZERO));
            assert!(!stability.observe(invalid, Duration::from_millis(700)));
            assert!(!stability.observe(Some(rect(5)), Duration::from_millis(800)));
            assert!(!stability.observe(Some(rect(5)), Duration::from_millis(1549)));
            assert!(stability.observe(Some(rect(5)), Duration::from_millis(1550)));
        }
        let mut stability = StableBounds::default();
        assert!(!stability.observe(Some(rect(5)), Duration::ZERO));
        assert!(!stability.observe(Some(rect(9)), Duration::from_millis(700)));
        assert!(!stability.observe(Some(rect(9)), Duration::from_millis(1449)));
        assert!(stability.observe(Some(rect(9)), Duration::from_millis(1450)));
    }
}
