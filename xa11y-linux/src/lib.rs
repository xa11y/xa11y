//! Linux accessibility backend using AT-SPI2 over D-Bus.
//!
//! This backend implements the `Provider` trait using the AT-SPI2 accessibility API.
//! Requires `at-spi2-core` package and toolkit accessibility to be enabled.

#[cfg(target_os = "linux")]
mod atspi;

#[cfg(target_os = "linux")]
mod events;

#[cfg(target_os = "linux")]
pub use atspi::LinuxProvider;

#[cfg(not(target_os = "linux"))]
mod stub;

#[cfg(not(target_os = "linux"))]
pub use stub::LinuxProvider;
