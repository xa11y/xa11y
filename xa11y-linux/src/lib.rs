//! Linux accessibility backend using AT-SPI2 over D-Bus.
//!
//! This backend implements the `Provider` trait using the AT-SPI2 accessibility API.
//! Requires `at-spi2-core` package and toolkit accessibility to be enabled.

use xa11y_core::{
    Action, ActionData, AppInfo, AppTarget, Error, NodeId, PermissionStatus, Provider,
    QueryOptions, Result, Tree,
};

/// Linux accessibility provider using AT-SPI2.
#[derive(Default)]
pub struct LinuxProvider {
    // Internal state will be added during implementation
}

impl LinuxProvider {
    /// Create a new Linux accessibility provider.
    pub fn new() -> Result<Self> {
        Ok(Self {})
    }
}

impl Provider for LinuxProvider {
    fn get_app_tree(&self, _target: &AppTarget, _opts: &QueryOptions) -> Result<Tree> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not yet implemented".to_string(),
        })
    }

    fn get_all_apps(&self, _opts: &QueryOptions) -> Result<Tree> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not yet implemented".to_string(),
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
            message: "Linux backend not yet implemented".to_string(),
        })
    }

    fn check_permissions(&self) -> Result<PermissionStatus> {
        // Stub: check AT-SPI availability
        Ok(PermissionStatus::Denied {
            instructions: "Enable accessibility: gsettings set org.gnome.desktop.interface toolkit-accessibility true".to_string(),
        })
    }

    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not yet implemented".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_provider() {
        let provider = LinuxProvider::new().unwrap();
        let status = provider.check_permissions().unwrap();
        assert!(matches!(status, PermissionStatus::Denied { .. }));
    }
}
