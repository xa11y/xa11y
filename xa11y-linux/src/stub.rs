//! Stub backend for non-Linux platforms (allows compilation on all targets).

use xa11y_core::{
    Action, ActionData, ElementData, Error, PermissionStatus, Provider, Result, Subscription,
};

#[derive(Default)]
pub struct LinuxProvider;

impl LinuxProvider {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

impl Provider for LinuxProvider {
    fn get_children(&self, _: Option<&ElementData>) -> Result<Vec<ElementData>> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }

    fn get_parent(&self, _: &ElementData) -> Result<Option<ElementData>> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }

    fn perform_action(&self, _: &ElementData, _: Action, _: Option<ActionData>) -> Result<()> {
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

    fn subscribe(&self, _: &ElementData) -> Result<Subscription> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }
}
