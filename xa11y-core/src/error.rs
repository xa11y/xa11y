use crate::action::Action;
use crate::node::NodeId;
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

    /// The target application was not found or is no longer running.
    #[error("Application not found: {target}")]
    AppNotFound { target: String },

    /// The node ID does not exist in the referenced snapshot.
    #[error("Node not found: {node_id}")]
    NodeNotFound { node_id: NodeId },

    /// The node's platform handle is stale and re-traversal could not relocate it.
    #[error("Element stale: node {node_id} could not be relocated")]
    ElementStale { node_id: NodeId },

    /// The requested action is not supported by this element.
    #[error("Action {action} not supported on {role}")]
    ActionNotSupported { action: Action, role: Role },

    /// Text value input is not supported for this element on this platform.
    #[error("Text value input not supported for this element")]
    TextValueNotSupported,

    /// A wait_for or wait_for_event call exceeded its timeout.
    #[error("Timeout after {elapsed:?}")]
    Timeout { elapsed: std::time::Duration },

    /// The selector string could not be parsed.
    #[error("Invalid selector '{selector}': {message}")]
    InvalidSelector { selector: String, message: String },

    /// A platform-specific error occurred.
    #[error("Platform error ({code}): {message}")]
    Platform { code: i64, message: String },
}
