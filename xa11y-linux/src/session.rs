//! Linux desktop-session detection and per-capability backend overrides.
//!
//! A Wayland desktop commonly exports both `WAYLAND_DISPLAY` and `DISPLAY`;
//! the latter is the XWayland endpoint, not evidence that the desktop itself
//! is X11.  Every Linux capability asks this module for its own route so input,
//! capture, and window management can be overridden independently.

use xa11y_core::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DesktopBackend {
    X11,
    Wayland,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionEnvironment {
    display: bool,
    wayland_display: bool,
    session_type: Option<DesktopBackend>,
}

impl SessionEnvironment {
    fn current() -> Self {
        let session_type = std::env::var("XDG_SESSION_TYPE").ok().and_then(|value| {
            match value.to_ascii_lowercase().as_str() {
                "x11" => Some(DesktopBackend::X11),
                "wayland" => Some(DesktopBackend::Wayland),
                _ => None,
            }
        });
        Self {
            display: std::env::var_os("DISPLAY").is_some(),
            wayland_display: std::env::var_os("WAYLAND_DISPLAY").is_some(),
            session_type,
        }
    }
}

/// Select one backend for a capability.
///
/// `override_var` accepts `auto`, `x11`, or `wayland`. In auto mode an
/// explicit `XDG_SESSION_TYPE` wins when its endpoint exists; otherwise a
/// Wayland endpoint wins over `DISPLAY`, because mixed sessions export the
/// latter for XWayland clients. No endpoint means an actionable unsupported
/// error rather than an attempted connection to a guessed backend.
pub(crate) fn select_backend(override_var: &str, capability: &str) -> Result<DesktopBackend> {
    select_backend_from(
        std::env::var(override_var).ok().as_deref(),
        SessionEnvironment::current(),
        override_var,
        capability,
    )
}

fn select_backend_from(
    override_value: Option<&str>,
    env: SessionEnvironment,
    override_var: &str,
    capability: &str,
) -> Result<DesktopBackend> {
    let requested = override_value.unwrap_or("auto").to_ascii_lowercase();
    let selected = match requested.as_str() {
        "auto" | "" => match env.session_type {
            Some(DesktopBackend::Wayland) if env.wayland_display => DesktopBackend::Wayland,
            Some(DesktopBackend::X11) if env.display => DesktopBackend::X11,
            _ if env.wayland_display => DesktopBackend::Wayland,
            _ if env.display => DesktopBackend::X11,
            _ if capability == "input simulation" => DesktopBackend::Wayland,
            _ => {
                return Err(Error::Unsupported {
                    feature: format!(
                        "{capability}: no usable desktop endpoint; set WAYLAND_DISPLAY or DISPLAY, \
                         or set {override_var}=wayland|x11 explicitly"
                    ),
                });
            }
        },
        "x11" => {
            if !env.display {
                return Err(Error::Unsupported {
                    feature: format!(
                        "{capability}: {override_var}=x11 requires DISPLAY to name an X server"
                    ),
                });
            }
            DesktopBackend::X11
        }
        "wayland" | "uinput" if capability == "input simulation" => {
            // The Wayland input implementation is kernel uinput and therefore
            // remains useful in compositor-less test environments.
            DesktopBackend::Wayland
        }
        "wayland" => {
            if !env.wayland_display {
                return Err(Error::Unsupported {
                    feature: format!(
                        "{capability}: {override_var}=wayland requires WAYLAND_DISPLAY to name a compositor"
                    ),
                });
            }
            DesktopBackend::Wayland
        }
        other => {
            return Err(Error::InvalidActionData {
                message: format!(
                    "invalid {override_var} value {other:?}; expected auto, x11, or wayland"
                ),
            });
        }
    };
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::{select_backend_from, DesktopBackend, SessionEnvironment};
    use xa11y_core::Error;

    fn env(display: bool, wayland_display: bool) -> SessionEnvironment {
        SessionEnvironment {
            display,
            wayland_display,
            session_type: None,
        }
    }

    #[test]
    fn auto_routes_pure_and_mixed_sessions() {
        assert_eq!(
            select_backend_from(None, env(true, false), "OVERRIDE", "capture").unwrap(),
            DesktopBackend::X11
        );
        assert_eq!(
            select_backend_from(None, env(false, true), "OVERRIDE", "capture").unwrap(),
            DesktopBackend::Wayland
        );
        assert_eq!(
            select_backend_from(None, env(true, true), "OVERRIDE", "capture").unwrap(),
            DesktopBackend::Wayland
        );
    }

    #[test]
    fn explicit_overrides_require_their_endpoint() {
        assert_eq!(
            select_backend_from(Some("x11"), env(true, true), "OVERRIDE", "capture").unwrap(),
            DesktopBackend::X11
        );
        assert!(matches!(
            select_backend_from(Some("x11"), env(false, true), "OVERRIDE", "capture"),
            Err(Error::Unsupported { .. })
        ));
        assert!(matches!(
            select_backend_from(Some("wayland"), env(true, false), "OVERRIDE", "capture"),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn stale_session_type_does_not_override_a_live_endpoint() {
        let stale_wayland = SessionEnvironment {
            display: true,
            wayland_display: false,
            session_type: Some(DesktopBackend::Wayland),
        };
        assert_eq!(
            select_backend_from(None, stale_wayland, "OVERRIDE", "capture").unwrap(),
            DesktopBackend::X11
        );
    }

    #[test]
    fn invalid_override_is_actionable() {
        assert!(matches!(
            select_backend_from(Some("magic"), env(true, true), "OVERRIDE", "capture"),
            Err(Error::InvalidActionData { .. })
        ));
    }
}
