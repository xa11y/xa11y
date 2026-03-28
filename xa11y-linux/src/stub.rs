//! Stub backend for non-Linux platforms (allows compilation on all targets).

use xa11y_core::{
    Action, ActionData, AppTarget, Error, NodeData, PermissionStatus, Provider, QueryOptions,
    Result, Tree,
};

#[derive(Default)]
pub struct LinuxProvider;

impl LinuxProvider {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

impl Provider for LinuxProvider {
    fn get_app_tree(&self, _target: &AppTarget, _opts: &QueryOptions) -> Result<Tree> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }

    fn get_apps(&self, _opts: &QueryOptions) -> Result<Tree> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }

    fn perform_action(
        &self,
        _tree: &Tree,
        _node: &NodeData,
        _action: Action,
        _data: Option<ActionData>,
    ) -> Result<()> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }

    fn check_permissions(&self) -> Result<PermissionStatus> {
        Ok(PermissionStatus::Denied {
            instructions: "Linux AT-SPI2 backend not available on this platform".to_string(),
        })
    }
}
