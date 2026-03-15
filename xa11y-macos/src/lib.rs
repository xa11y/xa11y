//! macOS accessibility backend using AXUIElement APIs.
//!
//! This backend uses Apple's Accessibility API (AXUIElement) to read
//! accessibility trees from running applications on macOS.
//!
//! This crate only provides functionality on macOS. On other platforms,
//! the crate compiles but exports nothing.

#[cfg(target_os = "macos")]
mod platform;

#[cfg(target_os = "macos")]
pub use platform::MacOSProvider;

#[cfg(target_os = "macos")]
pub fn create_provider() -> MacOSProvider {
    MacOSProvider::new()
}
