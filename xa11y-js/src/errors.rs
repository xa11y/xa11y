//! Map xa11y::Error into napi::Error with a stable `code` string.
//!
//! The JS wrapper (in `index.js`) re-throws these as typed `XA11yError`
//! subclasses so users can `instanceof` them.

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
    pub const PLATFORM: &str = "XA11Y_PLATFORM";
    pub const NO_ELEMENT_BOUNDS: &str = "XA11Y_NO_ELEMENT_BOUNDS";
    pub const UNSUPPORTED: &str = "XA11Y_UNSUPPORTED";
}

/// Convert an `xa11y::Error` into a `napi::Error`. The `reason` field doubles
/// as both the human-readable message (prefixed by the code tag) and a machine
/// code that the JS wrapper parses out.
pub fn map_err(e: xa11y::Error) -> Error {
    let (code, msg) = match e {
        xa11y::Error::PermissionDenied { instructions } => (codes::PERMISSION_DENIED, instructions),
        xa11y::Error::AccessibilityNotEnabled { app, instructions } => (
            codes::ACCESSIBILITY_NOT_ENABLED,
            format!("Accessibility not enabled for {app}: {instructions}"),
        ),
        xa11y::Error::SelectorNotMatched { selector } => (
            codes::SELECTOR_NOT_MATCHED,
            format!("No element matched: {selector}"),
        ),
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
        xa11y::Error::Timeout { elapsed } => {
            (codes::TIMEOUT, format!("Timeout after {elapsed:.1?}"))
        }
        xa11y::Error::InvalidSelector { selector, message } => (
            codes::INVALID_SELECTOR,
            format!("Invalid selector '{selector}': {message}"),
        ),
        xa11y::Error::InvalidActionData { message } => (
            codes::INVALID_ACTION_DATA,
            format!("Invalid action data: {message}"),
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

    // Encode the tag at the start of the message so the JS wrapper can split
    // it off: "XA11Y_PERMISSION_DENIED: <details>".
    Error::new(Status::GenericFailure, format!("{code}: {msg}"))
}
