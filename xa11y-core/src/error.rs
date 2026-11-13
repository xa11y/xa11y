//! Error types for xa11y operations.
//!
//! # The diagnosis pattern (tenet 6: errors carry their own diagnosis)
//!
//! Failure-path errors that a consumer might need to debug — timeouts and
//! "not found" lookups — carry an optional structured [`Diagnosis`] alongside
//! their identifying fields. The diagnosis answers the questions a consumer
//! would otherwise have to answer by wrapping the call in logging: *what was
//! the operation waiting for*, *what did it last observe*, and *what was
//! actually there*.
//!
//! Rules for attaching a diagnosis to a new failure path:
//!
//! 1. **Enrich at the terminal site, not at construction.** Errors like
//!    [`Error::SelectorNotMatched`] double as cheap retry signals inside poll
//!    loops (`Locator::auto_wait`, `App::by_pid_with`). Constructing them must
//!    stay allocation-light. Attach the diagnosis (via
//!    [`Error::diagnose`]) only where the error escapes to the caller —
//!    typically when a poll loop exhausts its timeout or a fail-fast query
//!    returns.
//! 2. **Bound the cost.** Diagnosis collection runs only on the failure path
//!    and must be size-bounded: tree snapshots are depth-limited and
//!    line-capped, candidate lists are truncated. See the `DIAG_*` constants
//!    in `locator.rs`.
//! 3. **Never mask the original error.** If collecting a diagnosis itself
//!    fails (e.g. the scope dump errors because the app quit), record the
//!    collection failure *inside* the diagnosis instead of dropping it or
//!    replacing the original error.
//! 4. **Keep it structured.** Language bindings expose the diagnosis as
//!    structured fields (exception attributes in Python, error properties in
//!    JS) in addition to rendering it into the message. New fields belong on
//!    [`Diagnosis`], not interpolated into ad-hoc message strings.

use crate::element::{HiddenReason, Rect};
use crate::input::{Anchor, Point};
use crate::role::Role;

/// Geometry checked before a pointer action was refused as off-display.
///
/// `point` and `element_bounds` use the public desktop coordinate space.
/// `checked_point` and `displays` use `coordinate_space`: on Linux these may
/// be physical pixels after the same scaling used to dispatch input.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct PointerTargetDiagnosis {
    /// Requested point in the public desktop coordinate space.
    pub point: Point,
    /// Original element bounds, absent for a literal point target.
    pub element_bounds: Option<Rect>,
    /// Anchor used for an element target; literal points have no anchor.
    pub anchor: Option<Anchor>,
    /// Point after the backend's dispatch-coordinate conversion.
    pub checked_point: Point,
    /// Units used by the checked point and display rectangles.
    pub coordinate_space: String,
    /// Bounded snapshot of display regions actually checked.
    pub displays: Vec<Rect>,
    /// Number of checked regions omitted from the diagnostic snapshot.
    pub omitted_displays: usize,
    /// Where the checked regions came from, when the backend records it:
    /// e.g. RandR monitors, the X11 root window on a server without RandR
    /// 1.5, the compositor's outputs, or the `XA11Y_SCREEN_WIDTH` /
    /// `XA11Y_SCREEN_HEIGHT` override. `None` when the backend has a single
    /// source (the OS display list on macOS and Windows).
    #[serde(default)]
    pub display_source: Option<String>,
}

impl PointerTargetDiagnosis {
    /// A bounded diagnostic snapshot. Validation must check the complete
    /// display set before constructing this failure-only payload.
    pub fn new(
        point: Point,
        checked_point: Point,
        coordinate_space: impl Into<String>,
        displays: &[Rect],
    ) -> Self {
        const LIMIT: usize = 32;
        Self {
            point,
            element_bounds: None,
            anchor: None,
            checked_point,
            coordinate_space: coordinate_space.into(),
            displays: displays.iter().take(LIMIT).copied().collect(),
            omitted_displays: displays.len().saturating_sub(LIMIT),
            display_source: None,
        }
    }
}

