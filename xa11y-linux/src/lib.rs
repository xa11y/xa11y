//! Linux accessibility backend.
//!
//! - AT-SPI2 introspection over D-Bus (`atspi`) with native EWMH/ICCCM and
//!   Sway window management — implements the core `Provider` trait.
//! - Input simulation (`input`, `wayland_input`) — XTest on X11 sessions,
//!   a `/dev/uinput` virtual evdev device on Wayland sessions. uinput needs
//!   write access to `/dev/uinput`: membership in the `input` group, or a
//!   udev rule granting it. Routing is per capability at runtime, with
//!   explicit environment overrides and correct mixed Wayland/XWayland
//!   detection.
//! - Screen capture (`screenshot`) — `GetImage` on X11, the Screenshot
//!   portal on Wayland.
//!
//! # Delivery disposition of action failures
//!
//! Every error answers [`xa11y_core::Error::delivery`]. A failure is
//! `NotDelivered` only when this backend decided it before sending anything
//! that mutates the application, and `Unknown` for every error from a call
//! that was sent. A dispatched `DoAction`, `SetCurrentValue`,
//! `SetTextContents` or `Select*` that fails or answers `false` is `Unknown`.
//! A `GrabFocus` that answers `false` or D-Bus `NotSupported` is the adapter
//! declining to take focus, kept as `ActionNotSupported` (`NotDelivered`)
//! because focusing twice is harmless. A step that fails after an earlier step
//! of the same verb went through (`set_value` after `DeleteText`, the second
//! EWMH message of `minimize`) is `Unknown`. The full table is in the
//! "Delivery disposition" section of the platform details reference.

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
mod session;

#[cfg(target_os = "linux")]
mod window;

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
