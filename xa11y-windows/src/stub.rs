//! Stub backend for non-Windows platforms (allows compilation on all targets).

use xa11y_core::{
    Action, ActionData, Error, NodeData, PermissionStatus, Provider, Result, Tree, WindowHandle,
};

#[derive(Default)]
pub struct WindowsProvider;

impl WindowsProvider {
    pub fn new() -> Result<Self> {
        Err(Error::Platform {
            code: -1,
            message: "Windows backend only available on Windows".to_string(),
        })
    }
}

impl Provider for WindowsProvider {
    fn get_tree_by_name(&self, _: &str) -> Result<Tree> {
        unreachable!()
    }
    fn get_tree_by_pid(&self, _: u32) -> Result<Tree> {
        unreachable!()
    }
    fn get_tree_by_window(&self, _: &WindowHandle) -> Result<Tree> {
        unreachable!()
    }
    fn get_apps(&self) -> Result<Tree> {
        unreachable!()
    }
    fn perform_action(
        &self,
        _: &Tree,
        _: &NodeData,
        _: Action,
        _: Option<ActionData>,
    ) -> Result<()> {
        unreachable!()
    }
    fn check_permissions(&self) -> Result<PermissionStatus> {
        unreachable!()
    }
}