/// Whether an action that failed may already have taken effect.
///
/// A caller deciding whether to retry a failed action needs exactly this, and
/// no platform return code can be trusted for it: a timeout or a D-Bus
/// `NoReply` often arrives after the application applied the action, and a
/// retry then double-submits (a second "Send"). Read it with
/// [`Error::delivery`].
///
/// There are two answers, and the absence of a third is deliberate. A
/// confirmed delivery is not an error, so `Ok` already says it. What an
/// error can honestly claim is "nothing reached the application"
/// ([`NotDelivered`](Self::NotDelivered), safe to retry) or "I cannot tell"
/// ([`Unknown`](Self::Unknown), not safe to retry blindly). A failure that
/// does not clearly happen before dispatch is `Unknown`.
///
/// The disposition describes errors that come from an action. For an error
/// from a read or a wait it says only whether an action could have reached
/// the application.
///
/// Each backend documents which of its platform failures map to which value
/// in the crate-level docs of `xa11y-linux`, `xa11y-macos`, and
/// `xa11y-windows`.
///
/// `#[non_exhaustive]`: only core resolves a disposition and the bindings
/// pass it on as a string, so a further value needs no work in a backend.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// The failure happened before the action was dispatched to the
    /// application, so nothing was applied and a retry is safe. Examples:
    /// the selector matched nothing, the element does not support the
    /// action, the element went stale before dispatch, an argument was
    /// rejected, an auto-wait timed out before acting.
    NotDelivered,
    /// The action was dispatched, or may have been, and the outcome is
    /// unconfirmed. A retry may apply the action twice. Examples: a D-Bus
    /// `NoReply`, an AX `kAXErrorCannotComplete`, a UIA timeout, any
    /// platform error from the dispatch call itself.
    Unknown,
}

impl Delivery {
    /// The `snake_case` name this disposition crosses the bindings, the CLI
    /// and MCP as (`"not_delivered"`, `"unknown"`).
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Self::NotDelivered => "not_delivered",
            Self::Unknown => "unknown",
        }
    }

    /// True when retrying the failed action cannot apply it twice.
    pub fn is_safe_to_retry(self) -> bool {
        matches!(self, Self::NotDelivered)
    }
}

impl std::fmt::Display for Delivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_snake_case())
    }
}

/// Result type alias for xa11y operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Structured diagnostic context attached to failure-path errors.
///
/// See the [module docs](self) for the pattern governing when and how to
/// attach one. All fields are optional; renderers skip empty fields.
///
/// `#[non_exhaustive]`: diagnostic context is expected to grow, and every
/// growth would otherwise be a breaking change for the platform crates and
/// bindings that build these. Construct with [`Diagnosis::new`] and the
/// chained setters below:
///
/// ```
/// # use xa11y_core::Diagnosis;
/// Diagnosis::new()
///     .condition("visible")
///     .last_observed("matched button \"Export\" (visible=false)");
/// ```
///
/// The fields stay public — reading them is how the bindings project a
/// diagnosis into exception attributes, and a renderer that skips an unknown
/// field is not a compatibility hazard the way a broken constructor is.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Diagnosis {
    /// What the operation was waiting for or trying to find, e.g.
    /// `"visible"`, `"press target actionable (visible && enabled)"`,
    /// `"event matching predicate"`, `"application with pid 1234"`.
    pub condition: Option<String>,
    /// The selector being resolved, when the failing operation had one and
    /// it is not already part of the error's primary fields.
    pub selector: Option<String>,
    /// What was last observed before the failure, e.g.
    /// `"matched button \"Export\" (visible=false, enabled=true)"`,
    /// `"selector never matched"`, or
    /// `"3 event(s) received, none matched"`.
    pub last_observed: Option<String>,
    /// Why the last observed element was not visible, when a wait failed on
    /// an element that matched but read `visible: false`. A harness can act
    /// on it (`ScrolledOut` is what `scroll_into_view` fixes; a minimized
    /// window needs restoring) without parsing `last_observed`.
    pub hidden_reason: Option<HiddenReason>,
    /// What an auto-waiting action did about a target that was hidden only
    /// because it was scrolled out: whether the platform refused to scroll
    /// it, scrolled it without effect, or brought it into view (after which
    /// something else, such as `enabled=false`, kept the wait going).
    /// `None` when the wait never tried to scroll.
    pub auto_scroll: Option<AutoScrollOutcome>,
    /// Bounded list of near-miss candidates — elements or applications that
    /// were present in the search scope and partially matched (e.g. same
    /// role, different name).
    pub candidates: Vec<String>,
    /// Depth- and line-bounded rendering of the search scope (an indented
    /// tree dump or an application list), so the consumer does not need to
    /// re-run the failure under `print(app.dump())`.
    pub scope: Option<String>,
    /// Overrides the disposition [`Error::delivery`] would derive from the
    /// error's variant. Set where the variant alone is wrong: core's own
    /// wait timeouts happen before any dispatch (`NotDelivered`), while a
    /// bare [`Error::Timeout`] from a backend is `Unknown`.
    pub delivery: Option<Delivery>,
    /// Requested and checked geometry when no display contains a pointer
    /// target. Attached to [`Error::InvalidActionData`].
    pub pointer_target: Option<PointerTargetDiagnosis>,
    /// The platform call that was still in flight when the operation's
    /// deadline expired, and what it was addressed to, e.g.
    /// `"GetChildren on :1.42 /org/a11y/atspi/accessible/7 (pid 4242)"`.
    /// `None` when the deadline expired between calls, or when the failure
    /// was not a deadline expiry. See [`crate::deadline`].
    pub pending: Option<String>,
}

