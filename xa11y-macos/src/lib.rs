//! macOS accessibility backend using AXUIElement API.
//!
//! Implements the `Provider` trait by reading the macOS accessibility tree
//! via the Accessibility framework (ApplicationServices/HIServices).

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
        // On macOS: should succeed (may or may not have permissions)
        // On other platforms: should fail
        #[cfg(target_os = "macos")]
        assert!(result.is_ok());
        #[cfg(not(target_os = "macos"))]
        assert!(result.is_err());
    }
}
