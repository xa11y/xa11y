//! Stub backend for non-Windows platforms (allows compilation on all targets).

use xa11y_core::{
    Action, ActionData, ElementData, Error, PermissionStatus, Provider, Result, Subscription,
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
    fn get_children(&self, _: Option<&ElementData>) -> Result<Vec<ElementData>> {
        unreachable!()
    }
    fn get_parent(&self, _: &ElementData) -> Result<Option<ElementData>> {
        unreachable!()
    }
    fn perform_action(&self, _: &ElementData, _: Action, _: Option<ActionData>) -> Result<()> {
        unreachable!()
    }
    fn check_permissions(&self) -> Result<PermissionStatus> {
        unreachable!()
    }
    fn subscribe(&self, _: &ElementData) -> Result<Subscription> {
        unreachable!()
    }
}