impl Diagnosis {
    /// An empty diagnosis. Fill it in with the chained setters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the geometry of a refused off-display pointer target.
    #[must_use]
    pub fn pointer_target(mut self, target: PointerTargetDiagnosis) -> Self {
        self.pointer_target = Some(target);
        self
    }

    /// Set what the operation was waiting for or trying to find.
    #[must_use]
    pub fn condition(mut self, condition: impl Into<String>) -> Self {
        self.condition = Some(condition.into());
        self
    }

    /// Set the selector being resolved when the operation failed.
    #[must_use]
    pub fn selector(mut self, selector: impl Into<String>) -> Self {
        self.selector = Some(selector.into());
        self
    }

    /// Set what was last observed before the failure.
    #[must_use]
    pub fn last_observed(mut self, last_observed: impl Into<String>) -> Self {
        self.last_observed = Some(last_observed.into());
        self
    }

    /// Set what an auto-waiting action's scroll attempt came to.
    #[must_use]
    pub fn auto_scroll(mut self, outcome: AutoScrollOutcome) -> Self {
        self.auto_scroll = Some(outcome);
        self
    }

    /// Set why the last observed element was not visible.
    #[must_use]
    pub fn hidden_reason(mut self, reason: HiddenReason) -> Self {
        self.hidden_reason = Some(reason);
        self
    }

    /// Set the bounded list of near-miss candidates.
    #[must_use]
    pub fn candidates(mut self, candidates: impl IntoIterator<Item = String>) -> Self {
        self.candidates = candidates.into_iter().collect();
        self
    }

    /// Set the bounded rendering of the search scope.
    #[must_use]
    pub fn scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    /// Set the platform call that was in flight when the deadline expired.
    #[must_use]
    pub fn pending(mut self, pending: impl Into<String>) -> Self {
        self.pending = Some(pending.into());
        self
    }

    /// Override the delivery disposition of the error this is attached to.
    #[must_use]
    pub fn delivery(mut self, delivery: Delivery) -> Self {
        self.delivery = Some(delivery);
        self
    }

    /// True when no field carries any information.
    pub fn is_empty(&self) -> bool {
        self.condition.is_none()
            && self.selector.is_none()
            && self.last_observed.is_none()
            && self.hidden_reason.is_none()
            && self.auto_scroll.is_none()
            && self.candidates.is_empty()
            && self.scope.is_none()
            && self.delivery.is_none()
            && self.pointer_target.is_none()
            && self.pending.is_none()
    }
}

