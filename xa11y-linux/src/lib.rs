//! Linux accessibility backend.
//!
//! - AT-SPI2 introspection over D-Bus (`atspi`) — implements the core
//!   `Provider` trait. Requires `at-spi2-core` and toolkit accessibility.
//! - Input simulation (`input`, `wayland_input`) — XTest on X11 sessions,
//!   libei via `org.freedesktop.portal.RemoteDesktop` on Wayland sessions.
//!   Routing is at runtime via `DISPLAY` / `WAYLAND_DISPLAY`.
//! - Screen capture (`screenshot`) — `GetImage` on X11, the Screenshot
//!   portal on Wayland.

#[cfg(target_os = "linux")]
mod atspi;

#[cfg(target_os = "linux")]
mod events;

#[cfg(target_os = "linux")]
mod input;

#[cfg(target_os = "linux")]
mod wayland_input;

#[cfg(target_os = "linux")]
mod scale;

#[cfg(target_os = "linux")]
mod screenshot;

#[cfg(target_os = "linux")]
pub use atspi::LinuxProvider;

#[cfg(target_os = "linux")]
pub use input::LinuxInputProvider;

#[cfg(target_os = "linux")]
pub use screenshot::LinuxScreenshot;

#[cfg(not(target_os = "linux"))]
mod stub;

#[cfg(not(target_os = "linux"))]
pub use stub::{LinuxInputProvider, LinuxProvider, LinuxScreenshot};
