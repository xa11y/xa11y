//! Windows accessibility backend using UI Automation (UIA).
//!
//! This backend implements the `Provider` trait using the Windows UI Automation API.
//! No special permissions are required for local UIA queries.

#[cfg(target_os = "windows")]
mod uia;

#[cfg(target_os = "windows")]
pub use uia::WindowsProvider;

#[cfg(not(target_os = "windows"))]
mod stub;

#[cfg(not(target_os = "windows"))]
pub use stub::WindowsProvider;

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use xa11y_core::*;

    #[test]
    fn create_provider() {
        let result = WindowsProvider::new();
        #[cfg(target_os = "windows")]
        assert!(result.is_ok());
        #[cfg(not(target_os = "windows"))]
        assert!(result.is_err());
    }
}