impl std::fmt::Display for Diagnosis {
    /// Renders as `; `-separated clauses appended to an error's primary
    /// message, with the (potentially multi-line) scope on its own lines:
    ///
    /// ```text
    /// ; waiting for: visible; last observed: selector never matched;
    /// candidates: button "Back", button "Forward"
    /// ```
    ///
    /// or, for an element that matched but was not visible:
    ///
    /// ```text
    /// ; waiting for: visible; last observed: matched button "Save"
    /// (visible=false, enabled=true, focused=false); hidden because: scrolled_out
    /// ```
    ///
    /// The scope, when present, follows on its own lines:
    ///
    /// ```text
    /// search scope (bounded):
    ///   window "Main"
    ///     ...
    /// ```
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(condition) = &self.condition {
            write!(f, "; waiting for: {condition}")?;
        }
        if let Some(selector) = &self.selector {
            write!(f, "; selector: {selector}")?;
        }
        if let Some(last) = &self.last_observed {
            write!(f, "; last observed: {last}")?;
        }
        if let Some(reason) = self.hidden_reason {
            write!(f, "; hidden because: {reason}")?;
        }
        if let Some(outcome) = self.auto_scroll {
            write!(f, "; auto-scroll: {outcome}")?;
        }
        if let Some(pending) = &self.pending {
            write!(f, "; still pending: {pending}")?;
        }
        if !self.candidates.is_empty() {
            write!(f, "; candidates: {}", self.candidates.join(", "))?;
        }
        if let Some(target) = &self.pointer_target {
            write!(
                f,
                "; pointer target: {:?}, element bounds: {:?}, anchor: {:?}; \
                 checked point: {:?} ({}), displays: {:?}",
                target.point,
                target.element_bounds,
                target.anchor,
                target.checked_point,
                target.coordinate_space,
                target.displays,
            )?;
            if target.omitted_displays > 0 {
                write!(f, " ({} more displays omitted)", target.omitted_displays)?;
            }
            if let Some(source) = &target.display_source {
                write!(f, ", display source: {source}")?;
            }
        }
        if let Some(scope) = &self.scope {
            write!(f, "\nsearch scope (bounded):\n{scope}")?;
        }
        Ok(())
    }
}

/// Render an optional boxed diagnosis as a message suffix ("" when absent or
/// empty). Used by the `thiserror` display attributes below.
fn diagnosis_suffix(diagnosis: &Option<Box<Diagnosis>>) -> String {
    match diagnosis {
        Some(d) if !d.is_empty() => d.to_string(),
        _ => String::new(),
    }
}

/// How an auto-waiting action's attempt to scroll its target into view ended.
///
/// An action waits for its target to be visible. When the target is enabled
/// and hidden only because it is scrolled out, the wait calls the platform's
/// `scroll_into_view` on it, and a timeout reports the outcome here.
///
/// `#[non_exhaustive]`: nothing outside core matches on it. The bindings, the
/// CLI and MCP all render it through [`AutoScrollOutcome::to_snake_case`],
/// whose exhaustive match is the compiler's guard, and
/// `[[types.variant_coverage]]` guards the hand-written type declarations
/// (the Python `Literal` and the TypeScript union).
///
/// The outcome describes the first check after the latest accepted scroll.
/// `Revealed` alongside `hidden_reason: scrolled_out` therefore means the
/// element came into view and later scrolled back out.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutoScrollOutcome {
    /// The platform refused to scroll this element (it has no semantic
    /// scroll call for it). The wait asked once and kept polling.
    Refused,
    /// The platform accepted the call, but the element was still not
    /// visible when it was next observed: still `scrolled_out`, or hidden
    /// for another reason. `last_observed` says which.
    NoEffect,
    /// The next check after the scroll read the element visible; something
    /// else (such as `enabled=false`) kept the action from running.
    Revealed,
}

impl AutoScrollOutcome {
    /// The `snake_case` name this outcome crosses the bindings, the CLI and
    /// MCP as (`"refused"`, `"no_effect"`, `"revealed"`).
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Self::Refused => "refused",
            Self::NoEffect => "no_effect",
            Self::Revealed => "revealed",
        }
    }
}

impl std::fmt::Display for AutoScrollOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_snake_case())
    }
}

/// How a stale error names the element: `role "name"`, or the role alone,
/// so the message says what was captured without reading the platform.
///
/// Not public API: the shared spelling for the backend crates.
#[doc(hidden)]
pub fn stale_subject(element: &crate::element::ElementData) -> String {
    match &element.name {
        Some(name) => format!("{} {name:?}", element.role),
        None => element.role.to_string(),
    }
}

/// Why an element is stale ([`Error::ElementStale`]).
///
/// `#[non_exhaustive]`: providers map platform facts *to* a reason, never the
/// reverse, so a finer reason needs no work in a backend that cannot tell it
/// apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StaleReason {
    /// The node no longer exists in the application.
    Removed,
    /// The process that owned the node has exited.
    ProcessExited,
    /// The process id or name now belongs to a new process instance.
    ProcessRestarted,
    /// The platform id now names a different node: re-reading it gave a
    /// different role than the one captured.
    IdentityChanged,
    /// The provider's bounded handle table dropped the handle, so the node
    /// can no longer be reached (it may still exist).
    Evicted,
}

