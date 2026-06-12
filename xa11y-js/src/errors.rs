//! Map xa11y::Error into napi::Error with a stable `code` string.
//!
//! The JS wrapper (in `index.js`) re-throws these as typed `XA11yError`
//! subclasses so users can `instanceof` them.
//!
//! Diagnosis-carrying errors (`SelectorNotMatched`, `Timeout`) additionally
//! append a JSON payload after a U+001F unit separator; the wrapper parses
//! it and assigns the structured fields (`selector`, `condition`,
//! `lastObserved`, `candidates`, `scope`, `elapsedMs`) onto the typed error
//! (tenet 6: rich context is structured, not just message prose).

use napi::{Error, Status};

/// Error code tags that the JS wrapper uses to promote a plain `Error`
/// into a typed `XA11yError` subclass.
pub mod codes {
    pub const PERMISSION_DENIED: &str = "XA11Y_PERMISSION_DENIED";
    pub const ACCESSIBILITY_NOT_ENABLED: &str = "XA11Y_ACCESSIBILITY_NOT_ENABLED";
    pub const SELECTOR_NOT_MATCHED: &str = "XA11Y_SELECTOR_NOT_MATCHED";
    pub const ELEMENT_STALE: &str = "XA11Y_ELEMENT_STALE";
    pub const ACTION_NOT_SUPPORTED: &str = "XA11Y_ACTION_NOT_SUPPORTED";
    pub const TEXT_VALUE_NOT_SUPPORTED: &str = "XA11Y_TEXT_VALUE_NOT_SUPPORTED";
    pub const TIMEOUT: &str = "XA11Y_TIMEOUT";
    pub const INVALID_SELECTOR: &str = "XA11Y_INVALID_SELECTOR";
    pub const INVALID_ACTION_DATA: &str = "XA11Y_INVALID_ACTION_DATA";
    pub const INVALID_CONFIG: &str = "XA11Y_INVALID_CONFIG";
    pub const PLATFORM: &str = "XA11Y_PLATFORM";
    pub const NO_ELEMENT_BOUNDS: &str = "XA11Y_NO_ELEMENT_BOUNDS";
    pub const UNSUPPORTED: &str = "XA11Y_UNSUPPORTED";
}

/// Unit separator between the human-readable message and the JSON diagnosis
/// payload inside the napi error reason. U+001F can't occur in rendered
/// messages (selector strings and tree dumps never contain control
/// characters), so a plain `indexOf` split on the JS side is unambiguous.
pub const DIAGNOSIS_SEP: char = '\u{1f}';

/// Serialize the structured diagnosis for the JS wrapper. `selector` is the
/// error's primary selector field (SelectorNotMatched) or the diagnosis
/// selector (Timeout); `elapsed_ms` is set for timeouts only.
fn diagnosis_json(
    selector: Option<&str>,
    elapsed_ms: Option<f64>,
    diagnosis: Option<&xa11y::Diagnosis>,
) -> serde_json::Value {
    serde_json::json!({
        "selector": selector,
        "elapsedMs": elapsed_ms,
        "condition": diagnosis.and_then(|d| d.condition.as_deref()),
        "lastObserved": diagnosis.and_then(|d| d.last_observed.as_deref()),
        "candidates": diagnosis.map(|d| d.candidates.clone()).unwrap_or_default(),
        "scope": diagnosis.and_then(|d| d.scope.as_deref()),
    })
}

/// Convert an `xa11y::Error` into a `napi::Error`. The `reason` field doubles
/// as both the human-readable message (prefixed by the code tag) and a machine
/// code that the JS wrapper parses out.
pub fn map_err(e: xa11y::Error) -> Error {
    let (code, msg) = match &e {
        xa11y::Error::PermissionDenied { instructions } => {
            (codes::PERMISSION_DENIED, instructions.clone())
        }
        xa11y::Error::AccessibilityNotEnabled { app, instructions } => (
            codes::ACCESSIBILITY_NOT_ENABLED,
            format!("Accessibility not enabled for {app}: {instructions}"),
        ),
        // Core Display renders the diagnosis suffix into the message; the
        // structured payload below carries the same fields for programmatic
        // access.
        xa11y::Error::SelectorNotMatched { .. } => (codes::SELECTOR_NOT_MATCHED, e.to_string()),
        xa11y::Error::ElementStale { selector } => {
            (codes::ELEMENT_STALE, format!("Element stale: {selector}"))
        }
        xa11y::Error::ActionNotSupported { action, role } => (
            codes::ACTION_NOT_SUPPORTED,
            format!("{action} not supported on {role}"),
        ),
        xa11y::Error::TextValueNotSupported => (
            codes::TEXT_VALUE_NOT_SUPPORTED,
            "Text value not supported for this element".to_string(),
        ),
        xa11y::Error::Timeout { .. } => (codes::TIMEOUT, e.to_string()),
        xa11y::Error::InvalidSelector { selector, message } => (
            codes::INVALID_SELECTOR,
            format!("Invalid selector '{selector}': {message}"),
        ),
        xa11y::Error::InvalidActionData { message } => (
            codes::INVALID_ACTION_DATA,
            format!("Invalid action data: {message}"),
        ),
        xa11y::Error::InvalidConfig { message } => (
            codes::INVALID_CONFIG,
            format!("Invalid configuration: {message}"),
        ),
        xa11y::Error::Platform { code, message } => (
            codes::PLATFORM,
            format!("Platform error ({code}): {message}"),
        ),
        xa11y::Error::NoElementBounds => (
            codes::NO_ELEMENT_BOUNDS,
            "Element has no bounds; cannot compute a screen point".to_string(),
        ),
        xa11y::Error::Unsupported { feature } => {
            (codes::UNSUPPORTED, format!("Unsupported: {feature}"))
        }
    };

    let payload = match &e {
        xa11y::Error::SelectorNotMatched { selector, .. } => {
            Some(diagnosis_json(Some(selector), None, e.diagnosis()))
        }
        xa11y::Error::ElementStale { selector } => Some(diagnosis_json(Some(selector), None, None)),
        xa11y::Error::Timeout { elapsed, .. } => {
            let d = e.diagnosis();
            Some(diagnosis_json(
                d.and_then(|d| d.selector.as_deref()),
                Some(elapsed.as_secs_f64() * 1000.0),
                d,
            ))
        }
        _ => None,
    };

    // Encode the tag at the start of the message so the JS wrapper can split
    // it off: "XA11Y_PERMISSION_DENIED: <details>". The diagnosis payload,
    // when present, follows a U+001F separator.
    let reason = match payload {
        Some(p) => format!("{code}: {msg}{DIAGNOSIS_SEP}{p}"),
        None => format!("{code}: {msg}"),
    };
    Error::new(Status::GenericFailure, reason)
}
