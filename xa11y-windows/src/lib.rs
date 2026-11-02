//! Windows accessibility backend using UI Automation (UIA).
//!
//! This backend implements the `Provider` trait using the Windows UI Automation API.
//! No special permissions are required for local UIA queries.

mod splice;

#[cfg(target_os = "windows")]
mod coords;
#[cfg(target_os = "windows")]
mod dpi;
#[cfg(target_os = "windows")]
mod input;
#[cfg(target_os = "windows")]
mod uia;
#[cfg(target_os = "windows")]
mod uipi;

mod screenshot;

#[cfg(target_os = "windows")]
pub use input::WindowsInputProvider;
#[cfg(target_os = "windows")]
pub use uia::WindowsProvider;

/// How many times an xa11y call on this thread switched it to Per-Monitor V2
/// DPI awareness. Test support for the system-aware host test; not API.
#[doc(hidden)]
#[cfg(target_os = "windows")]
pub fn __dpi_scope_switches() -> u64 {
    dpi::scope_switches()
}

pub use screenshot::WindowsScreenshot;

#[cfg(not(target_os = "windows"))]
mod stub;

#[cfg(not(target_os = "windows"))]
pub use stub::{WindowsInputProvider, WindowsProvider};

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use xa11y_core::*;

    #[test]
    fn create_provider() {
        let result = WindowsProvider::new();
        #[cfg(target_os = "windows")]
        // No E_FAIL skip: the first-touch race it hid is serialized now
        // (issue #500), so any failure is a regression.
        if let Err(e) = &result {
            panic!("WindowsProvider::new failed: {e}");
        }
        #[cfg(not(target_os = "windows"))]
        assert!(result.is_err());
    }
}
