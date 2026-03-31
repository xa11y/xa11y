//! macOS accessibility backend using AXUIElement API.

#[cfg(target_os = "macos")]
mod ax;

#[cfg(target_os = "macos")]
pub use ax::MacOSProvider;

#[cfg(not(target_os = "macos"))]
mod stub {
    use xa11y_core::*;

    pub struct MacOSProvider;

    impl MacOSProvider {
        pub fn new() -> Result<Self> {
            Err(Error::Platform {
                code: -1,
                message: "macOS backend only available on macOS".to_string(),
            })
        }
    }

    impl Provider for MacOSProvider {
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
}

#[cfg(not(target_os = "macos"))]
pub use stub::MacOSProvider;

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use xa11y_core::*;

    #[test]
    fn create_provider() {
        let result = MacOSProvider::new();
        #[cfg(target_os = "macos")]
        assert!(result.is_ok());
        #[cfg(not(target_os = "macos"))]
        assert!(result.is_err());
    }
}
