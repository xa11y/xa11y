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
//! Every error answers [`xa11y_core::Error::delivery`]: whether the failed
//! action may already have taken effect. A caller retries only on
//! `NotDelivered`. This is how AT-SPI2 and window-manager failures map.
//!
//! `NotDelivered` (decided before any call that mutates the application):
//!
//! - `ActionNotSupported`: the element lists no matching AT-SPI action, lacks
//!   the `Value` / `EditableText` / `Selection` interface the verb needs, or
//!   is not a top-level window for a window verb.
//! - `ElementStale`: `ensure_attached` found the object or its peer gone
//!   (`UnknownObject`, `ServiceUnknown`, `NameHasNoOwner`) before dispatch.
//! - `InvalidActionData`, `InvalidSelector`, `NoElementBounds`,
//!   `Unsupported`: rejected from the arguments or the session shape.
//!
//! `Unknown` (the call was sent, or may have been):
//!
//! - `Timeout` from an unresponsive peer, the D-Bus `NoReply` / method
//!   timeout. The application may have applied the call and failed to answer.
//! - `Platform` from the dispatching call itself: a failed `DoAction`,
//!   `SetCurrentValue`, `SetTextContents`, `GrabFocus` or `Select*`, a reply
//!   that does not decode, and `DoAction` returning `false`. A provider that
//!   returns `false` after acting is indistinguishable from one that declined,
//!   so `false` is `Unknown`, not `NotDelivered`.
//! - `Platform` from a window-manager request (EWMH client message, Sway IPC)
//!   that was sent and not confirmed.
//! - Any other `Platform` or `Timeout`, including one raised by a read that
//!   runs before dispatch (an `ensure_attached` call that times out). The
//!   backend does not split those apart, so they stay `Unknown`.

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