impl StaleReason {
    /// The `snake_case` name this reason crosses the bindings, the CLI and
    /// MCP as (`"removed"`, `"process_exited"`, ...).
    pub fn to_snake_case(self) -> &'static str {
        match self {
            StaleReason::Removed => "removed",
            StaleReason::ProcessExited => "process_exited",
            StaleReason::ProcessRestarted => "process_restarted",
            StaleReason::IdentityChanged => "identity_changed",
            StaleReason::Evicted => "evicted",
        }
    }
}

impl std::fmt::Display for StaleReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_snake_case())
    }
}

/// Structured error type for xa11y operations.
/// Designed to be informative across FFI boundaries.
///
/// Construct [`SelectorNotMatched`](Error::SelectorNotMatched),
/// [`Timeout`](Error::Timeout) and
/// [`InvalidActionData`](Error::InvalidActionData) through
/// [`Error::selector_not_matched`] / [`Error::timeout`] /
/// [`Error::invalid_action_data`] and attach context with [`Error::diagnose`] — see the
/// [module docs](self) for the diagnosis pattern.
///
/// `#[non_exhaustive]`: new failure modes arrive with every backend and
/// platform quirk, so downstream `match` arms need a `_` fallback. Before
/// this attribute the bindings' exhaustive matches were what forced a new
/// variant to be mapped; that guard now lives in
/// `cargo xtask check-bindings-parity`, which fails when a variant is not
/// referenced by both binding error mappers (see `[[types.variant_coverage]]`
/// in `bindings/parity_allowlist.toml`).
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Accessibility permissions not granted.
    #[error("Permission denied: {instructions}")]
    PermissionDenied { instructions: String },

    /// The target application advertises an accessibility tree but its
    /// content is empty because the app's accessibility bridge is disabled
    /// (Chromium/Electron on Linux without `--force-renderer-accessibility`,
    /// for example).
    #[error("Accessibility not enabled for {app}: {instructions}")]
    AccessibilityNotEnabled { app: String, instructions: String },

    /// No element matched the selector.
    ///
    /// Doubles as the "not yet" retry signal inside poll loops, so bare
    /// construction stays cheap; terminal sites attach a [`Diagnosis`].
    #[error("No element matched selector: {selector}{}", diagnosis_suffix(.diagnosis))]
    SelectorNotMatched {
        selector: String,
        diagnosis: Option<Box<Diagnosis>>,
    },

    /// The element no longer names a live platform node, so nothing was done
    /// to it.
    ///
    /// xa11y never re-matches a stale element by name or position: an action
    /// on a captured element reaches that node or fails here. `element`
    /// describes what was captured (role and name, or the handle when that is
    /// all the provider still has); `reason` says why it is stale. Re-resolve
    /// with a selector to act on whatever is there now.
    #[error("Element stale ({reason}): {element}")]
    ElementStale {
        /// Why the element is stale: removed from the tree, its process
        /// exited or restarted, its identity changed, or its handle was
        /// evicted.
        reason: StaleReason,
        /// What was captured, for the message: `role "name"`, the role
        /// alone, or the handle (`handle:42`) when that is all the provider
        /// still has. Descriptive only; never used to re-match.
        element: String,
    },

    /// The requested action is not supported by this element.
    #[error("Action {action} not supported on {role}")]
    ActionNotSupported { action: String, role: Role },

    /// Text value input is not supported for this element on this platform.
    #[error("Text value input not supported for this element")]
    TextValueNotSupported,

    /// A wait operation exceeded its timeout.
    ///
    /// Always carries what the wait was for via its [`Diagnosis`] — a bare
    /// "Timeout after Ns" tells the consumer nothing actionable (tenet 6).
    #[error("Timeout after {elapsed:.1?}{}", diagnosis_suffix(.diagnosis))]
    Timeout {
        elapsed: std::time::Duration,
        diagnosis: Option<Box<Diagnosis>>,
    },

    /// The selector string could not be parsed.
    #[error("Invalid selector '{selector}': {message}")]
    InvalidSelector { selector: String, message: String },

    /// Invalid argument to an action method.
    ///
    /// Construct through [`Error::invalid_action_data`]. The diagnosis is set
    /// where the argument is checked against something the caller cannot see
    /// in the argument alone: an off-display pointer target carries
    /// [`Diagnosis::pointer_target`], the geometry it was checked against.
    #[error("Invalid action data: {message}{}", diagnosis_suffix(.diagnosis))]
    InvalidActionData {
        message: String,
        diagnosis: Option<Box<Diagnosis>>,
    },

    /// A text range is out of bounds, reversed, or splits a surrogate pair.
    ///
    /// Offsets are UTF-16 code units. `length` is the text's current length
    /// when the check knew it, so a range made stale by an edit says how long
    /// the text is now.
    #[error("Invalid text range {start}..{end}{}: {message}", length.map(|l| format!(" (text length {l})")).unwrap_or_default())]
    InvalidTextRange {
        /// Start of the rejected range, in UTF-16 code units. 0-based.
        start: u32,
        /// End of the rejected range, exclusive, in UTF-16 code units.
        /// 0-based.
        end: u32,
        /// The text's length in UTF-16 code units when the check knew it,
        /// else `None`.
        length: Option<u32>,
        /// Which rule the range broke (past the end, reversed, splits a
        /// surrogate pair).
        message: String,
    },

    /// Process-wide configuration is invalid (e.g. an unparsable
    /// `XA11Y_DEFAULT_TIMEOUT` environment variable).
    #[error("Invalid configuration: {message}")]
    InvalidConfig { message: String },

    /// The element has no bounds (e.g. an off-screen or virtual node), so a
    /// screen point can't be computed for input simulation.
    #[error("Element has no bounds")]
    NoElementBounds,

    /// The requested operation has no implementation on this platform/session
    /// (e.g. pointer warp on Wayland without a portal grant).
    #[error("Unsupported: {feature}")]
    Unsupported { feature: String },

    /// A platform-specific error occurred.
    ///
    /// Construct through [`Error::platform`]: the variant is
    /// `#[non_exhaustive]`, so it can grow context without breaking every
    /// backend that reports one. Match it with `Error::Platform { code, .. }`.
    #[error("Platform error ({code}): {message}{}", diagnosis_suffix(.diagnosis))]
    #[non_exhaustive]
    Platform {
        /// The platform's own code (an `HRESULT`, an `AXError`, a D-Bus
        /// error number), or a negative value xa11y chose where the platform
        /// gave none.
        code: i64,
        /// What failed, in the platform's words where it gave any.
        message: String,
        /// Context attached where the error escapes, such as the
        /// [`Delivery`] of a failure known to precede dispatch.
        diagnosis: Option<Box<Diagnosis>>,
    },
}

