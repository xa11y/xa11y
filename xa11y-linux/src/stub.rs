//! Stub backend for non-Linux platforms (allows compilation on all targets).

use xa11y_core::{
    Action, ActionData, Error, NodeData, PermissionStatus, Provider, Result, Tree, WindowHandle,
};

#[derive(Default)]
pub struct LinuxProvider;

impl LinuxProvider {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

impl Provider for LinuxProvider {
    fn get_tree_by_name(&self, _name: &str) -> Result<Tree> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }

    fn get_tree_by_pid(&self, _pid: u32) -> Result<Tree> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }

    fn get_tree_by_window(&self, _handle: &WindowHandle) -> Result<Tree> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }

    fn get_apps(&self) -> Result<Tree> {
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
