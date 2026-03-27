//! Stub backend for non-Windows platforms (allows compilation on all targets).

use xa11y_core::action::{Action, ActionData};
use xa11y_core::{
    AppInfo, AppTarget, Error, Node, PermissionStatus, Provider, QueryOptions, Result, Tree,
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
    fn get_app_tree(&self, _: &AppTarget, _: &QueryOptions) -> Result<Tree> {
        unreachable!()
    }
    fn get_all_apps(&self, _: &QueryOptions) -> Result<Tree> {
        unreachable!()
    }
    fn perform_action(&self, _: &Tree, _: &Node, _: Action, _: Option<ActionData>) -> Result<()> {
        unreachable!()
    }
    fn check_permissions(&self) -> Result<PermissionStatus> {
        unreachable!()
    }
    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        unreachable!()
    }
}