impl Error {
    /// Construct a bare [`Error::SelectorNotMatched`] (no diagnosis).
    ///
    /// This is the cheap form for poll-loop retry signals; attach context at
    /// the terminal site with [`diagnose`](Self::diagnose).
    pub fn selector_not_matched(selector: impl Into<String>) -> Self {
        Self::SelectorNotMatched {
            selector: selector.into(),
            diagnosis: None,
        }
    }

    /// Construct a bare [`Error::Timeout`] (no diagnosis).
    ///
    /// Prefer attaching a [`Diagnosis`] with [`diagnose`](Self::diagnose)
    /// before the error reaches a consumer — see tenet 6.
    pub fn timeout(elapsed: std::time::Duration) -> Self {
        Self::Timeout {
            elapsed,
            diagnosis: None,
        }
    }

    /// Construct an [`Error::Platform`] (no diagnosis).
    pub fn platform(code: i64, message: impl Into<String>) -> Self {
        Self::Platform {
            code,
            message: message.into(),
            diagnosis: None,
        }
    }

    /// Construct a bare [`Error::InvalidActionData`] (no diagnosis).
    pub fn invalid_action_data(message: impl Into<String>) -> Self {
        Self::InvalidActionData {
            message: message.into(),
            diagnosis: None,
        }
    }

