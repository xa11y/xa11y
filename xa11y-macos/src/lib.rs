//! macOS accessibility backend using AXUIElement.
//!
//! This backend implements the `Provider` trait using macOS Accessibility API.
//! It requires the "Accessibility" permission in System Preferences.

use xa11y_core::{
    Action, ActionData, AppInfo, AppTarget, Error, NodeId, PermissionStatus, Provider,
    QueryOptions, Result, Tree,
};

/// macOS accessibility provider using AXUIElement API.
#[derive(Default)]
pub struct MacOSProvider {
    // Internal state will be added during implementation
}

impl MacOSProvider {
    /// Create a new macOS accessibility provider.
    pub fn new() -> Result<Self> {
        Ok(Self {})
    }
}

impl Provider for MacOSProvider {
    fn get_app_tree(&self, _target: &AppTarget, _opts: &QueryOptions) -> Result<Tree> {
        Err(Error::Platform {
            code: -1,
            message: "macOS backend not yet implemented".to_string(),
        })
    }

    fn get_all_apps(&self, _opts: &QueryOptions) -> Result<Tree> {
        Err(Error::Platform {
            code: -1,
            message: "macOS backend not yet implemented".to_string(),
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
            message: "macOS backend not yet implemented".to_string(),
        })
    }

    fn check_permissions(&self) -> Result<PermissionStatus> {
        // Stub: always report denied until real implementation
        Ok(PermissionStatus::Denied {
            instructions:
                "Enable Accessibility in System Preferences → Privacy & Security → Accessibility"
                    .to_string(),
        })
    }

    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        Err(Error::Platform {
            code: -1,
            message: "macOS backend not yet implemented".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_provider() {
        let provider = MacOSProvider::new().unwrap();
        let status = provider.check_permissions().unwrap();
        assert!(matches!(status, PermissionStatus::Denied { .. }));
    }
}
