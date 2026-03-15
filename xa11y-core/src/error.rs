use thiserror::Error;

/// Result type for xa11y operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Error types for xa11y operations.
#[derive(Debug, Error)]
pub enum Error {
    /// Accessibility permissions not granted
    #[error("accessibility permission denied: {0}")]
    PermissionDenied(String),

    /// Target application not found
    #[error("application not found: {0}")]
    AppNotFound(String),

    /// Node not found in tree
    #[error("node not found: {0}")]
    NodeNotFound(u32),

    /// Action not supported on this element
    #[error("action not supported: {0}")]
    ActionNotSupported(String),

    /// Invalid selector syntax
    #[error("invalid selector: {0}")]
    InvalidSelector(String),

    /// Operation timed out
    #[error("operation timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// Platform-specific error
    #[error("platform error: {0}")]
    Platform(String),

    /// Element became stale (UI changed since snapshot)
    #[error("element stale: node {0} no longer exists")]
    StaleElement(u32),

    /// Serialization/deserialization error
    #[error("serialization error: {0}")]
    Serialization(String),
}