    /// The [`Error::InvalidActionData`] for a name that is not one of
    /// `valid`: it quotes the bad `input` and lists every valid name, so a
    /// typo is fixable from the message alone. `what` names the vocabulary
    /// (`"relation kind"`, `"text unit"`).
    ///
    /// The shared spelling for every `from_snake_case` caller that has to
    /// turn `None` into an error:
    ///
    /// ```
    /// use xa11y_core::{Error, TextUnit};
    /// let err = TextUnit::from_snake_case("sentence")
    ///     .ok_or_else(|| Error::unknown_name("text unit", "sentence",
    ///         TextUnit::ALL.map(TextUnit::to_snake_case)))
    ///     .unwrap_err();
    /// assert_eq!(
    ///     err.to_string(),
    ///     "Invalid action data: unknown text unit \"sentence\"; expected one of \
    ///      \"character\", \"word\", \"line\", \"paragraph\"",
    /// );
    /// ```
    pub fn unknown_name<'a>(
        what: &str,
        input: &str,
        valid: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let valid: Vec<String> = valid.into_iter().map(|n| format!("{n:?}")).collect();
        Self::invalid_action_data(format!(
            "unknown {what} {input:?}; expected one of {}",
            valid.join(", ")
        ))
    }

    /// An [`Error::ElementStale`] for `element` (a description such as
    /// `button "Save"` or `handle:42`).
    pub fn stale(reason: StaleReason, element: impl Into<String>) -> Self {
        Self::ElementStale {
            reason,
            element: element.into(),
        }
    }

    /// Attach (or replace) a [`Diagnosis`] on errors that carry one.
    ///
    /// Supported variants: [`SelectorNotMatched`](Self::SelectorNotMatched),
    /// [`Timeout`](Self::Timeout), [`InvalidActionData`](Self::InvalidActionData)
    /// and [`Platform`](Self::Platform). Other variants are returned
    /// unchanged: their primary fields already identify the failure, and
    /// every one of them is [`Delivery::NotDelivered`] by its variant, so
    /// there is no disposition to override either. An empty diagnosis is
    /// normalized to `None`.
    #[must_use]
    pub fn diagnose(mut self, diagnosis: Diagnosis) -> Self {
        let normalized = if diagnosis.is_empty() {
            None
        } else {
            Some(Box::new(diagnosis))
        };
        match &mut self {
            Self::SelectorNotMatched { diagnosis: d, .. }
            | Self::Timeout { diagnosis: d, .. }
            | Self::InvalidActionData { diagnosis: d, .. }
            | Self::Platform { diagnosis: d, .. } => {
                *d = normalized;
            }
            _ => {}
        }
        self
    }

    /// Whether the failed action may already have taken effect.
    ///
    /// Safe to call on any error. The value is the attached
    /// [`Diagnosis::delivery`] when one is set, otherwise derived from the
    /// variant:
    ///
    /// - [`NotDelivered`](Delivery::NotDelivered): the failure is decided
    ///   before dispatch. These are `SelectorNotMatched`, `ElementStale`,
    ///   `ActionNotSupported`, `TextValueNotSupported`, `InvalidSelector`,
    ///   `InvalidActionData`, `InvalidTextRange`, `InvalidConfig`, `NoElementBounds`,
    ///   `Unsupported`, `PermissionDenied` and `AccessibilityNotEnabled`.
    /// - [`Unknown`](Delivery::Unknown): `Timeout` and `Platform`, where the
    ///   platform call may have reached the application.
    ///
    /// The match has no wildcard arm, so a new variant does not compile until
    /// someone classifies it here.
    pub fn delivery(&self) -> Delivery {
        if let Some(delivery) = self.diagnosis().and_then(|d| d.delivery) {
            return delivery;
        }
        match self {
            Self::PermissionDenied { .. }
            | Self::AccessibilityNotEnabled { .. }
            | Self::SelectorNotMatched { .. }
            | Self::ElementStale { .. }
            | Self::ActionNotSupported { .. }
            | Self::TextValueNotSupported
            | Self::InvalidSelector { .. }
            | Self::InvalidActionData { .. }
            | Self::InvalidTextRange { .. }
            | Self::InvalidConfig { .. }
            | Self::NoElementBounds
            | Self::Unsupported { .. } => Delivery::NotDelivered,
            Self::Timeout { .. } | Self::Platform { .. } => Delivery::Unknown,
        }
    }

    /// The attached [`Diagnosis`], if this error carries one. Bindings use
    /// this to expose structured fields on their exception types.
    pub fn diagnosis(&self) -> Option<&Diagnosis> {
        match self {
            Self::SelectorNotMatched { diagnosis, .. }
            | Self::Timeout { diagnosis, .. }
            | Self::InvalidActionData { diagnosis, .. }
            | Self::Platform { diagnosis, .. } => diagnosis.as_deref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn bare_timeout_renders_duration_only() {
        let err = Error::timeout(Duration::from_secs(5));
        assert_eq!(format!("{err}"), "Timeout after 5.0s");
    }

    #[test]
    fn diagnosed_timeout_renders_condition_selector_and_last_observed() {
        let err = Error::timeout(Duration::from_secs(60)).diagnose(Diagnosis {
            condition: Some("visible".into()),
            selector: Some(r#"dialog[name^="Submit"]"#.into()),
            last_observed: Some("selector never matched".into()),
            ..Diagnosis::default()
        });
        let msg = format!("{err}");
        assert!(msg.contains("Timeout after 60.0s"), "{msg}");
        assert!(msg.contains("waiting for: visible"), "{msg}");
        assert!(msg.contains(r#"selector: dialog[name^="Submit"]"#), "{msg}");
        assert!(
            msg.contains("last observed: selector never matched"),
            "{msg}"
        );
    }

    #[test]
    fn diagnosed_not_matched_renders_candidates_and_scope() {
        let err = Error::selector_not_matched(r#"button[name="Exprot"]"#).diagnose(Diagnosis {
            candidates: vec![r#"button "Export""#.into(), r#"button "Cancel""#.into()],
            scope: Some("  window \"Main\"\n    button \"Export\"".into()),
            ..Diagnosis::default()
        });
        let msg = format!("{err}");
        assert!(msg.contains("No element matched selector"), "{msg}");
        assert!(
            msg.contains(r#"candidates: button "Export", button "Cancel""#),
            "{msg}"
        );
        assert!(msg.contains("search scope (bounded):"), "{msg}");
    }

    #[test]
    fn empty_diagnosis_is_normalized_away() {
        let err = Error::timeout(Duration::from_secs(1)).diagnose(Diagnosis::default());
        assert!(err.diagnosis().is_none());
        assert_eq!(format!("{err}"), "Timeout after 1.0s");
    }

    #[test]
    fn diagnose_on_unsupported_variant_is_a_documented_no_op() {
        let err = Error::NoElementBounds.diagnose(Diagnosis {
            condition: Some("anything".into()),
            ..Diagnosis::default()
        });
        assert!(err.diagnosis().is_none());
    }

    #[test]
    fn diagnosis_accessor_round_trips() {
        let err = Error::selector_not_matched("button").diagnose(Diagnosis {
            last_observed: Some("selector matched 1 element(s); nth(3) requested".into()),
            ..Diagnosis::default()
        });
        let d = err.diagnosis().expect("diagnosis must be attached");
        assert_eq!(
            d.last_observed.as_deref(),
            Some("selector matched 1 element(s); nth(3) requested")
        );
    }

    #[test]
    fn delivery_is_not_delivered_for_errors_decided_before_dispatch() {
        let cases = [
            Error::selector_not_matched("button"),
            Error::stale(StaleReason::Removed, "button"),
            Error::ActionNotSupported {
                action: "press".into(),
                role: Role::Button,
            },
            Error::TextValueNotSupported,
            Error::InvalidSelector {
                selector: "[".into(),
                message: "bad".into(),
            },
            Error::invalid_action_data("bad"),
            Error::NoElementBounds,
            Error::Unsupported {
                feature: "x".into(),
            },
            Error::PermissionDenied {
                instructions: "grant".into(),
            },
        ];
        for err in cases {
            assert_eq!(err.delivery(), Delivery::NotDelivered, "{err}");
            assert!(err.delivery().is_safe_to_retry(), "{err}");
        }
    }

    #[test]
    fn delivery_is_unknown_for_platform_failures_and_bare_timeouts() {
        let platform = Error::platform(-25204, "cannot complete");
        assert_eq!(platform.delivery(), Delivery::Unknown);
        assert!(!platform.delivery().is_safe_to_retry());
        assert_eq!(
            Error::timeout(Duration::from_secs(1)).delivery(),
            Delivery::Unknown
        );
    }

    #[test]
    fn diagnosis_delivery_overrides_the_variant_default() {
        let err = Error::timeout(Duration::from_secs(1))
            .diagnose(Diagnosis::new().delivery(Delivery::NotDelivered));
        assert_eq!(err.delivery(), Delivery::NotDelivered);
        // A diagnosis carrying only a disposition is not empty, and adds
        // nothing to the rendered message.
        assert!(err.diagnosis().is_some());
        assert_eq!(format!("{err}"), "Timeout after 1.0s");
    }

    #[test]
    fn delivery_names_are_stable_snake_case() {
        assert_eq!(Delivery::NotDelivered.to_snake_case(), "not_delivered");
        assert_eq!(Delivery::Unknown.to_string(), "unknown");
    }
}
