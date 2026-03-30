//! Stub backend for non-Linux platforms (allows compilation on all targets).

use xa11y_core::{
    Action, ActionData, Element, Error, PermissionStatus, Provider, Result, Subscription,
};

#[derive(Default)]
pub struct LinuxProvider;

impl LinuxProvider {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

impl Provider for LinuxProvider {
    fn resolve_pid_by_name(&self, _name: &str) -> Result<u32> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }

    fn get_elements(&self, _pid: u32) -> Result<Element> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }

    fn get_apps(&self) -> Result<Element> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }

    fn perform_action(
        &self,
        _element: &Element,
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

    fn subscribe(&self, _pid: u32) -> Result<Subscription> {
        Err(Error::Platform {
            code: -1,
            message: "Linux backend not available on this platform".to_string(),
        })
    }
}
