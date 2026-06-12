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

use crate::role::Role;

/// Result type alias for xa11y operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Structured diagnostic context attached to failure-path errors.
///
/// See the [module docs](self) for the pattern governing when and how to
/// attach one. All fields are optional; renderers skip empty fields.
///
/// Always construct with functional-update syntax —
/// `Diagnosis { condition: ..., ..Diagnosis::default() }` — so adding a new
/// diagnostic field later doesn't break existing construction sites.
/// (`#[non_exhaustive]` would enforce that, but it also forbids struct
/// expressions from the platform crates and bindings that build these.)
#[derive(Debug, Clone, Default, PartialEq)]
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
    /// Bounded list of near-miss candidates — elements or applications that
    /// were present in the search scope and partially matched (e.g. same
    /// role, different name).
    pub candidates: Vec<String>,
    /// Depth- and line-bounded rendering of the search scope (an indented
    /// tree dump or an application list), so the consumer does not need to
    /// re-run the failure under `print(app.dump())`.
    pub scope: Option<String>,
}

impl Diagnosis {
    /// True when no field carries any information.
    pub fn is_empty(&self) -> bool {
        self.condition.is_none()
            && self.selector.is_none()
            && self.last_observed.is_none()
            && self.candidates.is_empty()
            && self.scope.is_none()
    }
}

impl std::fmt::Display for Diagnosis {
    /// Renders as `; `-separated clauses appended to an error's primary
    /// message, with the (potentially multi-line) scope on its own lines:
    ///
    /// ```text
    /// ; waiting for: visible; last observed: selector never matched;
    /// candidates: button "Back", button "Forward"
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
        if !self.candidates.is_empty() {
            write!(f, "; candidates: {}", self.candidates.join(", "))?;
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

/// Structured error type for xa11y operations.
/// Designed to be informative across FFI boundaries.
///
/// Construct [`SelectorNotMatched`](Error::SelectorNotMatched) and
/// [`Timeout`](Error::Timeout) through [`Error::selector_not_matched`] /
/// [`Error::timeout`] and attach context with [`Error::diagnose`] — see the
/// [module docs](self) for the diagnosis pattern.
#[derive(Debug, thiserror::Error)]
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

    /// The node's platform handle is stale and re-traversal could not relocate it.
    #[error("Element stale: could not relocate element for selector: {selector}")]
    ElementStale { selector: String },

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
    #[error("Invalid action data: {message}")]
    InvalidActionData { message: String },

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
    #[error("Platform error ({code}): {message}")]
    Platform { code: i64, message: String },
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

    /// Attach (or replace) a [`Diagnosis`] on errors that carry one.
    ///
    /// Supported variants: [`SelectorNotMatched`](Self::SelectorNotMatched)
    /// and [`Timeout`](Self::Timeout). Other variants are returned unchanged —
    /// their primary fields already identify the failure, and silently
    /// growing a diagnosis field on them would not be rendered. An empty
    /// diagnosis is normalized to `None`.
    #[must_use]
    pub fn diagnose(mut self, diagnosis: Diagnosis) -> Self {
        let normalized = if diagnosis.is_empty() {
            None
        } else {
            Some(Box::new(diagnosis))
        };
        match &mut self {
            Self::SelectorNotMatched { diagnosis: d, .. } | Self::Timeout { diagnosis: d, .. } => {
                *d = normalized;
            }
            _ => {}
        }
        self
    }

    /// The attached [`Diagnosis`], if this error carries one. Bindings use
    /// this to expose structured fields on their exception types.
    pub fn diagnosis(&self) -> Option<&Diagnosis> {
        match self {
            Self::SelectorNotMatched { diagnosis, .. } | Self::Timeout { diagnosis, .. } => {
                diagnosis.as_deref()
            }
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
}
