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
        fn press(&self, _: &ElementData) -> Result<()> {
            unreachable!()
        }
        fn focus(&self, _: &ElementData) -> Result<()> {
            unreachable!()
        }
        fn blur(&self, _: &ElementData) -> Result<()> {
            unreachable!()
        }
        fn toggle(&self, _: &ElementData) -> Result<()> {
            unreachable!()
        }
        fn select(&self, _: &ElementData) -> Result<()> {
            unreachable!()
        }
        fn expand(&self, _: &ElementData) -> Result<()> {
            unreachable!()
        }
        fn collapse(&self, _: &ElementData) -> Result<()> {
            unreachable!()
        }
        fn show_menu(&self, _: &ElementData) -> Result<()> {
            unreachable!()
        }
        fn increment(&self, _: &ElementData) -> Result<()> {
            unreachable!()
        }
        fn decrement(&self, _: &ElementData) -> Result<()> {
            unreachable!()
        }
        fn scroll_into_view(&self, _: &ElementData) -> Result<()> {
            unreachable!()
        }
        fn set_value(&self, _: &ElementData, _: &str) -> Result<()> {
            unreachable!()
        }
        fn set_numeric_value(&self, _: &ElementData, _: f64) -> Result<()> {
            unreachable!()
        }
        fn type_text(&self, _: &ElementData, _: &str) -> Result<()> {
            unreachable!()
        }
        fn set_text_selection(&self, _: &ElementData, _: u32, _: u32) -> Result<()> {
            unreachable!()
        }
        fn perform_action(&self, _: &ElementData, _: &str) -> Result<()> {
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
