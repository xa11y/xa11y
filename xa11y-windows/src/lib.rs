//! Windows accessibility backend using UI Automation (UIA).
//!
//! This backend implements the `Provider` trait using the Windows UI Automation API.
//! No special permissions are required for local UIA queries.

use xa11y_core::{
    Action, ActionData, AppInfo, AppTarget, Error, NodeId, PermissionStatus, Provider,
    QueryOptions, Result, Tree,
};

/// Windows accessibility provider using UI Automation.
#[derive(Default)]
pub struct WindowsProvider {
    // Internal state will be added during implementation
}

impl WindowsProvider {
    /// Create a new Windows accessibility provider.
    pub fn new() -> Result<Self> {
        Ok(Self {})
    }
}

impl Provider for WindowsProvider {
    fn get_app_tree(&self, _target: &AppTarget, _opts: &QueryOptions) -> Result<Tree> {
        Err(Error::Platform {
            code: -1,
            message: "Windows backend not yet implemented".to_string(),
        })
    }

    fn get_all_apps(&self, _opts: &QueryOptions) -> Result<Tree> {
        Err(Error::Platform {
            code: -1,
            message: "Windows backend not yet implemented".to_string(),
        })
    }

    fn perform_action(
        &self,
        _tree: &Tree,
        _node_id: NodeId,
        _action: Action,
        _data: Option<ActionData>,
    ) -> Result<()> {
        Err(Error::Platform {
            code: -1,
            message: "Windows backend not yet implemented".to_string(),
        })
    }

    fn check_permissions(&self) -> Result<PermissionStatus> {
        // Windows UIA doesn't require special permissions
        Ok(PermissionStatus::Granted)
    }

    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        Err(Error::Platform {
            code: -1,
            message: "Windows backend not yet implemented".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_provider() {
        let provider = WindowsProvider::new().unwrap();
        let status = provider.check_permissions().unwrap();
        assert!(matches!(status, PermissionStatus::Granted));
    }
}
