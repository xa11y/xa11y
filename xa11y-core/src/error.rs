use crate::role::Role;

/// Result type alias for xa11y operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Structured error type for xa11y operations.
/// Designed to be informative across FFI boundaries.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Accessibility permissions not granted.
    #[error("Permission denied: {instructions}")]
    PermissionDenied { instructions: String },

    /// No element matched the selector.
    #[error("No element matched selector: {selector}")]
    SelectorNotMatched { selector: String },

    /// The node's platform handle is stale and re-traversal could not relocate it.
    #[error("Element stale: could not relocate element")]
    ElementStale { selector: String },

    /// The requested action is not supported by this element.
    #[error("Action {action} not supported on {role}")]
    ActionNotSupported { action: String, role: Role },

    /// Text value input is not supported for this element on this platform.
    #[error("Text value input not supported for this element")]
    TextValueNotSupported,

    /// A wait_for or wait_for_event call exceeded its timeout.
    #[error("Timeout after {elapsed:?}")]
    Timeout { elapsed: std::time::Duration },

    /// The selector string could not be parsed.
    #[error("Invalid selector '{selector}': {message}")]
    InvalidSelector { selector: String, message: String },

    /// Invalid argument to an action method.
    #[error("Invalid action data: {message}")]
    InvalidActionData { message: String },

    /// A platform-specific error occurred.
    #[error("Platform error ({code}): {message}")]
    Platform { code: i64, message: String },
}
