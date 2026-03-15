//! Windows accessibility backend using UI Automation (UIA).
//!
//! This backend uses Microsoft's UI Automation API to read
//! accessibility trees from running applications on Windows.
//!
//! This crate only provides functionality on Windows. On other platforms,
//! the crate compiles but exports nothing.

#[cfg(target_os = "windows")]
mod mapping;
#[cfg(target_os = "windows")]
mod platform;

#[cfg(target_os = "windows")]
pub use platform::WindowsProvider;

#[cfg(target_os = "windows")]
pub fn create_provider() -> WindowsProvider {
    WindowsProvider::new()
}
