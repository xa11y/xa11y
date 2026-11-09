//! Map xa11y::Error into napi::Error with a stable `code` string.
//!
//! The JS wrapper (in `index.js`) re-throws these as typed `XA11yError`
//! subclasses so users can `instanceof` them.
//!
//! Every error appends (`delivery`, plus the diagnosis for `SelectorNotMatched`/`Timeout`,
//! and `start`/`end`/`length` for `InvalidTextRange`)
//! a JSON payload after a U+001F unit separator; the wrapper parses
//! it and assigns the structured fields (`selector`, `condition`,
//! `lastObserved`, `candidates`, `scope`, `elapsedMs`, `delivery`) onto the typed error
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
    pub const INVALID_TEXT_RANGE: &str = "XA11Y_INVALID_TEXT_RANGE";
    pub const INVALID_CONFIG: &str = "XA11Y_INVALID_CONFIG";
    pub const PLATFORM: &str = "XA11Y_PLATFORM";
    pub const NO_ELEMENT_BOUNDS: &str = "XA11Y_NO_ELEMENT_BOUNDS";
    pub const UNSUPPORTED: &str = "XA11Y_UNSUPPORTED";
    /// Fallback for an `xa11y::Error` variant this build of the binding does
    /// not know. `xa11y::Error` is `#[non_exhaustive]`, so a core built ahead
    /// of the bindings can produce one. `cargo xtask check-bindings-parity`
    /// fails when a variant is not named in `map_err`, so reaching this in a
    /// matched pair of versions is a bug, not a supported path.
    pub const UNKNOWN: &str = "XA11Y_UNKNOWN";
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
    delivery: xa11y::Delivery,
    selector: Option<&str>,
    elapsed_ms: Option<f64>,
    diagnosis: Option<&xa11y::Diagnosis>,
) -> serde_json::Value {
    serde_json::json!({
        "delivery": delivery.to_snake_case(),
        "selector": selector,
        "elapsedMs": elapsed_ms,
        "condition": diagnosis.and_then(|d| d.condition.as_deref()),
        "lastObserved": diagnosis.and_then(|d| d.last_observed.as_deref()),
        "hiddenReason": diagnosis.and_then(|d| d.hidden_reason.map(|r| r.to_snake_case())),
        "autoScroll": diagnosis.and_then(|d| d.auto_scroll.map(|o| o.to_snake_case())),
        "pending": diagnosis.and_then(|d| d.pending.as_deref()),
        "pointerTarget": diagnosis.and_then(|d| d.pointer_target.as_ref()).map(|target| {
            serde_json::json!({
                "point": target.point,
                "elementBounds": target.element_bounds,
                "anchor": target.anchor,
                "checkedPoint": target.checked_point,
                "coordinateSpace": target.coordinate_space,
                "displays": target.displays,
                "omittedDisplays": target.omitted_displays,
                "displaySource": target.display_source,
            })
        }),
        "candidates": diagnosis.map(|d| d.candidates.clone()).unwrap_or_default(),
        "scope": diagnosis.and_then(|d| d.scope.as_deref()),
    })
}

/// The JS spelling of a stale reason (`ElementStaleError.reason`), the same
/// snake_case string as Python's.
fn stale_reason_name(reason: xa11y::StaleReason) -> &'static str {
    match reason {
        xa11y::StaleReason::Removed => "removed",
        xa11y::StaleReason::ProcessExited => "process_exited",
        xa11y::StaleReason::ProcessRestarted => "process_restarted",
        xa11y::StaleReason::IdentityChanged => "identity_changed",
        xa11y::StaleReason::Evicted => "evicted",
        // `StaleReason` is `#[non_exhaustive]`; variant coverage in the
        // parity check fails the build before a new one reaches this arm.
        other => other.to_snake_case(),
    }
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
        xa11y::Error::ElementStale { .. } => (codes::ELEMENT_STALE, e.to_string()),
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
        // Core Display renders an off-display pointer target's geometry
        // into the message; the payload carries it as `pointerTarget`.
        xa11y::Error::InvalidActionData { .. } => (codes::INVALID_ACTION_DATA, e.to_string()),
        // Core Display renders the range and length into the message; the
        // payload below carries them as structured fields.
        xa11y::Error::InvalidTextRange { .. } => (codes::INVALID_TEXT_RANGE, e.to_string()),
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
        // See `codes::UNKNOWN`: variant coverage is enforced by
        // `cargo xtask check-bindings-parity`, not by the compiler.
        other => (codes::UNKNOWN, other.to_string()),
    };

    // Every error carries a payload, because every error carries a delivery
    // disposition: whether the failed action may already have taken effect.
    let delivery = e.delivery();
    let payload = match &e {
        xa11y::Error::SelectorNotMatched { selector, .. } => Some(diagnosis_json(
            delivery,
            Some(selector),
            None,
            e.diagnosis(),
        )),
        xa11y::Error::ElementStale { reason, element } => {
            let mut payload = diagnosis_json(delivery, None, None, None);
            payload["reason"] = serde_json::json!(stale_reason_name(*reason));
            payload["element"] = serde_json::json!(element);
            Some(payload)
        }
        xa11y::Error::Timeout { elapsed, .. } => {
            let d = e.diagnosis();
            Some(diagnosis_json(
                delivery,
                d.and_then(|d| d.selector.as_deref()),
                Some(elapsed.as_secs_f64() * 1000.0),
                d,
            ))
        }
        xa11y::Error::InvalidTextRange {
            start, end, length, ..
        } => {
            let mut p = diagnosis_json(delivery, None, None, None);
            if let Some(obj) = p.as_object_mut() {
                obj.insert("start".into(), (*start).into());
                obj.insert("end".into(), (*end).into());
                obj.insert("length".into(), (*length).into());
            }
            Some(p)
        }
        xa11y::Error::InvalidActionData { .. } => {
            Some(diagnosis_json(delivery, None, None, e.diagnosis()))
        }
        _ => Some(diagnosis_json(delivery, None, None, None)),
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
