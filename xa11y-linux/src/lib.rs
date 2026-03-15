//! Linux accessibility backend using AT-SPI2 over D-Bus.
//!
//! This backend uses the AT-SPI2 accessibility framework to read
//! accessibility trees from running applications on Linux.
//!
//! This crate only provides functionality on Linux. On other platforms,
//! the crate compiles but exports nothing.

#[cfg(target_os = "linux")]
mod platform;

#[cfg(target_os = "linux")]
pub use platform::LinuxProvider;

#[cfg(target_os = "linux")]
pub fn create_provider() -> LinuxProvider {
    LinuxProvider::new()
}
