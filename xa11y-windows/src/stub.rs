//! Stub backend for non-Windows platforms (allows compilation on all targets).

use xa11y_core::{
    Action, ActionData, Element, Error, PermissionStatus, Provider, Result, Subscription,
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
    fn resolve_pid_by_name(&self, _: &str) -> Result<u32> {
        unreachable!()
    }
    fn get_elements(&self, _: u32) -> Result<Element> {
        unreachable!()
    }
    fn get_apps(&self) -> Result<Element> {
        unreachable!()
    }
    fn perform_action(&self, _: &Element, _: Action, _: Option<ActionData>) -> Result<()> {
        unreachable!()
    }
    fn check_permissions(&self) -> Result<PermissionStatus> {
        unreachable!()
    }
    fn subscribe(&self, _: u32) -> Result<Subscription> {
        unreachable!()
    }
}
