//! Real AT-SPI2 backend implementation using zbus D-Bus bindings.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use rayon::prelude::*;
use xa11y_core::{
    ElementData, ElementParts, Error, Provider, Rect, Result, Role, ShellSurfaceKind, StateParts,
    StateSet, Subscription, Toggled,
};
use zbus::blocking::{Connection, Proxy};
use zbus::DBusError;

use crate::window::WindowManager;

/// Upper bound on a single AT-SPI D-Bus method call.
///
/// zbus applies no method timeout of its own, so without this a call to an
/// application that is stopped (`SIGSTOP`, a debugger) or whose main loop is
/// hung blocks forever — and because enumeration reads every application on
/// the bus, one such app hangs `App::list` and `App::by_name` for everyone.
/// Long enough for a slow but live toolkit to answer a large `GetChildren`;
/// short enough that a hung peer surfaces as an error in seconds.
pub(crate) const ATSPI_METHOD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Global handle counter for mapping ElementData back to AccessibleRefs.
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Format a normalized state attribute as the same string `xa11y_core::selector::resolve_attr`
/// would produce, so fast-path matching agrees byte-for-byte with the slow path.
fn state_attr_to_string(name: &str, s: &StateSet) -> Option<String> {
    match name {
        "enabled" => Some(s.enabled.to_string()),
        "visible" => Some(s.visible.to_string()),
        "focused" => Some(s.focused.to_string()),
        "active" => Some(s.active.to_string()),
        "focusable" => Some(s.focusable.to_string()),
        "selected" => Some(s.selected.to_string()),
        "editable" => Some(s.editable.to_string()),
        "modal" => Some(s.modal.to_string()),
        "required" => Some(s.required.to_string()),
        "busy" => Some(s.busy.to_string()),
        "expanded" => s.expanded.map(|b| b.to_string()),
        "checked" => s.checked.map(|c| {
            match c {
                Toggled::On => "on",
                Toggled::Off => "off",
                Toggled::Mixed => "mixed",
            }
            .to_string()
        }),
        _ => None,
    }
}

/// Linux accessibility provider using AT-SPI2 over D-Bus.
pub struct LinuxProvider {
    /// The accessibility-bus connection. Behind a lock so a connection the
    /// bus closed can be replaced; see [`LinuxProvider::connection`].
    a11y_bus: Mutex<Connection>,
    /// Cached AT-SPI accessible refs keyed by handle ID.
    handle_cache: Mutex<HashMap<u64, AccessibleRef>>,
    /// Cached AT-SPI2 action indices keyed by element handle.
    /// Maps each action name (snake_case) to the integer index used by `DoAction(i)`.
    action_indices: Mutex<HashMap<u64, HashMap<String, i32>>>,
    /// Native X11/Wayland window-manager channel, selected independently of
    /// the AT-SPI accessibility connection.
    window_manager: WindowManager,
    /// Per-application facts for GTK 4's screen extents (issue #487).
    gtk4: Gtk4Geometry,
}

/// Caches for [`LinuxProvider::gtk4_screen_extents`]: whether each
/// application bus is GTK 4, and each object's top-level window. Cleared
/// with the handle cache on a reconnect, which reuses bus names.
#[derive(Default)]
struct Gtk4Geometry {
    is_gtk4: Mutex<HashMap<String, bool>>,
    /// `bus + path` → the top-level window's `(bus, path)`.
    toplevels: Mutex<HashMap<String, AccessibleRef>>,
}

/// AT-SPI2 accessible reference: (bus_name, object_path).
#[derive(Debug, Clone)]
pub(crate) struct AccessibleRef {
    pub(crate) bus_name: String,
    pub(crate) path: String,
}

/// The optional AT-SPI interfaces an object reports via
/// `Accessible.GetInterfaces`, read once per node so every property fetch
/// that depends on one can be gated without another round trip.
///
/// Gating matters beyond the round trips: atk-bridge answers `Text`,
/// `Value` and `Component` reads on *every* object and its handlers log a
/// `CRITICAL` assertion for each one whose `AtkObject` lacks the interface
/// (issue #465 for Text/Value; GTK TreeView cell renderers for Component).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Interfaces {
    pub(crate) component: bool,
    pub(crate) text: bool,
    pub(crate) value: bool,
}

impl Interfaces {
    pub(crate) fn from_names<S: AsRef<str>>(names: &[S]) -> Self {
        let has = |iface: &str| names.iter().any(|n| n.as_ref() == iface);
        Self {
            component: has("org.a11y.atspi.Component"),
            text: has("org.a11y.atspi.Text"),
            value: has("org.a11y.atspi.Value"),
        }
    }
}

/// Call `GetInterfaces` on a proxy for an object's
/// `org.a11y.atspi.Accessible` interface.
pub(crate) fn read_interfaces(accessible: &Proxy<'_>) -> Result<Interfaces> {
    let reply = accessible
        .call_method("GetInterfaces", &())
        .map_err(|e| Error::Platform {
            code: -1,
            message: format!("GetInterfaces failed: {}", e),
        })?;
    let names: Vec<String> = reply.body().deserialize().map_err(|e| Error::Platform {
        code: -1,
        message: format!("GetInterfaces deserialize failed: {}", e),
    })?;
    Ok(Interfaces::from_names(&names))
}

impl LinuxProvider {
    /// Create a new Linux accessibility provider.
    ///
    /// Connects to the dedicated AT-SPI2 accessibility bus, whose address
    /// the `org.a11y.Bus` launcher publishes on the session bus. There is no
    /// fallback to the session bus itself: the AT-SPI registry never lives
    /// there, so a provider connected to it would answer every query with an
    /// empty desktop and lose the reason the real connection failed.
    pub fn new() -> Result<Self> {
        let a11y_bus = Self::connect_a11y_bus()?;
        Ok(Self {
            a11y_bus: Mutex::new(a11y_bus),
            handle_cache: Mutex::new(HashMap::new()),
            action_indices: Mutex::new(HashMap::new()),
            window_manager: WindowManager::new(),
            gtk4: Gtk4Geometry::default(),
        })
    }

    /// Connect to the AT-SPI2 accessibility bus.
    ///
    /// Each way this can fail is reported as its own error naming what was
    /// tried: the session bus being unreachable, the `org.a11y.Bus` launcher
    /// being absent (AT-SPI is not installed or not running), the launcher
    /// failing to answer, or the published address refusing a connection.
    ///
    /// Both connections carry [`ATSPI_METHOD_TIMEOUT`], so a hung bus
    /// launcher or a stopped application fails a call instead of blocking it
    /// forever (zbus has no default method timeout).
    pub(crate) fn connect_a11y_bus() -> Result<Connection> {
        let address = Self::a11y_bus_address()?;
        let parsed = zbus::Address::try_from(address.as_str()).map_err(|e| Error::Platform {
            code: -1,
            message: format!(
                "the AT-SPI bus launcher (org.a11y.Bus) published an unparsable \
                 accessibility bus address {address:?}: {e}"
            ),
        })?;
        zbus::blocking::connection::Builder::address(parsed)
            .map(|b| b.method_timeout(ATSPI_METHOD_TIMEOUT))
            .and_then(|b| b.build())
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!(
                    "could not connect to the AT-SPI accessibility bus at {address:?} \
                     (published by org.a11y.Bus on the session bus): {e}"
                ),
            })
    }

    /// Ask the `org.a11y.Bus` launcher on the session bus for the address
    /// of the accessibility bus.
    fn a11y_bus_address() -> Result<String> {
        let session_address =
            std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_else(|_| "<unset>".to_string());
        let session = zbus::blocking::connection::Builder::session()
            .map(|b| b.method_timeout(ATSPI_METHOD_TIMEOUT))
            .and_then(|b| b.build())
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!(
                    "could not connect to the D-Bus session bus \
                     (DBUS_SESSION_BUS_ADDRESS={session_address}) to look up the AT-SPI \
                     accessibility bus: {e}"
                ),
            })?;
        let reply = session
            .call_method(
                Some("org.a11y.Bus"),
                "/org/a11y/bus",
                Some("org.a11y.Bus"),
                "GetAddress",
                &(),
            )
            .map_err(|e| {
                if is_peer_gone(&e) {
                    Error::Platform {
                        code: -1,
                        message: format!(
                            "AT-SPI is not running: no org.a11y.Bus service on the session bus \
                             (DBUS_SESSION_BUS_ADDRESS={session_address}). Install at-spi2-core \
                             and start at-spi-bus-launcher, or log in to a desktop session that \
                             runs it: {e}"
                        ),
                    }
                } else {
                    Error::Platform {
                        code: -1,
                        message: format!(
                            "the AT-SPI bus launcher (org.a11y.Bus on the session bus \
                             {session_address}) did not return the accessibility bus address: {e}"
                        ),
                    }
                }
            })?;
        reply
            .body()
            .deserialize::<String>()
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("org.a11y.Bus.GetAddress returned an unexpected reply: {e}"),
            })
    }

    /// The live accessibility-bus connection, reconnecting first when the
    /// previous one has closed.
    ///
    /// zbus marks a connection closed when its socket hits an I/O error or
    /// the bus hangs up (the bus was restarted, the session ended). Every call
    /// on a closed connection fails, and the provider is memoized for the
    /// life of the process, so without this one dropped socket would break
    /// the process for good. The call that observed the failure still
    /// returned its error; only the next one reconnects. A failed reconnect
    /// is returned as-is and leaves the closed connection in place, so the
    /// call after that tries again.
    fn connection(&self) -> Result<Connection> {
        self.connection_with(Self::connect_a11y_bus)
    }

    /// [`connection`](Self::connection) with the reconnect step supplied,
    /// so the replacement path is testable without a desktop bus.
    fn connection_with(
        &self,
        reconnect: impl FnOnce() -> Result<Connection>,
    ) -> Result<Connection> {
        let mut conn = self.a11y_bus.lock().unwrap_or_else(|e| e.into_inner());
        if conn.is_closed() {
            log::warn!("AT-SPI accessibility bus connection closed; reconnecting");
            *conn = reconnect()?;
            // Handles minted on the old connection name objects by bus
            // unique name, and a restarted bus hands the same `:1.N` names
            // out again — to different applications. Forget them, so an old
            // handle is `ElementStale` instead of silently resolving to
            // whatever owns that name now. Taken while `a11y_bus` is held so
            // no call can mint a handle on the new connection first; neither
            // map's lock is ever held while acquiring `a11y_bus`.
            self.handle_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            self.action_indices
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            self.gtk4
                .is_gtk4
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            self.gtk4
                .toplevels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
        Ok(conn.clone())
    }

    fn make_proxy(&self, bus_name: &str, path: &str, interface: &str) -> Result<Proxy<'_>> {
        // Use uncached proxy to avoid GetAll calls — Qt's AT-SPI adaptor
        // doesn't support GetAll on all objects, causing spurious errors.
        let conn = self.connection()?;
        zbus::blocking::proxy::Builder::<Proxy>::new(&conn)
            .destination(bus_name.to_owned())
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Failed to set proxy destination: {}", e),
            })?
            .path(path.to_owned())
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Failed to set proxy path: {}", e),
            })?
            .interface(interface.to_owned())
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Failed to set proxy interface: {}", e),
            })?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Failed to create proxy: {}", e),
            })
    }

    /// Read the interfaces an accessible object reports via
    /// `Accessible.GetInterfaces`.
    fn get_interfaces(&self, aref: &AccessibleRef) -> Result<Interfaces> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        read_interfaces(&proxy)
    }

    /// Get the numeric AT-SPI role via GetRole method.
    fn get_role_number(&self, aref: &AccessibleRef) -> Result<u32> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        let reply = proxy
            .call_method("GetRole", &())
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetRole failed: {}", e),
            })?;
        reply
            .body()
            .deserialize::<u32>()
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetRole deserialize failed: {}", e),
            })
    }

    /// Get the AT-SPI role name string.
    fn get_role_name(&self, aref: &AccessibleRef) -> Result<String> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        let reply = proxy
            .call_method("GetRoleName", &())
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetRoleName failed: {}", e),
            })?;
        reply
            .body()
            .deserialize::<String>()
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetRoleName deserialize failed: {}", e),
            })
    }

    /// Detect Chromium/Electron's "accessibility bridge disabled" signature.
    ///
    /// On Linux, Chromium-based apps (Electron, Chrome, VS Code, …) register
    /// with AT-SPI but expose only an `application → frame` skeleton — the
    /// frame's children list is empty (just the `/org/a11y/atspi/null`
    /// sentinel) — unless the process was launched with the
    /// `--force-renderer-accessibility` flag. Without this detection, callers
    /// see zero results and assume their selector is wrong.
    ///
    /// Call this after observing an empty filtered children list. Returns an
    /// error only when the parent is a window/frame whose AT-SPI bus reports
    /// `Application.ToolkitName == "Chromium"`; otherwise returns `Ok(())` so
    /// genuinely empty windows in other toolkits stay valid.
    ///
    /// `role_hint` lets callers that already know the role skip a `GetRole`
    /// round-trip.
    fn check_chromium_a11y_enabled(
        &self,
        parent: &AccessibleRef,
        role_hint: Option<Role>,
    ) -> Result<()> {
        let app_root = AccessibleRef {
            bus_name: parent.bus_name.clone(),
            path: "/org/a11y/atspi/accessible/root".to_string(),
        };
        let toolkit = match self
            .make_proxy(
                &app_root.bus_name,
                &app_root.path,
                "org.a11y.atspi.Application",
            )
            .ok()
            .and_then(|proxy| proxy.get_property::<String>("ToolkitName").ok())
        {
            Some(t) => t,
            None => return Ok(()),
        };
        if !toolkit.eq_ignore_ascii_case("Chromium") {
            return Ok(());
        }
        let role = role_hint.unwrap_or_else(|| self.resolve_role(parent));
        if role != Role::Window {
            return Ok(());
        }
        let app_name = self.get_name(&app_root).unwrap_or_default();
        Err(Error::AccessibilityNotEnabled {
            app: app_name,
            instructions: "Chromium/Electron app exposes an empty accessibility tree on Linux. \
                Relaunch it with `--force-renderer-accessibility`. In a session with no \
                settings daemon (CI, Xvfb, a container), also set `ACCESSIBILITY_ENABLED=1` \
                in its environment: there Chromium needs both before it exposes web content."
                .to_string(),
        })
    }

    /// Get the name of an accessible element.
    fn get_name(&self, aref: &AccessibleRef) -> Result<String> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        proxy
            .get_property::<String>("Name")
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Get Name property failed: {}", e),
            })
    }

    /// Read a registry entry's application name for enumeration.
    ///
    /// `Ok(None)` means the entry has no name to report: the application
    /// left the bus between `GetChildren` and this read, or its root does
    /// not implement `Name`. A peer that does not answer within
    /// [`ATSPI_METHOD_TIMEOUT`] is an [`Error::Timeout`] naming the
    /// application, its pid and its process state, so the caller can see
    /// which app is hung instead of the listing stalling or quietly
    /// omitting it.
    fn read_app_name(&self, aref: &AccessibleRef) -> Result<Option<String>> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        match proxy.get_property::<String>("Name") {
            Ok(name) => Ok(Some(name)),
            Err(e) if is_peer_gone(&e) || is_absent_member(&e) => Ok(None),
            Err(e) if is_timeout(&e) => Err(self.unresponsive_peer(aref, "Accessible.Name")),
            Err(e) => Err(Error::Platform {
                code: -1,
                message: format!(
                    "reading the name of AT-SPI application {}{}{} failed: {e}",
                    aref.bus_name,
                    aref.path,
                    self.get_dbus_pid(&aref.bus_name)
                        .map(|p| format!(" (pid {p})"))
                        .unwrap_or_default(),
                ),
            }),
        }
    }

    /// `GetChildren` for a registry scan: `Ok(None)` when the peer left the
    /// bus, the object was destroyed, or it does not implement the call;
    /// [`Error::Timeout`] naming the app when it does not answer.
    fn scan_children(&self, aref: &AccessibleRef) -> Result<Option<Vec<AccessibleRef>>> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        let reply = match proxy.call_method("GetChildren", &()) {
            Ok(reply) => reply,
            Err(e) if is_peer_gone(&e) || is_absent_member(&e) => return Ok(None),
            Err(e) if is_timeout(&e) => {
                return Err(self.unresponsive_peer(aref, "Accessible.GetChildren"))
            }
            Err(e) => return Err(children_call_error(e, aref)),
        };
        let children: Vec<(String, zbus::zvariant::OwnedObjectPath)> =
            reply.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetChildren deserialize failed: {}", e),
            })?;
        Ok(Some(
            children
                .into_iter()
                .map(|(bus_name, path)| AccessibleRef {
                    bus_name,
                    path: path.to_string(),
                })
                .collect(),
        ))
    }

    /// `GetState` as one 64-bit set, for a decision that must not guess (the
    /// registry scans and the `App::windows()` filter, #492). `Ok(None)` when
    /// the object or its peer is gone: GDBus (GTK 4) answers a destroyed
    /// object with `UnknownMethod`, so every absent-member reply counts.
    /// [`Error::Timeout`] naming the app when it does not answer; any other
    /// failure is returned. Contrast [`Self::state_bits`], which the snapshot
    /// path uses to tolerate an unreadable state per field.
    fn scan_state_bits(&self, aref: &AccessibleRef) -> Result<Option<u64>> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        let reply = match proxy.call_method("GetState", &()) {
            Ok(reply) => reply,
            Err(e) if is_peer_gone(&e) || is_absent_member(&e) => return Ok(None),
            Err(e) if is_timeout(&e) => {
                return Err(self.unresponsive_peer(aref, "Accessible.GetState"))
            }
            Err(e) => {
                return Err(Error::Platform {
                    code: -1,
                    message: format!("GetState on {}{} failed: {e}", aref.bus_name, aref.path),
                })
            }
        };
        let words: Vec<u32> = reply.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!(
                "GetState deserialize failed for {}{}: {e}",
                aref.bus_name, aref.path
            ),
        })?;
        Ok(Some(
            words
                .iter()
                .take(2)
                .enumerate()
                .fold(0u64, |acc, (i, w)| acc | ((*w as u64) << (32 * i))),
        ))
    }

    /// The [`Error::Timeout`] for a peer that did not answer `call` within
    /// [`ATSPI_METHOD_TIMEOUT`].
    ///
    /// Built on the failure path only: it costs one bus-daemon round trip
    /// for the pid (the daemon answers even when the application cannot)
    /// and one `/proc` read for the process state.
    fn unresponsive_peer(&self, aref: &AccessibleRef, call: &str) -> Error {
        let pid = self.get_dbus_pid(&aref.bus_name);
        unresponsive_peer_error(aref, call, pid, pid.and_then(read_process_state))
    }

    /// Get the description of an accessible element.
    fn get_description(&self, aref: &AccessibleRef) -> Result<String> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        proxy
            .get_property::<String>("Description")
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Get Description property failed: {}", e),
            })
    }

    /// Get children via the GetChildren method.
    /// AT-SPI registryd doesn't always implement standard D-Bus Properties,
    /// so we use GetChildren which is more reliable.
    fn get_atspi_children(&self, aref: &AccessibleRef) -> Result<Vec<AccessibleRef>> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        let reply = proxy.call_method("GetChildren", &()).map_err(|e| {
            if is_timeout(&e) {
                self.unresponsive_peer(aref, "Accessible.GetChildren")
            } else {
                children_call_error(e, aref)
            }
        })?;
        let children: Vec<(String, zbus::zvariant::OwnedObjectPath)> =
            reply.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetChildren deserialize failed: {}", e),
            })?;
        Ok(children
            .into_iter()
            .map(|(bus_name, path)| AccessibleRef {
                bus_name,
                path: path.to_string(),
            })
            .collect())
    }

    /// Read a live subtree, distinguishing a destroyed object from an empty
    /// child list. Only `UnknownObject` is absence; broken peers still fail.
    fn live_atspi_children(&self, aref: &AccessibleRef) -> Result<Option<Vec<AccessibleRef>>> {
        match self.get_atspi_children(aref) {
            Ok(children) => Ok(Some(children)),
            Err(Error::ElementStale { .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Get the state set as raw u32 values.
    fn get_state(&self, aref: &AccessibleRef) -> Result<Vec<u32>> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        let reply = proxy
            .call_method("GetState", &())
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetState failed: {}", e),
            })?;
        reply
            .body()
            .deserialize::<Vec<u32>>()
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetState deserialize failed: {}", e),
            })
    }

    /// `GetRole` for the shell scan, separating "the peer left the bus" from
    /// a genuine failure.
    ///
    /// `Ok(None)` means the owning application exited mid-scan; see
    /// [`PEER_GONE_ERRORS`].
    fn scan_role_number(&self, aref: &AccessibleRef) -> Result<Option<u32>> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        let reply = match proxy.call_method("GetRole", &()) {
            Ok(reply) => reply,
            Err(e) if is_peer_gone(&e) => return Ok(None),
            Err(e) => {
                return Err(Error::Platform {
                    code: -1,
                    message: format!("GetRole failed: {}", e),
                })
            }
        };
        reply
            .body()
            .deserialize::<u32>()
            .map(Some)
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetRole deserialize failed: {}", e),
            })
    }

    /// `GetAttributes` for the shell scan.
    ///
    /// Three outcomes rather than two: `Ok(None)` is the peer having left the
    /// bus, `Ok(Some(None))` is an object that does not implement
    /// `GetAttributes` (an honest "not a panel"), and `Ok(Some(Some(_)))` is a
    /// reply.
    #[allow(
        clippy::option_option,
        reason = "The two Nones mean different things \
        — peer gone versus member absent — and collapsing them would erase \
        exactly the distinction this function exists to make."
    )]
    fn scan_attributes(
        &self,
        aref: &AccessibleRef,
    ) -> Result<Option<Option<HashMap<String, String>>>> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        let reply = match proxy.call_method("GetAttributes", &()) {
            Ok(reply) => reply,
            Err(e) if is_peer_gone(&e) => return Ok(None),
            Err(e) if is_absent_member(&e) => return Ok(Some(None)),
            Err(e) => {
                return Err(Error::Platform {
                    code: -1,
                    message: format!("GetAttributes failed: {}", e),
                })
            }
        };
        reply
            .body()
            .deserialize::<HashMap<String, String>>()
            .map(|a| Some(Some(a)))
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetAttributes deserialize failed: {}", e),
            })
    }

    /// Return true if the element reports the AT-SPI MULTI_LINE state.
    /// Used to distinguish multi-line text areas (TextArea) from single-line
    /// text inputs (TextField), since both use the AT-SPI "text" role name.
    /// Note: Qt's AT-SPI bridge does not reliably set SINGLE_LINE, so we
    /// check MULTI_LINE and default to TextField when neither is set.
    fn is_multi_line(&self, aref: &AccessibleRef) -> bool {
        let state_bits = self.get_state(aref).unwrap_or_default();
        let bits: u64 = if state_bits.len() >= 2 {
            (state_bits[0] as u64) | ((state_bits[1] as u64) << 32)
        } else if state_bits.len() == 1 {
            state_bits[0] as u64
        } else {
            0
        };
        // ATSPI_STATE_MULTI_LINE = 17 in AtspiStateType enum
        const MULTI_LINE: u64 = 1 << 17;
        (bits & MULTI_LINE) != 0
    }

    /// Resolve TextField vs TextArea using the MULTI_LINE state, regardless of
    /// the coarse role we started with. Different toolkits expose text inputs
    /// under different AT-SPI roles ("text", "textbox", "entry", "embedded",
    /// numeric 61/78/79), and WebKitGTK has changed mapping between versions
    /// (e.g. 2.50 → 2.52 swapped what role textarea reports). MULTI_LINE is
    /// the cross-toolkit invariant, so we let it decide.
    ///
    /// For any other coarse role, returns it unchanged.
    fn refine_text_role(&self, coarse: Role, aref: &AccessibleRef) -> Role {
        match coarse {
            Role::TextArea | Role::TextField => {
                if self.is_multi_line(aref) {
                    Role::TextArea
                } else {
                    Role::TextField
                }
            }
            other => other,
        }
    }

    /// Get bounds via Component interface.
    /// Gated on the node reporting Component, to avoid GTK CRITICAL warnings
    /// on objects (e.g. TreeView cell renderers) that don't implement it.
    fn get_extents(&self, aref: &AccessibleRef, ifaces: Interfaces) -> Option<Rect> {
        if !ifaces.component {
            return None;
        }
        if self.is_gtk4(&aref.bus_name) {
            return self
                .gtk4_screen_extents(aref)
                .map(|r| r.to_logical(crate::scale::coordinate_scale()));
        }
        let proxy = self
            .make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Component")
            .ok()?;
        // GetExtents(coord_type: u32) -> (x, y, width, height)
        // coord_type 0 = screen coordinates
        let reply = proxy.call_method("GetExtents", &(0u32,)).ok()?;
        let (x, y, w, h): (i32, i32, i32, i32) = reply.body().deserialize().ok()?;
        // AT-SPI screen extents are physical device pixels. Convert to logical
        // coordinates (the cross-platform contract, same space as the
        // screenshot/input layers) using the detected display scale. On the
        // common 1.0 case this is a no-op. See `crate::scale`.
        extents_to_rect(x, y, w, h).map(|r| r.to_logical(crate::scale::coordinate_scale()))
    }

    /// Whether the application on `bus` is GTK 4, from
    /// `Application.ToolkitName` / `Version`. Cached per bus; an
    /// unreadable answer is "not GTK 4" and is not cached, so a transient
    /// failure is retried.
    fn is_gtk4(&self, bus: &str) -> bool {
        if let Some(known) = self
            .gtk4
            .is_gtk4
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(bus)
        {
            return *known;
        }
        let Ok(proxy) = self.make_proxy(bus, APP_ROOT_PATH, "org.a11y.atspi.Application") else {
            return false;
        };
        let (Ok(name), Ok(version)) = (
            proxy.get_property::<String>("ToolkitName"),
            // The toolkit's version is the `Version` property (GTK 4 has no
            // `ToolkitVersion`; `AtspiVersion` is the protocol's).
            proxy.get_property::<String>("Version"),
        ) else {
            return false;
        };
        let gtk4 = is_gtk4_toolkit(&name, &version);
        self.gtk4
            .is_gtk4
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(bus.to_owned(), gtk4);
        gtk4
    }

    /// Screen extents (physical pixels) for a GTK 4 object.
    ///
    /// GTK 4 does not know its window's position, and on X11 its
    /// `GetExtents(SCREEN)` reports (0, 0) for every widget (GTK 4.14) or a
    /// window-relative position, wherever the window is (issue #487). The
    /// `WINDOW` extents are right, so the screen position is those plus the
    /// root origin of the top-level's X11 client window, found by pid and
    /// title. `None` — no bounds, rather than wrong ones — on Wayland, when
    /// the window cannot be matched to exactly one X11 window, or when any
    /// read fails.
    fn gtk4_screen_extents(&self, aref: &AccessibleRef) -> Option<Rect> {
        const ATSPI_COORD_TYPE_WINDOW: u32 = 1;
        let proxy = self
            .make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Component")
            .ok()?;
        let reply = proxy
            .call_method("GetExtents", &(ATSPI_COORD_TYPE_WINDOW,))
            .ok()?;
        let (x, y, w, h): (i32, i32, i32, i32) = reply.body().deserialize().ok()?;
        let toplevel = self.toplevel_of(aref)?;
        let title = self.get_name(&toplevel).ok().filter(|t| !t.is_empty())?;
        let pid = self.get_dbus_pid(&aref.bus_name)?;
        let (ox, oy) = self.window_manager.x11_client_origin(pid, &title).ok()??;
        extents_to_rect(x.checked_add(ox)?, y.checked_add(oy)?, w, h)
    }

    /// The top-level window `aref` belongs to: the ancestor whose parent is
    /// the application object. Every object visited on the way is cached,
    /// so siblings and descendants resolve without walking again.
    fn toplevel_of(&self, aref: &AccessibleRef) -> Option<AccessibleRef> {
        let key = |r: &AccessibleRef| format!("{}{}", r.bus_name, r.path);
        let mut visited = Vec::new();
        let mut current = aref.clone();
        let toplevel = loop {
            if let Some(found) = self
                .gtk4
                .toplevels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key(&current))
            {
                break found.clone();
            }
            if current.path == APP_ROOT_PATH {
                return None;
            }
            let parent = self.get_atspi_parent(&current).ok()?;
            visited.push(current.clone());
            match parent {
                Some(p) if p.path == APP_ROOT_PATH => break current,
                Some(p) => current = p,
                None => break current,
            }
        };
        let mut cache = self
            .gtk4
            .toplevels
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for r in visited {
            cache.insert(key(&r), toplevel.clone());
        }
        Some(toplevel)
    }

    /// Get available actions via Action interface, returning both the action list
    /// and a map of each action name to its AT-SPI2 integer index for direct `DoAction(i)`.
    ///
    /// Probes the interface directly rather than relying on the Interfaces property,
    /// which some AT-SPI adapters (e.g. AccessKit) don't expose.
    fn get_actions(&self, aref: &AccessibleRef) -> (Vec<String>, HashMap<String, i32>) {
        let mut actions = Vec::new();
        let mut indices = HashMap::new();

        // Try Action interface directly
        if let Ok(proxy) = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Action") {
            // NActions may be returned as i32 or u32 depending on AT-SPI implementation.
            let n_actions = proxy
                .get_property::<i32>("NActions")
                .or_else(|_| proxy.get_property::<u32>("NActions").map(|n| n as i32))
                .unwrap_or(0);
            for i in 0..n_actions {
                if let Ok(reply) = proxy.call_method("GetName", &(i,)) {
                    if let Ok(name) = reply.body().deserialize::<String>() {
                        if let Some(action_name) = map_atspi_action_name(&name) {
                            if !actions.contains(&action_name) {
                                indices.insert(action_name.clone(), i);
                                actions.push(action_name);
                            }
                        }
                    }
                }
            }
        }

        // NOTE: We do NOT add implicit "focus" based on Component interface existence.
        // On GTK4, GrabFocus() often returns false even when Component interface exists,
        // violating design tenet 3 ("if action is listed, calling it must work").
        // Only report "focus" if it's explicitly in the AT-SPI Action interface with a
        // proper index. This ensures focus() will work when reported.
        // Fixes GitHub issue #98.

        (actions, indices)
    }

    /// Return true when the accessible's application identifies itself as GTK
    /// via `org.a11y.atspi.Application.ToolkitName`.
    ///
    /// Used to scope the press-fallback heuristic for the
    /// AdwMenuButton/GtkMenuButton wrapper pattern; other toolkits are
    /// unaffected. Note that WebKitGTK also reports "GTK", so the answer is
    /// not a "GtkWidget app" discriminator.
    fn is_gtk_toolkit(&self, aref: &AccessibleRef) -> bool {
        let app_root = AccessibleRef {
            bus_name: aref.bus_name.clone(),
            path: "/org/a11y/atspi/accessible/root".to_string(),
        };
        self.make_proxy(
            &app_root.bus_name,
            &app_root.path,
            "org.a11y.atspi.Application",
        )
        .ok()
        .and_then(|proxy| proxy.get_property::<String>("ToolkitName").ok())
        .map(|t| t.eq_ignore_ascii_case("GTK"))
        .unwrap_or(false)
    }

    /// Refuse a text write the element's own state rules out, before
    /// calling into the toolkit.
    ///
    /// GTK 3 and others answer an `EditableText` write on a read-only field
    /// with success and change nothing, so the state is the only honest
    /// signal: `READ_ONLY` set, or `EDITABLE` clear, is
    /// [`Error::ActionNotSupported`] for `action`. A state read that fails
    /// is returned as-is rather than read as "writable".
    fn ensure_text_writable(
        &self,
        target: &AccessibleRef,
        element: &ElementData,
        action: &str,
    ) -> Result<()> {
        let bits = self.get_state(target).map(|words| {
            words
                .iter()
                .take(2)
                .enumerate()
                .fold(0u64, |acc, (i, w)| acc | ((*w as u64) << (32 * i)))
        })?;
        if text_writable(bits) {
            Ok(())
        } else {
            Err(Error::ActionNotSupported {
                action: action.to_string(),
                role: element.role,
            })
        }
    }

    /// The `length` argument for `EditableText.InsertText`, in the unit the
    /// target's toolkit reads it in.
    ///
    /// libatspi documents it in bytes, which is what at-spi2-atk (GTK 3)
    /// hands to `gtk_editable_insert_text`. Qt's adaptor instead resizes the
    /// `QString` to `length`, i.e. UTF-16 code units: a byte count pads
    /// non-ASCII text with NULs and a character count splits a surrogate
    /// pair (issue #493). The toolkit is read from
    /// `Application.ToolkitName`. Only a toolkit that genuinely does not
    /// expose that property (the reply says the property or interface is
    /// unknown) gets libatspi's documented byte count; any other failure to
    /// read it is returned, because guessing bytes for a Qt app is exactly
    /// the corruption this exists to prevent.
    fn insert_text_length(&self, target: &AccessibleRef, text: &str) -> Result<i32> {
        let proxy = self.make_proxy(
            &target.bus_name,
            "/org/a11y/atspi/accessible/root",
            "org.a11y.atspi.Application",
        )?;
        let toolkit = match proxy.get_property::<String>("ToolkitName") {
            Ok(name) => Some(name),
            Err(e) if is_property_absent(&e) => None,
            Err(e) => {
                return Err(Error::Platform {
                    code: -1,
                    message: format!(
                        "reading Application.ToolkitName of {} to size an InsertText \
                         failed: {e}",
                        target.bus_name
                    ),
                })
            }
        };
        Ok(insert_text_length_for(toolkit.as_deref(), text))
    }

    /// GTK press-fallback resolver.
    ///
    /// Walks the accessible's descendants (BFS, depth-capped) looking for a
    /// single activatable child that shares the outer widget's name. Returns
    /// the (ref, action_index) pair when exactly one suitable candidate
    /// exists at the shallowest matching depth. Returns `None` when the
    /// subtree contains nothing suitable or multiple equally plausible
    /// candidates — refusing to guess.
    ///
    /// Empirically fixes the `GtkMenuButton` / `AdwMenuButton` wrappers that
    /// ship in every stock GNOME app (Calculator, Text Editor, Logs, Clocks,
    /// Characters, …).
    fn find_gtk_press_fallback(
        &self,
        outer: &AccessibleRef,
        outer_name: &str,
    ) -> Option<(AccessibleRef, i32)> {
        let mut queue: VecDeque<(AccessibleRef, u32)> = VecDeque::new();
        queue.push_back((outer.clone(), 0));
        let mut visited: usize = 0;
        let mut shallowest_depth: Option<u32> = None;
        let mut hits: Vec<(AccessibleRef, i32)> = Vec::new();

        while let Some((node, depth)) = queue.pop_front() {
            // Once we've found the shallowest level with hits, don't look deeper.
            if let Some(best) = shallowest_depth {
                if depth > best {
                    continue;
                }
            }
            if visited > GTK_FALLBACK_MAX_NODES {
                break;
            }

            let role_name = if depth == 0 {
                String::new()
            } else {
                visited += 1;
                self.get_role_name(&node).unwrap_or_default().to_lowercase()
            };

            if depth > 0 && is_actionable_atspi_role(&role_name) {
                if let Some(idx) = self.gtk_fallback_pick(&node, outer_name) {
                    match shallowest_depth {
                        Some(d) if depth < d => {
                            shallowest_depth = Some(depth);
                            hits.clear();
                            hits.push((node.clone(), idx));
                        }
                        Some(d) if depth == d => hits.push((node.clone(), idx)),
                        Some(_) => {}
                        None => {
                            shallowest_depth = Some(depth);
                            hits.push((node.clone(), idx));
                        }
                    }
                }
            }

            // Do not descend into static/decorative roles. Descend through
            // containers and actionable roles alike (actionable roles may
            // themselves wrap an inner actionable — e.g. an AdwSplitButton's
            // primary button inside a toggle-button shell).
            let stop_descending = depth >= GTK_FALLBACK_MAX_DEPTH
                || (depth > 0 && is_never_descend_atspi_role(&role_name));
            if stop_descending {
                continue;
            }
            if let Ok(children) = self.get_atspi_children(&node) {
                for c in children {
                    queue.push_back((c, depth + 1));
                }
            }
        }

        if hits.len() == 1 {
            Some(hits.into_iter().next().unwrap())
        } else {
            None
        }
    }

    /// Per-node filter for `find_gtk_press_fallback`. Returns the AT-SPI
    /// action index to invoke when `aref` is a valid fallback candidate.
    fn gtk_fallback_pick(&self, aref: &AccessibleRef, outer_name: &str) -> Option<i32> {
        let (_, index_map) = self.get_actions(aref);
        let idx = *index_map.get("press")?;
        if !self.is_showing_visible_sensitive(aref) {
            return None;
        }
        // If the outer has a name, require the candidate to share it. Rules
        // out unrelated suffix widgets (e.g. a switch inside an AdwActionRow
        // that happens to expose `click`).
        if !outer_name.is_empty() {
            let inner_name = self.get_name(aref).unwrap_or_default();
            if !inner_name.is_empty() && inner_name != outer_name {
                return None;
            }
        }
        Some(idx)
    }

    /// True when the accessible's state has SHOWING, VISIBLE, and SENSITIVE
    /// set. ENABLED is deliberately excluded: in GTK4 it reflects "has an
    /// enabled GAction bound", which is false for the exact widgets we
    /// rescue.
    fn is_showing_visible_sensitive(&self, aref: &AccessibleRef) -> bool {
        let raw = self.get_state(aref).unwrap_or_default();
        let bits: u64 = if raw.len() >= 2 {
            (raw[0] as u64) | ((raw[1] as u64) << 32)
        } else if raw.len() == 1 {
            raw[0] as u64
        } else {
            0
        };
        const SENSITIVE: u64 = 1 << 24;
        const SHOWING: u64 = 1 << 25;
        const VISIBLE: u64 = 1 << 30;
        (bits & (SHOWING | VISIBLE | SENSITIVE)) == (SHOWING | VISIBLE | SENSITIVE)
    }

    /// Get value via the Text or Value interface, asking only for the ones
    /// the node reports (see [`Interfaces`] for why an unreported interface
    /// must not be read).
    fn get_value(&self, aref: &AccessibleRef, ifaces: Interfaces) -> Option<String> {
        // Try Text interface first for text content (text fields, labels, combo boxes).
        // This must come before Value because some AT-SPI adapters (e.g. AccessKit)
        // may expose both interfaces, and Value.CurrentValue returns 0.0 for text elements.
        if ifaces.text {
            let text_value = self.get_text_content(aref);
            if text_value.is_some() {
                return text_value;
            }
        }
        // Try Value interface (sliders, progress bars, scroll bars, spinners)
        if ifaces.value {
            if let Ok(proxy) = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Value") {
                if let Ok(val) = proxy.get_property::<f64>("CurrentValue") {
                    return Some(val.to_string());
                }
            }
        }
        None
    }

    /// Read text content via the AT-SPI Text interface.
    fn get_text_content(&self, aref: &AccessibleRef) -> Option<String> {
        let proxy = self
            .make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Text")
            .ok()?;
        let char_count: i32 = proxy.get_property("CharacterCount").ok()?;
        if char_count > 0 {
            let reply = proxy.call_method("GetText", &(0i32, char_count)).ok()?;
            let text: String = reply.body().deserialize().ok()?;
            if !text.is_empty() {
                return Some(text);
            }
        }
        None
    }

    /// Cache an AccessibleRef and return a new handle ID.
    fn cache_element(&self, aref: AccessibleRef) -> u64 {
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        self.handle_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle, aref);
        handle
    }

    /// Look up a cached AccessibleRef by handle.
    fn get_cached(&self, handle: u64) -> Result<AccessibleRef> {
        self.handle_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&handle)
            .cloned()
            .ok_or(Error::ElementStale {
                selector: format!("handle:{}", handle),
            })
    }

    /// Build an ElementData from an AccessibleRef, caching the ref for later lookup.
    ///
    /// After resolving the role (1-3 sequential D-Bus calls), all remaining
    /// property fetches are independent and run in parallel via rayon::join.
    fn build_element_data(&self, aref: &AccessibleRef, pid: Option<u32>) -> ElementData {
        // GetInterfaces runs alongside the role reads so it costs no extra
        // latency, and its result gates every Text / Value / Component read
        // below. A failed read gates them all off: like the other per-field
        // reads in this snapshot, an unreadable object degrades to absent
        // fields, and probing the interfaces anyway is exactly the unguarded
        // read issue #465 removed.
        let ((role_name, role_num), ifaces) = rayon::join(
            || {
                (
                    self.get_role_name(aref).unwrap_or_default(),
                    self.get_role_number(aref).unwrap_or(0),
                )
            },
            || self.get_interfaces(aref).unwrap_or_default(),
        );
        let coarse_role = self.refine_text_role(role_from_atspi(role_num, &role_name), aref);
        // GTK 3 overloads AT-SPI's `menu` role: both a popup menu container
        // and the MenuItem that opens a submenu report `menu`. The entry is
        // distinguishable by its native activation action (`click`, normalized
        // to `press`); GtkMenu itself has no Action interface. Fetch once here
        // so role refinement and the advertised action list use the same
        // evidence without a second D-Bus round trip.
        let prefetched_menu_actions = if coarse_role == Role::Menu {
            Some(self.get_actions(aref))
        } else {
            None
        };
        let role = refine_menu_role(
            coarse_role,
            prefetched_menu_actions
                .as_ref()
                .map(|(actions, _)| actions.as_slice())
                .unwrap_or(&[]),
        );

        // Fetch all independent properties in parallel.
        // Left tree: (name+value, description)
        // Right tree: ((states, bounds), (actions, numeric_values))
        let (
            ((mut name, value), description),
            (
                ((states, state_bits), bounds),
                ((actions, action_index_map), (numeric_value, min_value, max_value)),
            ),
        ) = rayon::join(
            || {
                rayon::join(
                    || {
                        let name = self.get_name(aref).ok().filter(|s| !s.is_empty());
                        let value = if role_has_value(role) {
                            self.get_value(aref, ifaces)
                        } else {
                            None
                        };
                        (name, value)
                    },
                    || self.get_description(aref).ok().filter(|s| !s.is_empty()),
                )
            },
            || {
                rayon::join(
                    || {
                        rayon::join(
                            || {
                                self.parse_states_with_bits(
                                    aref,
                                    role,
                                    is_password_role(&role_name, role_num),
                                )
                            },
                            || {
                                if role != Role::Application {
                                    self.get_extents(aref, ifaces)
                                } else {
                                    None
                                }
                            },
                        )
                    },
                    || {
                        rayon::join(
                            || {
                                if let Some(actions) = prefetched_menu_actions.as_ref() {
                                    actions.clone()
                                } else if role_has_actions(role) {
                                    self.get_actions(aref)
                                } else {
                                    (vec![], HashMap::new())
                                }
                            },
                            || {
                                if ifaces.value
                                    && matches!(
                                        role,
                                        Role::Slider
                                            | Role::ProgressBar
                                            | Role::ScrollBar
                                            | Role::SpinButton
                                    )
                                {
                                    if let Ok(proxy) = self.make_proxy(
                                        &aref.bus_name,
                                        &aref.path,
                                        "org.a11y.atspi.Value",
                                    ) {
                                        (
                                            proxy.get_property::<f64>("CurrentValue").ok(),
                                            proxy.get_property::<f64>("MinimumValue").ok(),
                                            proxy.get_property::<f64>("MaximumValue").ok(),
                                        )
                                    } else {
                                        (None, None, None)
                                    }
                                } else {
                                    (None, None, None)
                                }
                            },
                        )
                    },
                )
            },
        );

        // For label/static text elements, AT-SPI may put content in the Text
        // interface (returned as value) rather than the Name property.
        if name.is_none() && role == Role::StaticText {
            if let Some(ref v) = value {
                name = Some(v.clone());
            }
        }

        // `activate` is deliberately not advertised from AT-SPI alone. A real
        // native window backend may add it below after proving identity and
        // checking capabilities. The AT-SPI-only path is Component.GrabFocus
        // on the top-level frame, and Windows/macOS advertise theirs unconditionally,
        // but the AT-SPI adapters disagree about implementing a *frame*
        // GrabFocus: the GTK widget adapter answers true, while Chromium
        // (Electron), WebKitGTK (Tauri — whose bridge reports itself as
        // ToolkitName "GTK", so even a toolkit gate cannot separate it from
        // the GTK widget app) and Qt answer false. No interface or toolkit
        // probe discriminates them, and a snapshot-side GrabFocus probe
        // would activate windows as a side effect. Advertising anyway would
        // promise an action the call then declines — the false-promise shape
        // the integration suites exist to catch (round-4 CI: it failed the
        // activate suites on electron, tauri and qt; only the GTK cell passed).
        // The action stays callable on every toolkit, and the call reports a
        // `false` reply or a D-Bus NotSupported error honestly as
        // ActionNotSupported (tenet 3; tenet 1 — no silent fallback — is
        // why there is no substitute mechanism to advertise instead).

        let raw = {
            let raw_role = if role_name.is_empty() {
                format!("role_num:{}", role_num)
            } else {
                role_name
            };
            let mut raw = HashMap::new();
            raw.insert("atspi_role".into(), serde_json::Value::String(raw_role));
            raw.insert(
                "bus_name".into(),
                serde_json::Value::String(aref.bus_name.clone()),
            );
            raw.insert(
                "object_path".into(),
                serde_json::Value::String(aref.path.clone()),
            );
            // Preserve unstripped originals so callers who need bidi marks can
            // recover them after the strip below.
            if let Some(ref n) = name {
                raw.insert("atspi_name".into(), serde_json::Value::String(n.clone()));
            }
            if let Some(ref v) = value {
                raw.insert("atspi_value".into(), serde_json::Value::String(v.clone()));
            }
            if let Some(ref d) = description {
                raw.insert(
                    "atspi_description".into(),
                    serde_json::Value::String(d.clone()),
                );
            }
            // DEFUNCT (bit 6) marks a dead accessible — the object was
            // destroyed provider-side but a reference still resolves. It has
            // no cross-platform StateSet field, but it's the key diagnostic
            // for stale-reference bugs (e.g. a churning WebKit tree), so
            // surface it whenever set.
            if (state_bits & (1 << 6)) != 0 {
                raw.insert("atspi_defunct".into(), serde_json::Value::Bool(true));
            }
            raw
        };

        // Strip Unicode bidi format controls. RTL apps on Linux embed LRM/RLM
        // marks into reported strings; the originals are preserved in `raw`.
        let name = xa11y_core::text::strip_bidi_opt(name);
        let value = xa11y_core::text::strip_bidi_opt(value);
        let description = xa11y_core::text::strip_bidi_opt(description);

        let handle = self.cache_element(aref.clone());
        if !action_index_map.is_empty() {
            self.action_indices
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(handle, action_index_map);
        }

        let mut data: ElementData = ElementParts {
            role,
            name,
            value,
            description,
            bounds,
            actions,
            states,
            numeric_value,
            min_value,
            max_value,
            stable_id: Some(aref.path.clone()),
            pid,
            raw,
            handle,
        }
        .into();

        if matches!(role, Role::Window | Role::Dialog) && self.is_top_level_window_ref(aref) {
            let facts = self.window_manager.facts(&data);
            if facts.bounds.is_some() {
                data.bounds = facts.bounds;
            }
            for action in &facts.actions {
                if !data.actions.iter().any(|existing| existing == action) {
                    data.actions.push((*action).to_string());
                }
            }
            if facts.minimized.is_some() {
                data.states.minimized = facts.minimized;
            }
            if facts.maximized.is_some() {
                data.states.maximized = facts.maximized;
            }
            if facts.fullscreen.is_some() {
                data.states.fullscreen = facts.fullscreen;
            }
            if let Some(backend) = facts.backend {
                data.raw.insert(
                    "window_backend".to_string(),
                    serde_json::Value::String(backend.to_string()),
                );
            }
            if let Some(version) = facts.protocol_version {
                data.raw.insert(
                    "window_protocol_version".to_string(),
                    serde_json::Value::Number(version.into()),
                );
            }
            let capabilities = [
                "activate",
                "minimize",
                "maximize",
                "enter_fullscreen",
                "restore",
                "close",
                "move_to",
                "resize_to",
            ]
            .into_iter()
            .map(|action| {
                let status = if facts.backend.is_none() {
                    "unknown"
                } else if facts.actions.contains(&action) {
                    "supported"
                } else {
                    "unsupported"
                };
                (
                    action.to_string(),
                    serde_json::Value::String(status.to_string()),
                )
            })
            .collect();
            data.raw.insert(
                "window_capabilities".to_string(),
                serde_json::Value::Object(capabilities),
            );
        }

        data
    }

    /// Get the AT-SPI parent of an accessible ref.
    fn get_atspi_parent(&self, aref: &AccessibleRef) -> Result<Option<AccessibleRef>> {
        // Read the Parent property via the D-Bus Properties interface.
        let proxy = self.make_proxy(
            &aref.bus_name,
            &aref.path,
            "org.freedesktop.DBus.Properties",
        )?;
        let reply = proxy
            .call_method("Get", &("org.a11y.atspi.Accessible", "Parent"))
            .map_err(|e| {
                // `Parent` is a mandatory property of every AT-SPI accessible,
                // so a reply saying the member is absent is the object being
                // gone, not a bridge that skipped it: zbus answers a destroyed
                // object with `UnknownObject`, and GDBus (GTK 4) with
                // `UnknownMethod` "Object does not exist at path". Stale,
                // like `children_call_error`; every other failure stays one.
                if is_absent_member(&e) {
                    Error::ElementStale {
                        selector: format!("{}{}", aref.bus_name, aref.path),
                    }
                } else {
                    Error::Platform {
                        code: -1,
                        message: format!("Get Parent property failed: {}", e),
                    }
                }
            })?;
        // The reply is a Variant containing (so) — a struct of (bus_name, object_path)
        let variant: zbus::zvariant::OwnedValue =
            reply.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("Parent deserialize variant failed: {}", e),
            })?;
        let (bus, path): (String, zbus::zvariant::OwnedObjectPath) =
            zbus::zvariant::Value::from(variant).try_into().map_err(
                |e: zbus::zvariant::Error| Error::Platform {
                    code: -1,
                    message: format!("Parent deserialize struct failed: {}", e),
                },
            )?;
        let path_str = path.as_str();
        if path_str == "/org/a11y/atspi/null" || bus.is_empty() || path_str.is_empty() {
            return Ok(None);
        }
        // If the parent is the registry root, this is a top-level app — no parent
        if path_str == "/org/a11y/atspi/accessible/root" {
            return Ok(None);
        }
        Ok(Some(AccessibleRef {
            bus_name: bus,
            path: path_str.to_string(),
        }))
    }

    /// Fetch an accessible's AT-SPI2 state bitfield as a single `u64`.
    ///
    /// AT-SPI2 returns states as two `u32`s (low/high words); collapse them
    /// into one mask so callers can test bit positions directly.
    ///
    /// The `bool` reports whether `GetState` itself succeeded. A failed read
    /// is not "all states false": window-state optionals (`minimized`) must
    /// stay unknown rather than guess `false` (tenet 1), just as the rest of
    /// the window contract treats an unread state as `None`.
    fn state_bits(&self, aref: &AccessibleRef) -> (u64, bool) {
        match self.get_state(aref) {
            Ok(state_bits) if state_bits.len() >= 2 => (
                (state_bits[0] as u64) | ((state_bits[1] as u64) << 32),
                true,
            ),
            Ok(state_bits) if state_bits.len() == 1 => (state_bits[0] as u64, true),
            Ok(_) => (0, true),
            Err(_) => (0, false),
        }
    }

    /// Parse AT-SPI2 state bitfield into xa11y StateSet.
    ///
    /// Serves only the selector fast path, which never asks it for
    /// `password`: that flag comes from the raw AT-SPI role rather than the
    /// state bits, so `state_attr_to_string` has no arm for it and a
    /// `[password=..]` filter takes the full-`ElementData` route instead. The
    /// `false` passed here is therefore never read.
    fn parse_states(&self, aref: &AccessibleRef, role: Role) -> StateSet {
        self.parse_states_with_bits(aref, role, false).0
    }

    /// Like [`parse_states`], but also returns the raw AT-SPI state bitset so
    /// callers can surface provider-only facts (notably DEFUNCT) that have no
    /// cross-platform `StateSet` field.
    ///
    /// `password` is decided by the caller from the raw role (see
    /// [`is_password_role`]); the mapped `Role` cannot carry it, because a
    /// password field maps to `TextField` like any other entry.
    fn parse_states_with_bits(
        &self,
        aref: &AccessibleRef,
        role: Role,
        password: bool,
    ) -> (StateSet, u64) {
        let (bits, state_read_ok) = self.state_bits(aref);

        // AT-SPI2 state bit positions: each is `1 << ordinal` of the
        // `AtspiStateType` enum in at-spi2-core's `atspi/atspi-constants.h`
        // (ICONIFIED=15, MODAL=16, SELECTED=23, VISIBLE=30, ...). Kept in
        // sync with `states_from_bits` in events.rs.
        const ACTIVE: u64 = 1 << 1;
        const BUSY: u64 = 1 << 3;
        const CHECKED: u64 = 1 << 4;
        const EDITABLE: u64 = 1 << 7;
        const ENABLED: u64 = 1 << 8;
        const EXPANDABLE: u64 = 1 << 9;
        const EXPANDED: u64 = 1 << 10;
        const FOCUSABLE: u64 = 1 << 11;
        const FOCUSED: u64 = 1 << 12;
        const ICONIFIED: u64 = 1 << 15;
        const MODAL: u64 = 1 << 16;
        const SELECTED: u64 = 1 << 23;
        const SENSITIVE: u64 = 1 << 24;
        const INDETERMINATE: u64 = 1 << 32;
        const REQUIRED: u64 = 1 << 33;

        let enabled = (bits & ENABLED) != 0 || (bits & SENSITIVE) != 0;
        let visible = atspi_visible(bits);

        let checked = match role {
            Role::CheckBox | Role::RadioButton | Role::MenuItem | Role::Switch => {
                if (bits & INDETERMINATE) != 0 {
                    Some(Toggled::Mixed)
                } else if (bits & CHECKED) != 0 {
                    Some(Toggled::On)
                } else {
                    Some(Toggled::Off)
                }
            }
            _ => None,
        };

        let expanded = if (bits & EXPANDABLE) != 0 {
            Some((bits & EXPANDED) != 0)
        } else {
            None
        };

        let states = StateParts {
            enabled,
            visible,
            focused: (bits & FOCUSED) != 0,
            // AT-SPI `ACTIVE` marks the foreground window/frame (see
            // `focused_app`, which relies on the same bit).
            active: (bits & ACTIVE) != 0,
            checked,
            selected: (bits & SELECTED) != 0,
            expanded,
            editable: (bits & EDITABLE) != 0,
            focusable: (bits & FOCUSABLE) != 0,
            modal: (bits & MODAL) != 0,
            required: (bits & REQUIRED) != 0,
            busy: (bits & BUSY) != 0,
            // `ICONIFIED` is AT-SPI's minimized state, reported on window
            // frames. Maximized/fullscreen are not reported by AT-SPI at all
            // (no state bit), so they stay `None` — unknown, not guessed
            // `false` (tenet 1). macOS reads the fullscreen state from
            // AXFullScreen, but no platform can raise a
            // StateChanged{fullscreen} event: the AX API has no fullscreen
            // notification and UIA has no fullscreen state either. A failed
            // `GetState` must likewise leave `minimized` unknown — an unread
            // state is not a state that was read as absent.
            minimized: if matches!(role, Role::Window | Role::Dialog) {
                if state_read_ok {
                    Some((bits & ICONIFIED) != 0)
                } else {
                    None
                }
            } else {
                None
            },
            maximized: None,
            fullscreen: None,
            password,
        }
        .into();
        (states, bits)
    }

    /// Find an application by PID.
    ///
    /// Used by `subscribe` to resolve the target app's D-Bus sender name so
    /// signal match rules can be scoped to it.
    pub(crate) fn find_apps_by_pid(&self, pid: u32) -> Result<Vec<AccessibleRef>> {
        let mut matches = Vec::new();
        let registry = AccessibleRef {
            bus_name: "org.a11y.atspi.Registry".to_string(),
            path: "/org/a11y/atspi/accessible/root".to_string(),
        };
        let children = self.get_atspi_children(&registry)?;

        for child in &children {
            if child.path == "/org/a11y/atspi/null" {
                continue;
            }
            // Prefer D-Bus connection PID — it's authoritative for the
            // process owning the bus connection. Application.Id is *not* a
            // process pid: AT-SPI assigns it as a registry-local index
            // (1, 2, 3, …) for some bridges (notably GTK4), so matching
            // against Application.Id alone misses real processes.
            if let Some(app_pid) = self.get_dbus_pid(&child.bus_name) {
                if app_pid == pid {
                    matches.push(child.clone());
                    continue;
                }
            }
            // Fall back to Application.Id for adapters that do set it to pid.
            if let Ok(proxy) =
                self.make_proxy(&child.bus_name, &child.path, "org.a11y.atspi.Application")
            {
                if let Ok(app_pid) = proxy.get_property::<i32>("Id") {
                    if app_pid as u32 == pid {
                        matches.push(child.clone());
                    }
                }
            }
        }

        Ok(matches)
    }

    /// Get PID via D-Bus GetConnectionUnixProcessID.
    fn get_dbus_pid(&self, bus_name: &str) -> Option<u32> {
        let proxy = self
            .make_proxy(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            )
            .ok()?;
        let reply = proxy
            .call_method("GetConnectionUnixProcessID", &(bus_name,))
            .ok()?;
        let pid: u32 = reply.body().deserialize().ok()?;
        if pid > 0 {
            Some(pid)
        } else {
            None
        }
    }

    /// Fail with [`Error::ElementStale`] when `aref` is no longer attached to
    /// its parent.
    ///
    /// GTK 3 keeps a destroyed widget's accessible answering D-Bus calls for
    /// roughly 15–20 s, and `DoAction` on it returns `true` without doing
    /// anything (issue #491). Its parent stops counting it at once, so that is
    /// the check, at constant cost whatever the number of siblings:
    /// `GetIndexInParent` (-1 means detached), then the parent's
    /// `GetChildAtIndex` at that index must be this object. A parent that
    /// `MANAGES_DESCENDANTS` (large tables and tree views) does not keep its
    /// children as live objects, so there only the object's own `DEFUNCT`
    /// state is consulted. A top-level object (no parent, or the registry
    /// root) has nothing to be detached from and passes. An object or parent
    /// that is already gone is stale too; any other failure is returned.
    fn ensure_attached(&self, aref: &AccessibleRef) -> Result<()> {
        const DEFUNCT: u64 = 1 << 6;
        const MANAGES_DESCENDANTS: u64 = 1 << 31;
        let stale = || Error::ElementStale {
            selector: format!("{}{}", aref.bus_name, aref.path),
        };
        let parent = match self.get_atspi_parent(aref) {
            Ok(Some(parent)) => parent,
            Ok(None) => return Ok(()),
            Err(Error::ElementStale { .. }) => return Err(stale()),
            Err(e) => return Err(e),
        };
        let Some(parent_state) = self.scan_state_bits(&parent)? else {
            return Err(stale());
        };
        if parent_state & MANAGES_DESCENDANTS != 0 {
            return match self.scan_state_bits(aref)? {
                Some(bits) if bits & DEFUNCT == 0 => Ok(()),
                _ => Err(stale()),
            };
        }
        let Some(index) = self.attach_call::<i32>(aref, "GetIndexInParent", ())? else {
            return Err(stale());
        };
        if index < 0 {
            return Err(stale());
        }
        let Some((bus, path)) = self.attach_call::<(String, zbus::zvariant::OwnedObjectPath)>(
            &parent,
            "GetChildAtIndex",
            (index,),
        )?
        else {
            return Err(stale());
        };
        // A sibling inserted between the two calls can shift the index and
        // make this a spurious stale; a locator re-resolves and retries.
        if bus == aref.bus_name && path.as_str() == aref.path {
            Ok(())
        } else {
            Err(stale())
        }
    }

    /// One `org.a11y.atspi.Accessible` call for [`Self::ensure_attached`]:
    /// `Ok(None)` when the object or its peer is gone (the member is
    /// mandatory, so absence means the object is), a `Timeout` naming the app
    /// when it does not answer, and every other failure as an error.
    fn attach_call<R>(
        &self,
        aref: &AccessibleRef,
        method: &str,
        args: impl zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
    ) -> Result<Option<R>>
    where
        R: for<'d> zbus::export::serde::Deserialize<'d> + zbus::zvariant::Type,
    {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible")?;
        let reply = match proxy.call_method(method, &args) {
            Ok(reply) => reply,
            Err(e) if is_peer_gone(&e) || is_absent_member(&e) => return Ok(None),
            Err(e) if is_timeout(&e) => {
                return Err(self.unresponsive_peer(aref, &format!("Accessible.{method}")))
            }
            Err(e) => {
                return Err(Error::Platform {
                    code: -1,
                    message: format!("{method} on {}{} failed: {e}", aref.bus_name, aref.path),
                })
            }
        };
        reply
            .body()
            .deserialize::<R>()
            .map(Some)
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!(
                    "{method} reply from {}{} did not decode: {e}",
                    aref.bus_name, aref.path
                ),
            })
    }

    /// Perform an AT-SPI2 action by its integer index (from discovery).
    ///
    /// Checks first that the object is still attached to the tree
    /// ([`Self::ensure_attached`]): a destroyed GTK 3 widget would otherwise
    /// report the action as performed (issue #491).
    fn do_atspi_action_by_index(&self, aref: &AccessibleRef, index: i32) -> Result<()> {
        self.ensure_attached(aref)?;
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Action")?;
        let reply = proxy
            .call_method("DoAction", &(index,))
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("DoAction({}) failed: {}", index, e),
            })?;
        // AT-SPI's DoAction returns a boolean: true means the action ran. A
        // false reply means the provider declined — reporting Ok would let
        // activate()/focus() claim success while the window was never activated.
        let performed: bool = reply.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("DoAction({}) reply decode failed: {}", index, e),
        })?;
        if !performed {
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "DoAction({}) returned false; the accessibility provider declined the action",
                    index
                ),
            });
        }
        Ok(())
    }

    /// Look up the stored AT-SPI2 action index for the given element and action.
    fn get_action_index(&self, handle: u64, action: &str) -> Result<i32> {
        self.action_indices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&handle)
            .and_then(|map| map.get(action).copied())
            .ok_or_else(|| Error::ActionNotSupported {
                action: action.to_string(),
                role: Role::Unknown, // caller will provide better context
            })
    }

    /// Get the process PID for an AT-SPI accessible. Prefers the D-Bus
    /// connection PID over `Application.Id`: the latter is a registry-local
    /// index (1, 2, 3, …) on some bridges (e.g. GTK4), not a real pid, so
    /// reading it first produces apps whose `pid` property doesn't match the
    /// process the user launched.
    fn get_app_pid(&self, aref: &AccessibleRef) -> Option<u32> {
        if let Some(pid) = self.get_dbus_pid(&aref.bus_name) {
            return Some(pid);
        }

        // Fall back to Application.Id for adapters that set it to pid.
        // Require that a process with that id actually exists — a registry
        // index from a misbehaving bridge usually names a long-dead or absurd
        // pid. This is only a cheap sanity filter: a low index can still
        // collide with a real process (1 = init), which is why the D-Bus
        // connection pid above is authoritative whenever available.
        if let Ok(proxy) = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Application")
        {
            if let Ok(pid) = proxy.get_property::<i32>("Id") {
                if pid > 0 && std::path::Path::new(&format!("/proc/{pid}")).exists() {
                    return Some(pid as u32);
                }
            }
        }

        None
    }

    /// Resolve the mapped Role for an accessible ref (1-3 D-Bus calls, plus
    /// an Action probe for AT-SPI `menu`, whose meaning is ambiguous on GTK 3).
    fn resolve_role(&self, aref: &AccessibleRef) -> Role {
        let role_num = self.get_role_number(aref).unwrap_or(0);
        // The name is only consulted when the number does not map, so it is
        // not read at all on the common path.
        let coarse = match try_map_atspi_role_number(role_num) {
            Some(role) => role,
            None => role_from_atspi(role_num, &self.get_role_name(aref).unwrap_or_default()),
        };
        let role = self.refine_text_role(coarse, aref);
        if role == Role::Menu {
            let (actions, _) = self.get_actions(aref);
            refine_menu_role(role, &actions)
        } else {
            role
        }
    }

    /// Top-level probe: is `parent` the application node?
    ///
    /// Decided by the `GetRole` *number* alone (`ATSPI_ROLE_APPLICATION`),
    /// never the role name. GTK 4 names its top-level window `application`
    /// while numbering it a frame, so a name check took the window for the
    /// application and its own children for top-level windows (issue #485);
    /// AccessKit reports no role names at all. The number is the AT-SPI
    /// contract and every bridge sets it. A D-Bus failure is the caller's
    /// decision to make (propagated for the runtime guard).
    fn parent_role_is_application(&self, parent: &AccessibleRef) -> Result<bool> {
        Ok(self.get_role_number(parent)? == ATSPI_ROLE_APPLICATION)
    }

    /// Whether `aref` is an AT-SPI application object, by role *number*.
    ///
    /// Used to flatten nested application objects out of a window's
    /// children. The role name cannot be trusted for this: GTK 4 names its
    /// top-level window `application` while numbering it a frame, so a name
    /// check flattened the window itself away (issue #485). An unreadable
    /// role is not an application, matching the per-field tolerance of the
    /// surrounding traversal.
    fn is_application_object(&self, aref: &AccessibleRef) -> bool {
        self.get_role_number(aref).ok() == Some(ATSPI_ROLE_APPLICATION)
    }

    fn ensure_top_level_window_target(&self, element: &ElementData, action: &str) -> Result<()> {
        // AT-SPI window discovery is "direct child of the application node";
        // descendants with role=dialog are document structure, not OS windows.
        //
        // A Parent of the registry root (or the null address) is the AT-SPI
        // top-level shape: some adapters — AccessKit's, per the
        // `app_node_contract` integ test — report a top-level frame's Parent
        // there instead of on the application, and `get_atspi_parent` maps
        // it to `None` (see `get_parent`). `None` is therefore a
        // top-level-window answer, not a rejection: only an explicit
        // non-application parent (an in-page ARIA dialog's document ancestor)
        // means "not a real OS window".
        let target = self.get_cached(element.handle)?;
        let Some(parent) = self.get_atspi_parent(&target)? else {
            return Ok(());
        };
        // Same probe as the advertisement (`is_top_level_window_ref`), so a window
        // the snapshot advertised and the runtime guard can never disagree.
        // Both key on the role number, which GTK 4 and AccessKit report
        // correctly where their role names do not (#485).
        if self.parent_role_is_application(&parent)? {
            Ok(())
        } else {
            Err(Error::ActionNotSupported {
                action: action.to_string(),
                role: element.role,
            })
        }
    }

    fn is_top_level_window_ref(&self, target: &AccessibleRef) -> bool {
        match self.get_atspi_parent(target) {
            Ok(None) => true,
            Ok(Some(parent)) => self.parent_role_is_application(&parent).unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Check if an accessible ref matches a simple selector, fetching only the
    /// attributes the selector actually requires.
    ///
    /// Filter routing (cheapest first):
    ///   * `role` / `name` / `value` / `description` — single targeted D-Bus
    ///     call against this accessible.
    ///   * Normalized state attrs (`enabled`, `checked`, `focused`, …) —
    ///     one shared `GetState` call, cached for the rest of this match.
    ///   * Anything else (custom `raw` keys, `bounds`, numeric values) — fall
    ///     through to building a full `ElementData` and delegating to
    ///     [`xa11y_core::selector::matches_simple`]. This is slower but keeps
    ///     selectors with rare attribute filters semantically equivalent to
    ///     the default tree-traversal path.
    fn matches_ref(
        &self,
        aref: &AccessibleRef,
        simple: &xa11y_core::selector::SimpleSelector,
        pid: Option<u32>,
    ) -> bool {
        // Resolve role only if the selector needs it (for either the role
        // segment or any role/checked filter — checked depends on role).
        let needs_role = simple.role.is_some()
            || simple
                .filters
                .iter()
                .any(|f| matches!(f.attr.as_str(), "role" | "checked"));
        let role = if needs_role {
            Some(self.resolve_role(aref))
        } else {
            None
        };

        if let Some(ref role_match) = simple.role {
            match role_match {
                xa11y_core::selector::RoleMatch::Normalized(expected) => {
                    if role != Some(*expected) {
                        return false;
                    }
                }
                xa11y_core::selector::RoleMatch::Platform(platform_role) => {
                    let raw_role = self.get_role_name(aref).unwrap_or_default();
                    if raw_role != *platform_role {
                        return false;
                    }
                }
            }
        }

        let mut state_set: Option<StateSet> = None;
        // Fetched at most once per match, and only if a filter reads Text /
        // Value; a failed read gates both off, as in build_element_data.
        let mut ifaces: Option<Interfaces> = None;
        let mut value_of = |aref: &AccessibleRef| {
            let ifaces =
                *ifaces.get_or_insert_with(|| self.get_interfaces(aref).unwrap_or_default());
            self.get_value(aref, ifaces)
        };

        for filter in &simple.filters {
            let attr = filter.attr.as_str();
            let resolved: Option<Option<String>> = match attr {
                "role" => Some(role.map(|r| r.to_snake_case().to_string())),
                "name" => {
                    let name = self.get_name(aref).ok().filter(|s| !s.is_empty());
                    // Mirror build_element_data: StaticText carries its name
                    // in the Text interface's Value when Name is empty.
                    let resolved = if name.is_none() && role == Some(Role::StaticText) {
                        value_of(aref)
                    } else {
                        name
                    };
                    Some(resolved)
                }
                "value" => Some(value_of(aref)),
                "description" => Some(self.get_description(aref).ok().filter(|s| !s.is_empty())),
                "enabled" | "visible" | "focused" | "active" | "focusable" | "selected"
                | "editable" | "modal" | "required" | "busy" | "expanded" | "checked" => {
                    let s = state_set.get_or_insert_with(|| {
                        // `parse_states` reads `checked` based on role; pass
                        // whatever we already resolved (Unknown is a no-op for
                        // the role-gated `checked` mapping).
                        self.parse_states(aref, role.unwrap_or(Role::Unknown))
                    });
                    Some(state_attr_to_string(attr, s))
                }
                _ => None, // Routed through full ElementData below.
            };

            match resolved {
                Some(value) => {
                    if !xa11y_core::selector::match_op(&filter.op, &filter.value, value.as_deref())
                    {
                        return false;
                    }
                }
                None => {
                    // Filter targets an attribute the fast path doesn't know
                    // (e.g. `bounds`, `numeric_value`, a custom `raw` key).
                    // Build the full ElementData once and let the shared
                    // matcher handle every remaining filter — it dispatches
                    // to `ElementData` fields and the `raw` map identically
                    // to the default tree-traversal path.
                    let data = self.build_element_data(aref, pid);
                    return xa11y_core::selector::matches_simple(&data, simple);
                }
            }
        }

        true
    }

    /// ONE DFS over the subtree, evaluating each clause's first SimpleSelector
    /// against every visited node. Emits `(clause_idx, AccessibleRef)` pairs
    /// in document order; a node that matches multiple clauses is emitted
    /// once per matching clause (the merge step in `find_elements_group`
    /// collapses these by `(bus_name, path)` identity).
    ///
    /// This is the "push-down to the platform query per group" core: a
    /// selector group like `button, text_field` traverses the AT-SPI tree
    /// exactly once regardless of clause count, instead of N separate walks.
    fn collect_matching_refs_group(
        &self,
        parent: &AccessibleRef,
        clauses: &[&xa11y_core::selector::SimpleSelector],
        depth: u32,
        max_depth: u32,
        limit: Option<usize>,
        pid: Option<u32>,
    ) -> Result<Vec<(usize, AccessibleRef)>> {
        if depth > max_depth {
            return Ok(vec![]);
        }

        let Some(children) = self.live_atspi_children(parent)? else {
            return Ok(Vec::new());
        };

        // Filter invalid refs. Application-node flattening (collapsing a
        // redundant `application` accessible into its grandchildren) is only
        // valid when the parent is itself an application — i.e. we have already
        // descended past the registry. At the registry root, the children *are*
        // the applications and must not be collapsed, otherwise selectors like
        // `application` (used by `App::list` / `App::by_name`) match nothing
        // because the app accessibles get dissolved into their windows.
        let parent_is_registry = parent.bus_name == "org.a11y.atspi.Registry";
        let mut to_search: Vec<AccessibleRef> = Vec::new();
        for child in children {
            if child.path == "/org/a11y/atspi/null"
                || child.bus_name.is_empty()
                || child.path.is_empty()
            {
                continue;
            }

            if !parent_is_registry && self.is_application_object(&child) {
                let grandchildren = self.get_atspi_children(&child).unwrap_or_default();
                for gc in grandchildren {
                    if gc.path == "/org/a11y/atspi/null"
                        || gc.bus_name.is_empty()
                        || gc.path.is_empty()
                    {
                        continue;
                    }
                    if self.is_application_object(&gc) {
                        continue;
                    }
                    to_search.push(gc);
                }
                continue;
            }
            to_search.push(child);
        }

        // Detect Chromium/Electron's empty-tree signature whenever we
        // descend into a parent and find no real children.
        if to_search.is_empty() {
            self.check_chromium_a11y_enabled(parent, None)?;
        }

        // Process each child subtree in parallel: check match against every
        // clause + recurse. Tagging each match with `clause_idx` lets the
        // caller route phase-2 narrowing to the right clause without
        // re-running the first-segment match.
        //
        // We deliberately swallow transient sibling errors here — a single
        // flaky D-Bus call on one child shouldn't fail the whole locator
        // query (the rest of this file is similarly tolerant via
        // `unwrap_or_default()`). But `AccessibilityNotEnabled` is *not* a
        // transient error: it's the signal that a Chromium renderer bridge
        // isn't initialised, and callers need to see it. So we propagate
        // that variant specifically and keep tolerating everything else.
        // Per-child batches: matches collected during that child's subtree
        // walk, plus the first AccessibilityNotEnabled error (if any).
        type ChildBatch = (Vec<(usize, AccessibleRef)>, Option<Error>);
        let per_child: Vec<ChildBatch> = to_search
            .par_iter()
            .map(|child| {
                let mut child_results: Vec<(usize, AccessibleRef)> = Vec::new();
                for (idx, simple) in clauses.iter().enumerate() {
                    if self.matches_ref(child, simple, pid) {
                        child_results.push((idx, child.clone()));
                    }
                }
                match self.collect_matching_refs_group(
                    child,
                    clauses,
                    depth + 1,
                    max_depth,
                    limit,
                    pid,
                ) {
                    Ok(sub) => {
                        child_results.extend(sub);
                        (child_results, None)
                    }
                    Err(e @ Error::AccessibilityNotEnabled { .. }) => (Vec::new(), Some(e)),
                    Err(_) => (child_results, None),
                }
            })
            .collect();

        // Merge results, respecting limit. The first AccessibilityNotEnabled
        // error seen wins — any child subtree raising it means the whole
        // query is untrustworthy, so surface it rather than return partial
        // data.
        //
        // Note: `limit` is the *caller's* outer limit; for a multi-clause
        // group it must not be applied per-clause (a low-priority clause's
        // hit could come before a high-priority clause's in doc order, so
        // truncating mid-walk would drop legitimate matches). Callers pass
        // `None` for multi-clause queries and apply the limit after the
        // cross-clause merge.
        let mut results = Vec::new();
        for (batch, maybe_err) in per_child {
            if let Some(err) = maybe_err {
                return Err(err);
            }
            for r in batch {
                results.push(r);
                if let Some(limit) = limit {
                    if results.len() >= limit {
                        return Ok(results);
                    }
                }
            }
        }
        Ok(results)
    }
}

impl Provider for LinuxProvider {
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        match element {
            None => {
                // Top-level: delegate to `list_apps()`, the canonical
                // discovery primitive.
                self.list_apps()
            }
            Some(element_data) => {
                let aref = self.get_cached(element_data.handle)?;
                // A node destroyed after its snapshot has no live descendants.
                // Keep genuine D-Bus failures visible; do not run the Chromium
                // empty-tree diagnostic on a vanished window.
                let Some(children) = self.live_atspi_children(&aref)? else {
                    return Ok(Vec::new());
                };
                let pid = element_data.pid;

                // Pre-filter invalid refs and flatten nested application nodes,
                // collecting the concrete refs to build in parallel.
                let mut to_build: Vec<AccessibleRef> = Vec::new();
                for child_ref in &children {
                    if child_ref.path == "/org/a11y/atspi/null"
                        || child_ref.bus_name.is_empty()
                        || child_ref.path.is_empty()
                    {
                        continue;
                    }
                    if self.is_application_object(child_ref) {
                        // Nested application objects can also disappear between
                        // enumeration and this read. Prune only confirmed absence.
                        let Some(grandchildren) = self.live_atspi_children(child_ref)? else {
                            continue;
                        };
                        for gc_ref in grandchildren {
                            if gc_ref.path == "/org/a11y/atspi/null"
                                || gc_ref.bus_name.is_empty()
                                || gc_ref.path.is_empty()
                            {
                                continue;
                            }
                            if self.is_application_object(&gc_ref) {
                                continue;
                            }
                            to_build.push(gc_ref);
                        }
                        continue;
                    }
                    to_build.push(child_ref.clone());
                }

                if to_build.is_empty() {
                    self.check_chromium_a11y_enabled(&aref, Some(element_data.role))?;
                }

                let results: Vec<ElementData> = to_build
                    .par_iter()
                    .map(|r| self.build_element_data(r, pid))
                    .collect();

                Ok(results)
            }
        }
    }

    fn find_elements_group(
        &self,
        root: &ElementData,
        group: &xa11y_core::selector::SelectorGroup,
        limit: Option<usize>,
        max_depth: Option<u32>,
    ) -> Result<Vec<ElementData>> {
        // The AST's fields are public, so a hand-built group can break the
        // invariants the walk below indexes on (`clause.segments[0]`, Root
        // only first, 1-based `:nth`). Reject it before traversal.
        group.validate()?;

        // ONE AT-SPI walk that evaluates every clause's first SimpleSelector
        // against each visited node, then per-clause phase-2 narrowing on
        // the small candidate sets that fall out. Dedup uses
        // `(bus_name, path)` — the only AT-SPI identifier that's stable
        // across the per-clause narrowings (handles are minted fresh on
        // each `build_element_data` call).
        //
        // App discovery is handled separately by `list_apps()` — `root` is
        // always present here.
        let max_depth_val = max_depth.unwrap_or(xa11y_core::MAX_TREE_DEPTH);

        let firsts: Vec<&xa11y_core::selector::SimpleSelector> = group
            .clauses
            .iter()
            .map(|c| &c.segments[0].simple)
            .collect();

        let start_ref = self.get_cached(root.handle)?;

        // ── Phase-1 limit short-circuit ───────────────────────────
        // When there's exactly one clause, propagate the user's `limit`
        // (adjusted for `:nth`) to the AT-SPI walk so e.g.
        // `app.locator("button").first()` stops at the first match. With
        // multiple clauses, phase-1 hits for clause N might come before
        // clause M's in doc order, so we need the union before truncating.
        // (Critical on Electron-scale apps where this prevents 50k-node
        // walks.)
        let phase1_limit = if group.clauses.len() == 1 {
            let clause = &group.clauses[0];
            let first = firsts[0];
            let outer = if clause.segments.len() == 1 {
                limit
            } else {
                None
            };
            match (outer, first.nth) {
                (Some(l), Some(n)) => Some(l.max(n)),
                (_, Some(n)) => Some(n),
                (l, None) => l,
            }
        } else {
            None
        };

        let pid_from_root = root.pid;
        let phase1: Vec<(usize, AccessibleRef)> = self.collect_matching_refs_group(
            &start_ref,
            &firsts,
            0,
            max_depth_val,
            phase1_limit,
            pid_from_root,
        )?;

        // Bucket phase-1 hits by clause so each clause's tail can narrow
        // independently. `walk_pos` preserves the doc-order rank from the
        // single walk for the final merge.
        let mut by_clause: Vec<Vec<(usize, AccessibleRef)>> =
            (0..group.clauses.len()).map(|_| Vec::new()).collect();
        for (walk_pos, (clause_idx, aref)) in phase1.into_iter().enumerate() {
            by_clause[clause_idx].push((walk_pos, aref));
        }

        // For each clause, build ElementData for its phase-1 hits, then
        // narrow through any trailing segments. Single-segment clauses
        // skip phase 2 entirely.
        //
        // Each narrowed result keeps a (walk_pos, ElementData) pair: the
        // walk_pos of its phase-1 ancestor is the doc-order anchor we use
        // to merge across clauses at the end.
        let mut merged: Vec<(usize, ElementData)> = Vec::new();
        for (clause_idx, hits) in by_clause.into_iter().enumerate() {
            if hits.is_empty() {
                continue;
            }
            let clause = &group.clauses[clause_idx];
            // Build ElementData for this clause's phase-1 hits in parallel.
            let phase1_data: Vec<(usize, ElementData)> = hits
                .par_iter()
                .map(|(pos, aref)| {
                    // `root` is always present now, so the PID comes from
                    // the root element. No need to re-resolve via
                    // `get_app_pid` / `get_dbus_pid`.
                    (*pos, self.build_element_data(aref, pid_from_root))
                })
                .collect();

            if clause.segments.len() == 1 {
                // Apply :nth on the first segment (per-clause, then merged).
                let mut candidates = phase1_data;
                if let Some(nth) = clause.segments[0].simple.nth {
                    if nth <= candidates.len() {
                        let kept = candidates.remove(nth - 1);
                        candidates.clear();
                        candidates.push(kept);
                    } else {
                        candidates.clear();
                    }
                }
                merged.extend(candidates);
                continue;
            }

            // Multi-segment narrowing: re-use the trait helper. Each
            // narrowed result is anchored at the phase-1 ancestor's
            // walk_pos so the final sort puts it in doc order against
            // other clauses.
            for (anchor_pos, head) in phase1_data {
                let narrowed = self.narrow_multi_segment(
                    vec![head],
                    &clause.segments[1..],
                    max_depth_val,
                    None,
                )?;
                for n in narrowed {
                    merged.push((anchor_pos, n));
                }
            }
        }

        // Doc-order merge: stable sort by walk position. Within the same
        // walk position multiple clauses can have emitted the same node —
        // dedup by AT-SPI identity (`(bus_name, path)`) keeps the first.
        merged.sort_by_key(|(pos, _)| *pos);
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut out: Vec<ElementData> = Vec::with_capacity(merged.len());
        for (_, data) in merged {
            // Use the cached AccessibleRef for identity, not the (handle).
            // Handle is freshly minted per `build_element_data` call so it
            // would never collide here.
            if let Ok(aref) = self.get_cached(data.handle) {
                if !seen.insert((aref.bus_name.clone(), aref.path.clone())) {
                    continue;
                }
            }
            out.push(data);
        }
        if let Some(l) = limit {
            out.truncate(l);
        }
        Ok(out)
    }

    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
        let aref = self.get_cached(element.handle)?;
        match self.get_atspi_parent(&aref)? {
            Some(parent_ref) => {
                let data = self.build_element_data(&parent_ref, element.pid);
                Ok(Some(data))
            }
            None => Ok(None),
        }
    }

    /// Enumerate desktop panels and docks.
    ///
    /// A Linux shell surface is a top-level AT-SPI frame whose attribute set
    /// carries `window-type:dock` — how panels self-identify, from
    /// xfce4-panel's panel and dock rows to GNOME's and KDE's shells. One
    /// surface per such frame, so a panel process that owns two rows yields
    /// two [`ShellSurfaceKind::Panel`] entries, each with the panel process's
    /// pid. Panels are commonly unnamed; the frame's `name` is then `None` and
    /// the core falls back to the kind.
    ///
    /// `Panel` is the only kind Linux vends: the desktop has no menu bar,
    /// taskbar, or dock object of its own, and StatusNotifierItem *menus* live
    /// on `com.canonical.dbusmenu`, a different subsystem from AT-SPI. Their
    /// absence from the listing is honest scope, not failure.
    ///
    /// The scan runs over [`list_apps`](Self::list_apps) rather than its own
    /// registry walk, so it inherits that enumeration's empty-name filter: a
    /// panel that registers on the bus without a name is invisible here for
    /// the same reason it is invisible to `App::list`.
    ///
    /// # Errors
    ///
    /// A top-level that was just enumerated but will not answer `GetRole` or
    /// `GetAttributes` fails the listing (tenet 1). Those two calls are how a
    /// frame is classified, and `Panel` is the only kind Linux vends — so
    /// treating a D-Bus failure as "not a panel" turns one broken process into
    /// an empty listing that reads exactly like a desktop with no panels. The
    /// error names the app and object path that would not answer.
    ///
    /// An object that answers `GetAttributes` with `UnknownMethod` is the one
    /// exception, and it is absence rather than failure — see the call site.
    ///
    /// Enumerating one app's top-levels is the one step that still skips:
    /// [`get_atspi_children`](Self::get_atspi_children) failing is how a
    /// process that has left the bus presents, and `focused_app` already
    /// treats it that way.
    fn list_shell_surfaces(&self) -> Result<Vec<(ShellSurfaceKind, ElementData)>> {
        let mut surfaces = Vec::new();
        'apps: for app in self.list_apps()? {
            let app_ref = self.get_cached(app.handle)?;
            let top_levels = match self.get_atspi_children(&app_ref) {
                Ok(t) => t,
                // Same policy as `focused_app`: a single app failing to
                // enumerate its windows shouldn't abort the whole scan — skip
                // it and keep going.
                Err(_) => continue,
            };
            for top in top_levels
                .iter()
                .filter(|t| t.path != "/org/a11y/atspi/null")
            {
                // Both answer for a child the app has *just* enumerated, so a
                // D-Bus failure here is a failure, not absence — and "not a
                // panel" is the wrong reading of it (tenet 1). Say which
                // top-level of which app would not answer (tenet 6).
                //
                // `GetRole` rather than `GetRoleName` because every AT-SPI
                // object implements the numeric one, while AccessKit's bridge
                // (egui, and the other AccessKit-backed toolkits) implements
                // no `GetRoleName` at all — propagating that reply failed the
                // whole listing for a desktop running one such app.
                let Some(role_number) = self
                    .scan_role_number(top)
                    .map_err(|e| classify_failure(&app, top, e))?
                else {
                    // The application exited between `list_apps()` and this
                    // read. Its remaining top-levels are gone too, so move to
                    // the next application rather than re-asking a bus name
                    // that no longer has an owner.
                    continue 'apps;
                };
                // The same absence/failure split on the second read: an object
                // that does not implement `GetAttributes` cannot advertise
                // `window-type`, which is an honest "not a panel" — the answer
                // is in the reply, not missing from it. (AccessKit does serve
                // this one; the bridges that skip it are the reason the split
                // is here rather than only on the role.) Every other D-Bus
                // outcome — ServiceUnknown, NoReply, a timeout, a dropped
                // connection — still fails the listing.
                let Some(attributes) = self
                    .scan_attributes(top)
                    .map_err(|e| classify_failure(&app, top, e))?
                else {
                    continue 'apps;
                };
                let Some(attributes) = attributes else {
                    continue;
                };
                if is_dock_frame(role_number, &attributes) {
                    surfaces.push((
                        ShellSurfaceKind::Panel,
                        self.build_element_data(top, app.pid),
                    ));
                }
            }
        }
        Ok(surfaces)
    }

    /// The application's top-level children, minus windows that are not on
    /// screen in any sense.
    ///
    /// Qt keeps a hidden top-level dialog alive in the AT-SPI tree after
    /// `hide()`, with no native window behind it, so it was listed as a window
    /// the user cannot see (issue #492). A top-level window or dialog is
    /// dropped when its state set has neither `VISIBLE` nor `ICONIFIED`. A
    /// minimized window keeps `ICONIFIED`, and an unmapped one (minimized,
    /// or on another workspace) keeps `VISIBLE` without `SHOWING`; both stay
    /// listed. A window destroyed since the children read is gone and
    /// skipped; any other state-read failure fails the listing rather than
    /// guessing either way.
    fn app_windows(&self, app: &ElementData) -> Result<Vec<ElementData>> {
        let children = self.get_children(Some(app))?;
        let mut windows = Vec::with_capacity(children.len());
        for child in children {
            if matches!(child.role, Role::Window | Role::Dialog) {
                let aref = self.get_cached(child.handle)?;
                match self.scan_state_bits(&aref)? {
                    Some(bits) if is_listable_top_level(bits) => {}
                    // Hidden toolkit object, or destroyed since enumeration.
                    _ => continue,
                }
            }
            windows.push(child);
        }
        Ok(windows)
    }

    /// A single process can register several AT-SPI Application entries
    /// (per-process instance plus per-event-loop pieces), so `App::windows`
    /// must merge their window children by pid. The same-pid merge and the
    /// stable identities it deduplicates by live in `xa11y-core`.
    fn splits_app_across_entries(&self) -> bool {
        true
    }

    fn app_roots(&self, app: &ElementData) -> Result<Vec<ElementData>> {
        let Some(pid) = app.pid else {
            return Ok(vec![app.clone()]);
        };
        let mut roots: Vec<_> = self
            .list_apps()?
            .into_iter()
            .filter(|candidate| candidate.pid == Some(pid))
            .collect();
        if roots.is_empty() {
            roots.push(app.clone());
        }
        Ok(roots)
    }

    /// Enumerate top-level applications by listing direct children of the
    /// AT-SPI registry root — every running accessibility-enabled app
    /// registers a child accessible there. Apps with empty names are
    /// filtered out (toolkits sometimes register transient/system
    /// accessibles before the real app appears). PID resolution uses the
    /// AT-SPI `Application` interface, falling back to the D-Bus owner.
    fn list_apps(&self) -> Result<Vec<ElementData>> {
        let registry = AccessibleRef {
            bus_name: "org.a11y.atspi.Registry".to_string(),
            path: "/org/a11y/atspi/accessible/root".to_string(),
        };
        let children = self.get_atspi_children(&registry)?;

        // Read every application's name in parallel: each is a round trip to
        // a different process, and an unresponsive one costs a full
        // `ATSPI_METHOD_TIMEOUT`. An application that does not answer fails
        // the listing with an error naming it, rather than being dropped from
        // the list as if it were not running (tenet 1).
        let entries: Vec<&AccessibleRef> = children
            .iter()
            .filter(|c| c.path != "/org/a11y/atspi/null")
            .collect();
        let names: Vec<Option<String>> = entries
            .par_iter()
            .map(|c| self.read_app_name(c))
            .collect::<Result<_>>()?;
        let valid: Vec<(&AccessibleRef, String)> = entries
            .into_iter()
            .zip(names)
            .filter_map(|(c, name)| name.filter(|n| !n.is_empty()).map(|n| (c, n)))
            .collect();

        let results: Vec<ElementData> = valid
            .par_iter()
            .map(|(child, app_name)| {
                let pid = self.get_app_pid(child);
                let mut data = self.build_element_data(child, pid);
                data.name = Some(app_name.clone());
                data
            })
            .collect();

        Ok(results)
    }

    /// Find an application by pid directly against the AT-SPI registry.
    ///
    /// Differs from filtering `list_apps()` in two ways that matter for a
    /// freshly launched process:
    ///
    /// * No empty-name filter — toolkits can register on the bus before the
    ///   app name is set, and a pid match is precise enough on its own.
    /// * The not-matched error reports how many registry entries were
    ///   examined and how many had no resolvable pid (the misreported-pid
    ///   bridge quirk `get_app_pid` documents), so a timeout on CI says why
    ///   the app was invisible instead of just "not found".
    fn app_by_pid(&self, pid: u32) -> Result<ElementData> {
        let registry = AccessibleRef {
            bus_name: "org.a11y.atspi.Registry".to_string(),
            path: "/org/a11y/atspi/accessible/root".to_string(),
        };
        let children = self.get_atspi_children(&registry)?;

        let mut total = 0usize;
        let mut unresolved = 0usize;
        for child in children.iter().filter(|c| c.path != "/org/a11y/atspi/null") {
            total += 1;
            match self.get_app_pid(child) {
                Some(p) if p == pid => {
                    // Read the name before building: it is the one read that
                    // fails fast on a hung app. `build_element_data` tolerates
                    // per-field failures, so on an unresponsive peer it would
                    // wait out a method timeout for every field instead.
                    let name = self.read_app_name(child)?;
                    let mut data = self.build_element_data(child, Some(p));
                    // Mirror list_apps: prefer the registry-reported name.
                    if let Some(name) = name.filter(|n| !n.is_empty()) {
                        data.name = Some(name);
                    }
                    return Ok(data);
                }
                Some(_) => {}
                None => unresolved += 1,
            }
        }
        Err(
            Error::selector_not_matched(format!("application[pid={pid}]")).diagnose(
                xa11y_core::Diagnosis::new().last_observed(format!(
                    "{total} AT-SPI registry entries examined, {unresolved} without a \
                     resolvable pid"
                )),
            ),
        )
    }

    /// Identify the foreground application via the active top-level window.
    ///
    /// AT-SPI has no registry-level "focused application" call, and the
    /// `Application` accessibles themselves don't carry an active/focused
    /// state across toolkits. The portable signal is the `ACTIVE` state
    /// (`AtspiStateType::ACTIVE`) set on the foreground *window* (frame): we
    /// enumerate each registered application's top-level children and return
    /// the first application that owns a window reporting `ACTIVE`.
    ///
    /// Returns [`Error::SelectorNotMatched`] when no window is active (no app
    /// has the foreground — e.g. focus is on the shell or a non-AT-SPI
    /// surface). The core reads that as "nothing focused", not a failure.
    fn focused_app(&self) -> Result<ElementData> {
        // AtspiStateType::ACTIVE = 1 → bit position 1.
        const ACTIVE: u64 = 1 << 1;

        let registry = AccessibleRef {
            bus_name: "org.a11y.atspi.Registry".to_string(),
            path: "/org/a11y/atspi/accessible/root".to_string(),
        };
        let apps = self.get_atspi_children(&registry)?;

        for app in apps.iter().filter(|c| c.path != "/org/a11y/atspi/null") {
            // An application that left the bus mid-scan (or whose root has no
            // children to give) is skipped. One that does not answer is an
            // error naming it: skipping it would report "nothing focused"
            // when the hung app may be exactly the one in the foreground.
            let Some(windows) = self.scan_children(app)? else {
                continue;
            };
            let mut has_active_window = false;
            for w in windows.iter().filter(|w| w.path != "/org/a11y/atspi/null") {
                if self
                    .scan_state_bits(w)?
                    .is_some_and(|bits| bits & ACTIVE != 0)
                {
                    has_active_window = true;
                    break;
                }
            }
            if has_active_window {
                let Some(name) = self.read_app_name(app)? else {
                    // Gone between the state read and now.
                    continue;
                };
                let pid = self.get_app_pid(app);
                let mut data = self.build_element_data(app, pid);
                if !name.is_empty() {
                    data.name = Some(name);
                }
                return Ok(data);
            }
        }

        Err(Error::selector_not_matched("focused application"))
    }

    fn press(&self, element: &ElementData) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        // Fast path: the widget exposes `press` on its own Action interface.
        if let Ok(index) = self.get_action_index(element.handle, "press") {
            return self.do_atspi_action_by_index(&target, index);
        }
        // TENET-BREAK(1): substitute target. Human approval granted for this
        // break (see batch B6 / PR #125 / commit daeaf59). Justification: GTK4
        // menu-button widgets (GtkMenuButton, AdwMenuButton, AdwSplitButton)
        // expose their AT-SPI Action interface on an *inner* toggle-button
        // child, not the outer push-button accessible the app author addresses
        // by name. Without this workaround, `press()` silently does nothing on
        // every GtkMenuButton in every stock GNOME app (Calculator, Text
        // Editor, Logs, …). Alternatives considered and rejected:
        //   1. Expose the inner widget in the tree — breaks AT-SPI tree
        //      fidelity and leaks a GTK implementation detail into every
        //      consumer.
        //   2. Return ActionNotSupported — every GTK consumer would have to
        //      reimplement the same subtree search to work around it.
        // The break is narrowly scoped: gated on (a) the owning toolkit being
        // GTK and (b) the outer role being Role::Button (push-button, which is
        // what all three menu-button variants present as). Non-GtkMenuButton
        // roles and non-GTK toolkits continue to fail-fast with
        // ActionNotSupported.
        if element.role == Role::Button && self.is_gtk_toolkit(&target) {
            let outer_name = self.get_name(&target).unwrap_or_default();
            if let Some((inner, index)) = self.find_gtk_press_fallback(&target, &outer_name) {
                return self.do_atspi_action_by_index(&inner, index);
            }
        }
        Err(Error::ActionNotSupported {
            action: "press".to_string(),
            role: element.role,
        })
    }

    fn focus(&self, element: &ElementData) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        // The only mechanism is Component.GrabFocus, which returns a boolean
        // indicating success — we must check it. Fixes GitHub issue #98.
        //
        // There is deliberately no stored-action-index fallback: one cannot
        // fire. `action_indices` is populated only for roles in
        // `role_has_actions` (Window/Dialog excluded) from names
        // `map_atspi_action_name` produces, and no mapping yields "focus" (see
        // `get_actions`'s note: "focus" is only ever reported when explicitly
        // in the AT-SPI Action interface). Keeping the dead lookup here would
        // pretend to a second mechanism that never succeeds — the same
        // decision `activate` documents above (tenet 1).
        let proxy = self
            .make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Component")
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Component proxy while focusing failed: {}", e),
            })?;
        let reply = grab_focus_result(
            proxy.call_method("GrabFocus", &()),
            "focusing",
            "focus",
            element.role,
        )?;
        let grabbed: bool = reply.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("Component.GrabFocus reply while focusing failed: {}", e),
        })?;
        if grabbed {
            return Ok(());
        }
        // GrabFocus(false) is an honest "cannot focus this element" — the
        // AT-SPI adapter declined, not a platform failure.
        Err(Error::ActionNotSupported {
            action: "focus".to_string(),
            role: element.role,
        })
    }

    fn blur(&self, element: &ElementData) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        // Grab focus on parent element to blur the current one. We propagate
        // every failure — no silent fallbacks (tenet 1): callers need to see
        // when blur can't do anything useful.
        if let Some(parent_ref) = self.get_atspi_parent(&target)? {
            if parent_ref.path != "/org/a11y/atspi/null" {
                let p = self.make_proxy(
                    &parent_ref.bus_name,
                    &parent_ref.path,
                    "org.a11y.atspi.Component",
                )?;
                p.call_method("GrabFocus", &())
                    .map_err(|e| Error::Platform {
                        code: -1,
                        message: format!("Component.GrabFocus on parent failed: {}", e),
                    })?;
                return Ok(());
            }
        }
        Err(Error::ActionNotSupported {
            action: "blur".to_string(),
            role: element.role,
        })
    }

    fn toggle(&self, element: &ElementData) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        // Fast path: the widget exposes the literal AT-SPI action name "toggle"
        // (or one of its toolkit aliases — "check" / "uncheck" — both
        // canonicalised to "toggle" by map_atspi_action_name during cache
        // population).
        if let Ok(index) = self.get_action_index(element.handle, "toggle") {
            return self.do_atspi_action_by_index(&target, index);
        }
        // Fallback for AccessKit-backed apps. accesskit_unix's AT-SPI bridge
        // hard-codes the action name as "click" for every clickable node
        // regardless of role — see platforms/atspi-common/src/node.rs in
        // AccessKit. That means egui, eframe, winit-based apps, Bevy, Tauri
        // (when using AccessKit) and any other AccessKit consumer never
        // advertise "toggle" for CheckBox/RadioButton/Switch, even though
        // activating those widgets via "click" is functionally a toggle on
        // every platform. Without this fallback, xa11y.toggle() is
        // unreachable on every AccessKit-on-Linux widget.
        //
        // This is tenet-3-consistent: the semantic verb `toggle` on a
        // toggleable role means "activate this toggleable element," and on
        // Linux the canonical AccessKit implementation of that is the same
        // action node entry we'd dispatch for press(). It's the mirror image
        // of the Windows `press` dispatch (uia.rs) which fans out to Invoke,
        // Toggle, SelectionItem.Select, or ExpandCollapse depending on the
        // element's primary-activation pattern.
        //
        // Narrowly scoped: only fires for toggleable primary-activation
        // roles. Other roles still fail-fast with ActionNotSupported so a
        // misuse of toggle() on, e.g., a Button does not silently dispatch
        // press().
        if matches!(
            element.role,
            Role::CheckBox | Role::RadioButton | Role::Switch
        ) {
            if let Ok(index) = self.get_action_index(element.handle, "press") {
                return self.do_atspi_action_by_index(&target, index);
            }
        }
        Err(Error::ActionNotSupported {
            action: "toggle".to_string(),
            role: element.role,
        })
    }

    fn select(&self, element: &ElementData) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        let index = self
            .get_action_index(element.handle, "select")
            .map_err(|_| Error::ActionNotSupported {
                action: "select".to_string(),
                role: element.role,
            })?;
        self.do_atspi_action_by_index(&target, index)
    }

    fn expand(&self, element: &ElementData) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        let index = self
            .get_action_index(element.handle, "expand")
            .map_err(|_| Error::ActionNotSupported {
                action: "expand".to_string(),
                role: element.role,
            })?;
        self.do_atspi_action_by_index(&target, index)
    }

    fn collapse(&self, element: &ElementData) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        let index = self
            .get_action_index(element.handle, "collapse")
            .map_err(|_| Error::ActionNotSupported {
                action: "collapse".to_string(),
                role: element.role,
            })?;
        self.do_atspi_action_by_index(&target, index)
    }

    fn show_menu(&self, element: &ElementData) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        let index = self
            .get_action_index(element.handle, "show_menu")
            .map_err(|_| Error::ActionNotSupported {
                action: "show_menu".to_string(),
                role: element.role,
            })?;
        self.do_atspi_action_by_index(&target, index)
    }

    fn increment(&self, element: &ElementData) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        // Try stored AT-SPI2 action index first, fall back to Value interface
        if let Ok(index) = self.get_action_index(element.handle, "increment") {
            return self.do_atspi_action_by_index(&target, index);
        }
        let proxy = self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Value")?;
        let current: f64 = proxy
            .get_property("CurrentValue")
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Value.CurrentValue failed: {}", e),
            })?;
        let step: f64 = proxy.get_property("MinimumIncrement").unwrap_or(1.0);
        let step = if step <= 0.0 { 1.0 } else { step };
        proxy
            .set_property("CurrentValue", current + step)
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Value.SetCurrentValue failed: {}", e),
            })
    }

    fn decrement(&self, element: &ElementData) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        if let Ok(index) = self.get_action_index(element.handle, "decrement") {
            return self.do_atspi_action_by_index(&target, index);
        }
        let proxy = self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Value")?;
        let current: f64 = proxy
            .get_property("CurrentValue")
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Value.CurrentValue failed: {}", e),
            })?;
        let step: f64 = proxy.get_property("MinimumIncrement").unwrap_or(1.0);
        let step = if step <= 0.0 { 1.0 } else { step };
        proxy
            .set_property("CurrentValue", current - step)
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Value.SetCurrentValue failed: {}", e),
            })
    }

    fn scroll_into_view(&self, element: &ElementData) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        let proxy = self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Component")?;
        proxy
            .call_method("ScrollTo", &(0u32,))
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("ScrollTo failed: {}", e),
            })?;
        Ok(())
    }

    // ── Window management ──────────────────────────────────────────
    //
    // Native X11/Sway window-manager requests handle the verbs they advertise.
    // On Wayland compositors without a native provider, AT-SPI still exposes
    // `activate` (Component.GrabFocus on the frame) and two geometry setters:
    // SetPosition and SetSize. There is no simulated-shortcut path.
    //
    // Window discovery is `App::windows` — `get_children(app)` filtered to
    // Window|Dialog — as on every platform.

    fn activate(&self, element: &ElementData) -> Result<()> {
        // The AT-SPI implementation of "activate this window" is the same
        // Component.GrabFocus path `focus` uses: the frame that receives
        // focus becomes the active window. No separate activate API exists.
        //
        // Implemented here rather than delegating to `focus` so a failure
        // reports the verb the caller requested — `activate` — not `focus`'s
        // name (the two share a mechanism, not an identity).
        //
        // A GrabFocus(false) reply is an honest "cannot activate this window",
        // reported as ActionNotSupported: the action-index fallback that
        // `focus` carries cannot fire for window-like roles, because
        // `action_indices` is populated only for roles in `role_has_actions`
        // (Window/Dialog excluded) and `map_atspi_action_name` maps no
        // "focus" action name. Unlike Windows/macOS, `activate` is deliberately
        // NOT advertised on AT-SPI — the adapters disagree about a frame
        // GrabFocus and no interface probe discriminates them (see the
        // reasons in `build_element_data`); advertised or not, the call here
        // and the `focus` call use the same mechanism, which is why the
        // deleted `focus` lookup would only ever pretend to a second path
        // (tenet 1). If GTK4's GrabFocus(false) is ever reproduced with an
        // actual focus-named Action interface entry (issue #98), the right
        // fix is a direct Action probe on this failure path, not a cache
        // lookup the role/name mapping rules can never fill.
        self.ensure_top_level_window_target(element, "activate")?;
        if self.window_manager.uses_native() {
            return self.window_manager.activate(element);
        }
        let target = self.get_cached(element.handle)?;
        let proxy = self
            .make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Component")
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Component proxy while activating failed: {}", e),
            })?;
        let reply = grab_focus_result(
            proxy.call_method("GrabFocus", &()),
            "activating",
            "activate",
            element.role,
        )?;
        let grabbed: bool = reply.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("Component.GrabFocus reply while activating failed: {}", e),
        })?;
        if grabbed {
            return Ok(());
        }
        Err(Error::ActionNotSupported {
            action: "activate".to_string(),
            role: element.role,
        })
    }

    fn minimize(&self, element: &ElementData) -> Result<()> {
        self.ensure_top_level_window_target(element, "minimize")?;
        self.window_manager.minimize(element)
    }

    fn maximize(&self, element: &ElementData) -> Result<()> {
        self.ensure_top_level_window_target(element, "maximize")?;
        self.window_manager.maximize(element)
    }

    fn enter_fullscreen(&self, element: &ElementData) -> Result<()> {
        self.ensure_top_level_window_target(element, "enter_fullscreen")?;
        self.window_manager.enter_fullscreen(element)
    }

    fn restore(&self, element: &ElementData) -> Result<()> {
        self.ensure_top_level_window_target(element, "restore")?;
        self.window_manager.restore(element)
    }

    fn close(&self, element: &ElementData) -> Result<()> {
        self.ensure_top_level_window_target(element, "close")?;
        self.window_manager.close(element)
    }

    fn move_to(&self, element: &ElementData, x: i32, y: i32) -> Result<()> {
        self.ensure_top_level_window_target(element, "move_to")?;
        if self.window_manager.uses_native() {
            return self.window_manager.move_to(element, x, y);
        }
        let target = self.get_cached(element.handle)?;
        let proxy = self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Component")?;
        let scale = crate::scale::coordinate_scale();
        let point = xa11y_core::Point::new(x, y).to_physical(scale);
        // coord_type 0 is screen coordinates, matching GetExtents above.
        let reply = proxy
            .call_method("SetPosition", &(point.x, point.y, 0u32))
            .map_err(|e| {
                if is_absent_member(&e) {
                    Error::ActionNotSupported {
                        action: "move_to".to_string(),
                        role: element.role,
                    }
                } else {
                    Error::Platform {
                        code: -1,
                        message: format!("Component.SetPosition while moving window failed: {e}"),
                    }
                }
            })?;
        let moved: bool = reply.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("Component.SetPosition reply while moving window failed: {e}"),
        })?;
        if moved {
            Ok(())
        } else {
            Err(Error::ActionNotSupported {
                action: "move_to".to_string(),
                role: element.role,
            })
        }
    }

    fn resize_to(&self, element: &ElementData, w: u32, h: u32) -> Result<()> {
        self.ensure_top_level_window_target(element, "resize_to")?;
        if self.window_manager.uses_native() {
            return self.window_manager.resize_to(element, w, h);
        }
        let target = self.get_cached(element.handle)?;
        let proxy = self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Component")?;
        let physical = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        }
        .to_physical(crate::scale::coordinate_scale());
        let width = i32::try_from(physical.width).map_err(|_| Error::InvalidActionData {
            message: format!("resize width {w} is too large for AT-SPI"),
        })?;
        let height = i32::try_from(physical.height).map_err(|_| Error::InvalidActionData {
            message: format!("resize height {h} is too large for AT-SPI"),
        })?;
        let reply = proxy
            .call_method("SetSize", &(width, height))
            .map_err(|e| {
                if is_absent_member(&e) {
                    Error::ActionNotSupported {
                        action: "resize_to".to_string(),
                        role: element.role,
                    }
                } else {
                    Error::Platform {
                        code: -1,
                        message: format!("Component.SetSize while resizing window failed: {e}"),
                    }
                }
            })?;
        let resized: bool = reply.body().deserialize().map_err(|e| Error::Platform {
            code: -1,
            message: format!("Component.SetSize reply while resizing window failed: {e}"),
        })?;
        if resized {
            Ok(())
        } else {
            Err(Error::ActionNotSupported {
                action: "resize_to".to_string(),
                role: element.role,
            })
        }
    }

    fn set_value(&self, element: &ElementData, value: &str) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        self.ensure_text_writable(&target, element, "set_value")?;
        let proxy = self
            .make_proxy(
                &target.bus_name,
                &target.path,
                "org.a11y.atspi.EditableText",
            )
            .map_err(|_| Error::TextValueNotSupported)?;
        // SetTextContents is the one-call replace. A bridge whose
        // EditableText lacks just that method answers UnknownMethod — an
        // answer about the object, not a failure — and only then is the value
        // written as DeleteText + InsertText. An absent interface
        // (UnknownInterface) is TextValueNotSupported straight away. Every other outcome of SetTextContents,
        // including a FALSE reply, is the result of the write (tenet 1).
        match proxy.call_method("SetTextContents", &(value,)) {
            Ok(reply) => {
                return atspi_bool_reply(reply, "EditableText.SetTextContents", &target, element);
            }
            Err(zbus::Error::MethodError(name, ..))
                if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" => {}
            Err(e) => return Err(editable_text_call_error("SetTextContents", e, &target)),
        }
        let reply = proxy
            .call_method("DeleteText", &(0i32, i32::MAX))
            .map_err(|e| editable_text_call_error("DeleteText", e, &target))?;
        atspi_bool_reply(reply, "EditableText.DeleteText", &target, element)?;
        // Not atomic: if InsertText fails after DeleteText succeeded, the
        // field is left empty, and the error says which call failed.
        let length = self.insert_text_length(&target, value)?;
        let reply = proxy
            .call_method("InsertText", &(0i32, value, length))
            .map_err(|e| editable_text_call_error("InsertText", e, &target))?;
        atspi_bool_reply(reply, "EditableText.InsertText", &target, element)
    }

    fn set_numeric_value(&self, element: &ElementData, value: f64) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        let proxy = self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Value")?;
        proxy
            .set_property("CurrentValue", value)
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("SetValue failed: {}", e),
            })
    }

    fn type_text(&self, element: &ElementData, text: &str) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        self.ensure_text_writable(&target, element, "type_text")?;
        // Insert text via EditableText interface (accessibility API, not input simulation).
        // Get cursor position from Text interface, then insert at that position.
        // The caret offset comes back in the toolkit's own offset unit and
        // goes straight back into InsertText, so it needs no conversion.
        let text_proxy = self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Text");
        let insert_pos = text_proxy
            .as_ref()
            .ok()
            .and_then(|p| p.get_property::<i32>("CaretOffset").ok())
            .unwrap_or(-1); // -1 = append at end

        let proxy = self
            .make_proxy(
                &target.bus_name,
                &target.path,
                "org.a11y.atspi.EditableText",
            )
            .map_err(|_| Error::TextValueNotSupported)?;
        let pos = if insert_pos >= 0 {
            insert_pos
        } else {
            i32::MAX
        };
        let length = self.insert_text_length(&target, text)?;
        let reply = proxy
            .call_method("InsertText", &(pos, text, length))
            .map_err(|e| editable_text_call_error("InsertText", e, &target))?;
        atspi_bool_reply(reply, "EditableText.InsertText", &target, element)
    }

    fn set_text_selection(&self, element: &ElementData, start: u32, end: u32) -> Result<()> {
        // Units: `start`/`end` are passed through in whatever offset unit the
        // toolkit uses (characters for GTK, UTF-16 code units for Qt). The
        // public offset contract is #477; this only stops a FALSE reply from
        // reading as success.
        let target = self.get_cached(element.handle)?;
        let proxy = self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Text")?;
        let reply = proxy
            .call_method("SetSelection", &(0i32, start as i32, end as i32))
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Text.SetSelection failed: {}", e),
            })?;
        atspi_bool_reply(
            reply,
            &format!("Text.SetSelection({start}, {end})"),
            &target,
            element,
        )
    }

    fn perform_action(&self, element: &ElementData, action: &str) -> Result<()> {
        match action {
            "press" => self.press(element),
            "focus" => self.focus(element),
            "blur" => self.blur(element),
            "toggle" => self.toggle(element),
            "select" => self.select(element),
            "expand" => self.expand(element),
            "collapse" => self.collapse(element),
            "show_menu" => self.show_menu(element),
            "increment" => self.increment(element),
            "decrement" => self.decrement(element),
            "scroll_into_view" => self.scroll_into_view(element),
            "activate" => self.activate(element),
            "minimize" => self.minimize(element),
            "maximize" => self.maximize(element),
            "enter_fullscreen" => self.enter_fullscreen(element),
            "restore" => self.restore(element),
            "close" => self.close(element),
            // Payload verbs have no arguments on the generic escape hatch;
            // fail surfaceably with how to call them instead of guessing
            // (tenet 1: no silent fallback).
            "move_to" => Err(Error::InvalidActionData {
                message: "perform_action(\"move_to\") requires coordinates; call move_to(x, y)"
                    .to_string(),
            }),
            "resize_to" => Err(Error::InvalidActionData {
                message: "perform_action(\"resize_to\") requires dimensions; call \
                           resize_to(width, height)"
                    .to_string(),
            }),
            _ => Err(Error::ActionNotSupported {
                action: action.to_string(),
                role: element.role,
            }),
        }
    }

    fn subscribe(&self, element: &ElementData) -> Result<Subscription> {
        let pid = element.pid.ok_or(Error::Platform {
            code: -1,
            message: "Element has no PID for subscribe".to_string(),
        })?;
        let app_name = element.name.clone().unwrap_or_default();
        crate::events::subscribe_for_pid(self, pid, app_name)
    }
}

/// Whether a role typically has text or Value interface content.
/// Container/structural roles are skipped to save D-Bus round-trips.
fn role_has_value(role: Role) -> bool {
    !matches!(
        role,
        Role::Application
            | Role::Window
            | Role::Dialog
            | Role::Group
            | Role::MenuBar
            | Role::Toolbar
            | Role::TabGroup
            | Role::SplitGroup
            | Role::Table
            | Role::TableRow
            | Role::Separator
    )
}

/// Whether a role typically supports actions via the Action interface.
/// Container and display-only roles are skipped to save D-Bus round-trips.
///
/// Window and Dialog are deliberately excluded: their semantic verbs do not
/// come from the Action interface. `activate` (the one Linux supports) is also
/// deliberately NOT advertised — see the note in `build_element_data` on why
/// (the AT-SPI adapters disagree about implementing a frame GrabFocus, so
/// advertising it would promise an action the call then declines); it stays
/// callable, and the state-mutating verbs are `Unsupported` on AT-SPI — so
/// admitting them here would only earn a pointless GetActions probe
/// returning no usable entries.
fn role_has_actions(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::CheckBox
            | Role::RadioButton
            | Role::MenuItem
            | Role::Link
            | Role::ComboBox
            | Role::TextField
            | Role::TextArea
            | Role::SpinButton
            | Role::Tab
            | Role::TreeItem
            | Role::ListItem
            | Role::ScrollBar
            | Role::Slider
            | Role::Menu
            | Role::Image
            | Role::Unknown
    )
}

/// Classify a `Component.GrabFocus` call result, used by
/// [`focus`](LinuxProvider::focus) and [`activate`](LinuxProvider::activate).
///
/// The two verbs share the AT-SPI mechanism (there is no separate activate API),
/// and an adapter signals "this element cannot take focus" in one of two
/// honest shapes: a `false` reply (checked by both callers) or the D-Bus
/// `NotSupported` error, which is what the GTK4 adapter raises for
/// non-focusable controls instead of answering false (issue #98). Both mean
/// the adapter declined — `ActionNotSupported`, the documented contract that
/// the python/JS suites treat as xfail — and every other failure stays a
/// `Platform` error (tenet 1: no silent swallowing of unexpected failures).
fn grab_focus_result(
    call: std::result::Result<zbus::Message, zbus::Error>,
    verb: &str,
    action: &str,
    role: Role,
) -> Result<zbus::Message> {
    call.map_err(|e| match e {
        zbus::Error::MethodError(name, _, _) => {
            grab_focus_error(name.as_str().to_string(), verb, action, role)
        }
        other => Error::Platform {
            code: -1,
            message: format!("Component.GrabFocus while {verb} failed: {other}"),
        },
    })
}

/// Pure classification half of `grab_focus_result`, unit-testable without
/// constructing zbus internals.
fn grab_focus_error(name: String, verb: &str, action: &str, role: Role) -> Error {
    if name == "org.freedesktop.DBus.Error.NotSupported" {
        Error::ActionNotSupported {
            action: action.to_string(),
            role,
        }
    } else {
        Error::Platform {
            code: -1,
            message: format!("Component.GrabFocus while {verb} failed: {name}"),
        }
    }
}

/// Map an accessible's AT-SPI role to an xa11y [`Role`], keyed on the
/// `GetRole` *number*.
///
/// The number is the AT-SPI contract; the name is a display string, and
/// toolkits disagree with themselves there: GTK 4 names its window
/// `application`, containers `generic` and entries `text box` while
/// numbering them frame, filler/panel and text. Reading the name first made
/// GTK 4 windows nested applications that were then flattened out of the
/// tree (issue #485). The name is consulted only when the number does not
/// map — AccessKit reports no names at all, and a few bridges report a
/// number this table does not know — and both unmapped is
/// [`xa11y_core::unknown_role`] naming both. Refinements that need more than
/// the role (GTK 3 `menu` vs menu item, text field vs area) run after this.
pub(crate) fn role_from_atspi(role_num: u32, role_name: &str) -> Role {
    if let Some(role) = try_map_atspi_role_number(role_num) {
        return role;
    }
    if !role_name.is_empty() {
        if let Some(role) = try_map_atspi_role(role_name) {
            return role;
        }
    }
    // `AtspiRole::Unknown` with nothing better in the name is the platform
    // answering "unknown", not a role this table has yet to map.
    if role_num == ATSPI_ROLE_UNKNOWN {
        return Role::Unknown;
    }
    xa11y_core::unknown_role(&format!(
        "AT-SPI role number {role_num}, role name {role_name:?}"
    ))
}

/// Map AT-SPI2 role name to xa11y Role.
/// Test-only: production code goes through [`role_from_atspi`].
#[cfg(test)]
pub(crate) fn map_atspi_role(role_name: &str) -> Role {
    try_map_atspi_role(role_name).unwrap_or_else(|| xa11y_core::unknown_role(role_name))
}

/// [`map_atspi_role`] without the unknown-role arm: `None` for a name the
/// table does not know.
fn try_map_atspi_role(role_name: &str) -> Option<Role> {
    Some(match role_name.to_lowercase().as_str() {
        "application" => Role::Application,
        "window" | "frame" => Role::Window,
        "dialog" | "file chooser" => Role::Dialog,
        "alert" | "notification" | "info bar" => Role::Alert,
        "push button" | "push button menu" => Role::Button,
        "toggle button" => Role::Switch,
        "check box" | "check menu item" => Role::CheckBox,
        "radio button" | "radio menu item" => Role::RadioButton,
        "entry" | "password text" => Role::TextField,
        "spin button" | "spinbutton" => Role::SpinButton,
        // "textbox" is the ARIA role name returned by WebKit2GTK for both
        // <input type="text"> and <textarea>.  Map to TextArea here so the
        // multi-line refinement below can downgrade single-line ones to TextField.
        // "embedded" is WebKit2GTK's numeric-path equivalent (role 78);
        // "terminal" is VTE — text is reachable via the Text interface.
        "text" | "textbox" | "embedded" | "terminal" => Role::TextArea,
        "label" | "static" | "caption" => Role::StaticText,
        "combo box" | "combobox" => Role::ComboBox,
        // "listbox" is the ARIA role name returned by WebKit2GTK for role="listbox".
        "list" | "list box" | "listbox" => Role::List,
        "list item" => Role::ListItem,
        "menu" | "popup menu" => Role::Menu,
        "menu item" | "tearoff menu item" => Role::MenuItem,
        "menu bar" => Role::MenuBar,
        "page tab" => Role::Tab,
        "page tab list" => Role::TabGroup,
        "table" | "tree table" => Role::Table,
        "table row" => Role::TableRow,
        // Bare "column header"/"row header" are WebKit's <th> and GTK
        // column-view headers; the "table …" pair is the classic toolkit one.
        "table cell"
        | "table column header"
        | "table row header"
        | "column header"
        | "row header" => Role::TableCell,
        "tool bar" => Role::Toolbar,
        "scroll bar" => Role::ScrollBar,
        "slider" => Role::Slider,
        "image" | "icon" | "desktop icon" => Role::Image,
        "link" => Role::Link,
        "panel" | "section" | "form" | "filler" | "viewport" | "scroll pane" | "paragraph"
        | "header" | "footer" | "grouping" | "block quote" | "redundant object" | "tree"
        | "comment" | "title bar" | "calendar" => Role::Group,
        "progress bar" | "level bar" => Role::ProgressBar,
        "tree item" => Role::TreeItem,
        "document web"
        | "document frame"
        | "document spreadsheet"
        | "document presentation"
        | "document text"
        | "document email" => Role::WebArea,
        "heading" => Role::Heading,
        "separator" => Role::Separator,
        "split pane" => Role::SplitGroup,
        "tooltip" | "tool tip" => Role::Tooltip,
        "status bar" | "statusbar" => Role::Status,
        "landmark" | "navigation" => Role::Navigation,
        _ => return None,
    })
}

/// `AtspiRole::PasswordText` — the numeric role of a password entry.
const ATSPI_ROLE_PASSWORD_TEXT: u32 = 40;

/// Whether an AT-SPI role, by name or number, is `PasswordText`.
///
/// Both forms are accepted because bridges disagree on which they fill in:
/// AccessKit reports an empty role name and only the number, GTK both. The
/// mapped `Role` cannot answer this, since `PasswordText` maps to
/// `TextField` so `text_field` selectors keep matching password entries.
pub(crate) fn is_password_role(role_name: &str, role_num: u32) -> bool {
    role_num == ATSPI_ROLE_PASSWORD_TEXT || role_name.eq_ignore_ascii_case("password text")
}

/// `AtspiRole::Frame` — the numeric role a top-level window frame reports.
///
/// Named because `is_dock_frame` needs the number itself: the table below
/// folds `Frame` and `Window` (69) into the same `Role::Window`, and a panel
/// registers only as the former. `test_frame_role_number_is_the_atspi_frame_role`
/// pins it against the AtspiRole enum the tests carry.
const ATSPI_ROLE_FRAME: u32 = 23;

/// `AtspiRole::Application`.
const ATSPI_ROLE_APPLICATION: u32 = 75;

/// `AtspiRole::Unknown`.
const ATSPI_ROLE_UNKNOWN: u32 = 67;

/// Map AT-SPI2 numeric role (AtspiRole enum) to xa11y Role.
/// Values from atspi-common Role enum (repr(u32)).
/// Test-only: production code goes through [`role_from_atspi`].
#[cfg(test)]
pub(crate) fn map_atspi_role_number(role: u32) -> Role {
    try_map_atspi_role_number(role)
        .unwrap_or_else(|| xa11y_core::unknown_role(&format!("AT-SPI role number {role}")))
}

/// [`map_atspi_role_number`] without the unknown-role arm: `None` for a
/// number the table does not map, including `Invalid` (0, also what an
/// unreadable role defaults to) and `Unknown` (67), so the caller can fall
/// back to the name.
pub(crate) fn try_map_atspi_role_number(role: u32) -> Option<Role> {
    Some(match role {
        2 => Role::Alert,    // Alert
        5 => Role::Group,    // Calendar (mirrors UIA CalendarControlType)
        7 => Role::CheckBox, // CheckBox
        8 => Role::CheckBox, // CheckMenuItem
        // Bare ColumnHeader/RowHeader (10/47): WebKit and GTK emit these for
        // <th> / column-view header cells, distinct from the
        // TableColumnHeader/TableRowHeader pair (57/58) below.
        10 => Role::TableCell,                       // ColumnHeader
        11 => Role::ComboBox,                        // ComboBox
        13 => Role::Image,                           // DesktopIcon
        16 => Role::Dialog,                          // Dialog
        19 => Role::Dialog,                          // FileChooser
        20 => Role::Group,                           // Filler
        23 => Role::Window,                          // Frame
        26 => Role::Image,                           // Icon
        27 => Role::Image,                           // Image
        29 => Role::StaticText,                      // Label
        31 => Role::List,                            // List
        32 => Role::ListItem,                        // ListItem
        33 => Role::Menu,                            // Menu
        34 => Role::MenuBar,                         // MenuBar
        35 => Role::MenuItem,                        // MenuItem
        37 => Role::Tab,                             // PageTab
        38 => Role::TabGroup,                        // PageTabList
        39 => Role::Group,                           // Panel
        ATSPI_ROLE_PASSWORD_TEXT => Role::TextField, // PasswordText; see `is_password_role`
        41 => Role::Menu,                            // PopupMenu
        42 => Role::ProgressBar,                     // ProgressBar
        43 => Role::Button,                          // Button (push button)
        44 => Role::RadioButton,                     // RadioButton
        45 => Role::RadioButton,                     // RadioMenuItem
        47 => Role::TableCell,                       // RowHeader
        48 => Role::ScrollBar,                       // ScrollBar
        49 => Role::Group,                           // ScrollPane
        50 => Role::Separator,                       // Separator
        51 => Role::Slider,                          // Slider
        52 => Role::SpinButton,                      // SpinButton
        53 => Role::SplitGroup,                      // SplitPane
        54 => Role::Status,                          // StatusBar
        55 => Role::Table,                           // Table
        56 => Role::TableCell,                       // TableCell
        57 => Role::TableCell,                       // TableColumnHeader
        58 => Role::TableCell,                       // TableRowHeader
        59 => Role::MenuItem,                        // TearoffMenuItem
        60 => Role::TextArea, // Terminal (VTE and friends — text via the Text interface)
        61 => Role::TextArea, // Text
        62 => Role::Switch,   // ToggleButton
        63 => Role::Toolbar,  // ToolBar
        64 => Role::Tooltip,  // ToolTip
        65 => Role::Group,    // Tree
        66 => Role::Table,    // TreeTable
        68 => Role::Group,    // Viewport
        69 => Role::Window,   // Window
        // Text-document containers WebKit emits for plain markup: a bare
        // <p> (no ARIA naming) is Paragraph; header/footer/caption/block
        // quote come from the matching HTML elements. Containers normalize
        // to Group, caption to StaticText, mirroring the by-name map.
        71 => Role::Group,       // Header
        72 => Role::Group,       // Footer
        73 => Role::Group,       // Paragraph
        75 => Role::Application, // Application
        78 => Role::TextArea, // Embedded — WebKit2GTK uses this for <input type="text"> and <textarea>;
        // multi-line refinement below downgrades single-line ones to TextField
        79 => Role::TextField,  // Entry
        81 => Role::StaticText, // Caption
        82 => Role::WebArea,    // DocumentFrame
        83 => Role::Heading,    // Heading
        85 => Role::Group,      // Section
        86 => Role::Group,      // RedundantObject
        87 => Role::Group,      // Form
        88 => Role::Link,       // Link
        90 => Role::TableRow,   // TableRow
        91 => Role::TreeItem,   // TreeItem
        // Document roots (92-96): LibreOffice exposes spreadsheet /
        // presentation / text / email documents with these; all normalize to
        // WebArea alongside DocumentFrame/DocumentWeb above.
        92 => Role::WebArea,      // DocumentSpreadsheet
        93 => Role::WebArea,      // DocumentPresentation
        94 => Role::WebArea,      // DocumentText
        95 => Role::WebArea,      // DocumentWeb
        96 => Role::WebArea,      // DocumentEmail
        97 => Role::Group,        // Comment (ARIA comment container)
        98 => Role::List,         // ListBox — WebKit2GTK emits this for <ul role="listbox">
        99 => Role::Group,        // Grouping
        101 => Role::Alert,       // Notification
        102 => Role::Alert,       // InfoBar (GtkInfoBar message strip)
        103 => Role::ProgressBar, // LevelBar (GtkLevelBar)
        104 => Role::Group,       // TitleBar (mirrors UIA TitleBar)
        105 => Role::Group,       // BlockQuote
        110 => Role::Navigation,  // Landmark
        116 => Role::StaticText,  // Static
        129 => Role::Button,      // PushButtonMenu
        _ => return None,
    })
}

/// Depth cap for the GTK press-fallback BFS. Wrapper patterns nest at most
/// two levels (e.g. `AdwSplitButton` → inner toggle-button → inner label);
/// depth 3 covers them with headroom without letting the walk wander.
const GTK_FALLBACK_MAX_DEPTH: u32 = 3;

/// Hard cap on visited accessibles per fallback resolution. Defensive — the
/// depth cap already bounds typical cases to ≤ 20 nodes.
const GTK_FALLBACK_MAX_NODES: usize = 200;

/// Whether an AT-SPI2 role name represents an activatable widget we are
/// willing to invoke via the fallback path. Deliberately narrow: roles that
/// could carry destructive or misleading semantics (e.g. `label` with the
/// synthesised clipboard/selection actions) are excluded.
fn is_actionable_atspi_role(role: &str) -> bool {
    matches!(
        role,
        "push button"
            | "toggle button"
            | "check box"
            | "radio button"
            | "menu item"
            | "check menu item"
            | "radio menu item"
            | "link"
            | "page tab"
            | "tab"
            | "list item"
            | "tree item"
    )
}

/// Roles the BFS refuses to descend into. Static and decorative roles never
/// lead anywhere useful, and `label` in particular carries a fan-out of
/// text-editing actions (`clipboard.copy`, `selection.delete`, …) that we
/// must never reach via a press heuristic.
fn is_never_descend_atspi_role(role: &str) -> bool {
    matches!(
        role,
        "label" | "separator" | "image" | "icon" | "static" | "caption"
    )
}

/// Whether a top-level accessible is a desktop panel or dock: an AT-SPI2
/// `frame` (numeric role [`ATSPI_ROLE_FRAME`]) whose attribute set carries
/// `window-type:dock`.
///
/// The role arrives as the number `GetRole` reports rather than the
/// `GetRoleName` string: the numeric method is the one every AT-SPI object
/// implements. `map_atspi_role_number` is no help here — `Frame` (23) and
/// `Window` (69) both map to `Role::Window`, and only the former is what a
/// panel registers as.
///
/// `window-type` mirrors the window manager's `_NET_WM_WINDOW_TYPE` hint, so
/// it is set by the panel itself rather than inferred — an ordinary
/// application window carries `normal`, or no `window-type` at all.
fn is_dock_frame(role_number: u32, attributes: &HashMap<String, String>) -> bool {
    role_number == ATSPI_ROLE_FRAME && attributes.get("window-type").is_some_and(|t| t == "dock")
}

/// Whether an AT-SPI state set means "on screen".
///
/// `VISIBLE` only says the object *would* be shown if its ancestors were —
/// a widget in a hidden dialog, a collapsed panel or a minimised window keeps
/// it. `SHOWING` is the state that means it is actually drawn, so `visible`
/// requires it (issue #489). `VISIBLE` is still required alongside it: an
/// object a toolkit has explicitly hidden is not visible even if a stale
/// `SHOWING` lingers. Shared by the tree read and the event decoder so the
/// two never disagree.
pub(crate) fn atspi_visible(bits: u64) -> bool {
    const SHOWING: u64 = 1 << 25;
    const VISIBLE: u64 = 1 << 30;
    bits & SHOWING != 0 && bits & VISIBLE != 0
}

/// The application object's path on every AT-SPI application connection.
const APP_ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";

/// Whether `Application.ToolkitName` / `Version` name GTK 4.
fn is_gtk4_toolkit(name: &str, version: &str) -> bool {
    name.eq_ignore_ascii_case("GTK") && version.trim().split('.').next() == Some("4")
}

/// Turn a `Component.GetExtents` reply into bounds, or `None` when the reply
/// is a sentinel rather than a position.
///
/// Toolkits report an unrealised or off-screen widget as `INT32_MIN`
/// coordinates (GTK 3), a negative size (`-1,-1,-1,-1`) or `0x0`. Accepting
/// those as bounds put click targets and screenshot crops at -2147483648, so
/// they are "no bounds" (issue #489). A zero width *or* height alone (a
/// separator line) is a real, if thin, rectangle and is kept — the same rule
/// the Windows backend applies.
fn extents_to_rect(x: i32, y: i32, w: i32, h: i32) -> Option<Rect> {
    if x == i32::MIN || y == i32::MIN || w < 0 || h < 0 || (w == 0 && h == 0) {
        return None;
    }
    Some(Rect {
        x,
        y,
        width: w as u32,
        height: h as u32,
    })
}

/// Whether a top-level window's AT-SPI state set means it exists for the
/// user: `VISIBLE` (mapped, or unmapped but not hidden: minimized or on
/// another workspace, where `SHOWING` is clear) or `ICONIFIED` (minimized).
/// A window with neither is a hidden toolkit object, not a window (issue
/// #492). `SHOWING` alone does not count, matching the provider's own
/// definition of a hidden element.
pub(crate) fn is_listable_top_level(bits: u64) -> bool {
    const ICONIFIED: u64 = 1 << 15;
    const VISIBLE: u64 = 1 << 30;
    bits & (VISIBLE | ICONIFIED) != 0
}

/// The D-Bus error a peer answers for an object path it no longer serves.
const UNKNOWN_OBJECT: &str = "org.freedesktop.DBus.Error.UnknownObject";

/// Classify the structured reply before rendering it. `UnknownObject` means
/// this exact accessible was destroyed, not that the peer timed out or that
/// `GetChildren` is unimplemented. Direct reads retain the stale-element error;
/// live traversals prune it through `live_atspi_children`.
fn children_call_error(err: zbus::Error, aref: &AccessibleRef) -> Error {
    match &err {
        zbus::Error::MethodError(name, ..) if name.as_str() == UNKNOWN_OBJECT => {
            Error::ElementStale {
                selector: format!("{}{}", aref.bus_name, aref.path),
            }
        }
        _ => Error::Platform {
            code: -1,
            message: format!("GetChildren failed: {err}"),
        },
    }
}

/// AT-SPI `READ_ONLY` (`AtspiStateType` 43) and `EDITABLE` (7).
const STATE_READ_ONLY: u64 = 1 << 43;
const STATE_EDITABLE: u64 = 1 << 7;

/// Whether an element's state bits allow a text write: `EDITABLE` set and
/// `READ_ONLY` clear.
fn text_writable(bits: u64) -> bool {
    bits & STATE_EDITABLE != 0 && bits & STATE_READ_ONLY == 0
}

/// `InsertText`'s `length` for `text` under `toolkit` (see
/// `LinuxProvider::insert_text_length`). Saturates rather than wrapping for
/// text longer than `i32::MAX` units.
fn insert_text_length_for(toolkit: Option<&str>, text: &str) -> i32 {
    let units = match toolkit {
        Some(t) if t.eq_ignore_ascii_case("Qt") => text.encode_utf16().count(),
        _ => text.len(),
    };
    i32::try_from(units).unwrap_or(i32::MAX)
}

/// Decode the boolean reply of an AT-SPI write (`EditableText.*`,
/// `Text.SetSelection`). `FALSE` is the toolkit refusing the request, so it
/// is an error naming the call and the element, never `Ok` (issue #486).
/// `call` is the qualified member, e.g. `"EditableText.InsertText"`.
fn atspi_bool_reply(
    reply: zbus::Message,
    call: &str,
    target: &AccessibleRef,
    element: &ElementData,
) -> Result<()> {
    let applied: bool = reply.body().deserialize().map_err(|e| Error::Platform {
        code: -1,
        message: format!("{call} reply decode failed: {e}"),
    })?;
    if applied {
        Ok(())
    } else {
        Err(Error::Platform {
            code: -1,
            message: format!(
                "{call} on {} {}{} returned FALSE: the toolkit refused it",
                element.role, target.bus_name, target.path
            ),
        })
    }
}

/// Map a failed `EditableText` call. A missing member or interface is
/// [`Error::TextValueNotSupported`] (common on Chromium, whose URL bar only
/// exposes read-only `Text`; issue #101); anything else, including no reply,
/// is a platform error naming the method and the object — the write may or
/// may not have landed, and that is not success.
fn editable_text_call_error(method: &str, e: zbus::Error, target: &AccessibleRef) -> Error {
    if is_unimplemented(&e) {
        Error::TextValueNotSupported
    } else {
        Error::Platform {
            code: -1,
            message: format!(
                "EditableText.{method} on {}{} failed: {e}",
                target.bus_name, target.path
            ),
        }
    }
}

/// Whether a property read failed because the property (or its interface)
/// does not exist on the object — an answer, not a failure. Property reads
/// go through `org.freedesktop.DBus.Properties`, whose error replies zbus
/// surfaces as a structured `fdo::Error`.
fn is_property_absent(err: &zbus::Error) -> bool {
    const ABSENT: [&str; 3] = [
        "org.freedesktop.DBus.Error.UnknownProperty",
        "org.freedesktop.DBus.Error.UnknownInterface",
        "org.freedesktop.DBus.Error.UnknownMethod",
    ];
    match err {
        zbus::Error::FDO(e) => ABSENT.contains(&zbus::DBusError::name(e.as_ref()).as_str()),
        zbus::Error::MethodError(name, ..) => ABSENT.contains(&name.as_str()),
        _ => false,
    }
}

/// `UnknownMethod` / `UnknownInterface`: the object exists but does not
/// implement the call. Narrower than [`is_absent_member`], which also counts
/// `UnknownObject` — for a write, a destroyed target is a stale element, not
/// an element that cannot take text.
fn is_unimplemented(err: &zbus::Error) -> bool {
    match err {
        zbus::Error::MethodError(name, ..) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.UnknownMethod"
                | "org.freedesktop.DBus.Error.UnknownInterface"
        ),
        _ => false,
    }
}

/// Whether a failed D-Bus call is the connection's method timeout expiring
/// (see [`ATSPI_METHOD_TIMEOUT`]): zbus reports it as an I/O error of kind
/// `TimedOut`, distinct from a peer's own `org.freedesktop.DBus.Error.*` reply.
fn is_timeout(err: &zbus::Error) -> bool {
    matches!(err, zbus::Error::InputOutput(io) if io.kind() == std::io::ErrorKind::TimedOut)
}

/// The scheduler state of a process, read from the third field of
/// `/proc/<pid>/stat`. `None` when the process is gone or the file is not
/// readable.
fn read_process_state(pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_process_state(&stat)
}

/// Parse the state field out of a `/proc/<pid>/stat` line. The command name
/// in field two is parenthesised and may itself contain spaces or `)`, so the
/// state is the first token after the *last* `)`.
fn parse_process_state(stat: &str) -> Option<char> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().next()?.chars().next()
}

/// Render a `/proc` state letter as the reason a process might not answer.
fn describe_process_state(state: char) -> &'static str {
    match state {
        'T' => "stopped (SIGSTOP or job control); it cannot answer until it receives SIGCONT",
        't' => "stopped by a debugger (tracing stop)",
        'D' => "in uninterruptible sleep (blocked on I/O)",
        'Z' => "a zombie (exited but not yet reaped)",
        'R' | 'S' => {
            "running, so its event loop is busy or deadlocked rather than the process being \
             stopped"
        }
        _ => "in an unrecognised scheduler state",
    }
}

/// Build the timeout error for an application that did not answer `call`.
///
/// Kept free of D-Bus so its wording is unit-testable: the diagnosis names
/// the call, the bus name and object, the pid and what `/proc` says about the
/// process — everything a caller needs to decide whether to skip, retry, or
/// `SIGCONT` the app without re-running under logging (tenet 6).
fn unresponsive_peer_error(
    aref: &AccessibleRef,
    call: &str,
    pid: Option<u32>,
    state: Option<char>,
) -> Error {
    let who = match pid {
        Some(pid) => format!("AT-SPI application {} (pid {pid})", aref.bus_name),
        None => format!("AT-SPI application {} (pid unknown)", aref.bus_name),
    };
    let observed = match (pid, state) {
        (Some(pid), Some(state)) => format!(
            "no reply to {call} on {} within {:?}; process {pid} is {}",
            aref.path,
            ATSPI_METHOD_TIMEOUT,
            describe_process_state(state)
        ),
        (Some(pid), None) => format!(
            "no reply to {call} on {} within {:?}; process {pid} has no readable /proc entry",
            aref.path, ATSPI_METHOD_TIMEOUT
        ),
        (None, _) => format!(
            "no reply to {call} on {} within {:?}",
            aref.path, ATSPI_METHOD_TIMEOUT
        ),
    };
    Error::timeout(ATSPI_METHOD_TIMEOUT).diagnose(
        xa11y_core::Diagnosis::new()
            .condition(format!("{who} to answer {call}"))
            .last_observed(observed),
    )
}

/// D-Bus error names that mean the member was never there, as opposed to the
/// call having failed.
///
/// A reply of `UnknownMethod` / `UnknownInterface` / `UnknownObject` is the
/// bus answering the question: this object does not implement that. AT-SPI
/// bridges implement the interface partially — AccessKit's serves `GetRole`
/// and `GetLocalizedRoleName` but no `GetRoleName` at all — so a caller that
/// reads absence as failure fails on a perfectly healthy desktop.
const ABSENT_MEMBER_ERRORS: [&str; 3] = [
    "org.freedesktop.DBus.Error.UnknownMethod",
    "org.freedesktop.DBus.Error.UnknownInterface",
    "org.freedesktop.DBus.Error.UnknownObject",
];

/// D-Bus error names that mean the *peer* is gone, as opposed to the call
/// having failed.
///
/// The shell scan reads every top-level of every application on the bus, and an
/// application that quits between `list_apps()` and those reads answers with
/// one of these. That is genuine absence — the process and its surfaces are
/// gone — not a failure to hide (tenet 1): there is nothing left to report and
/// nothing a caller could do about it. A hung or broken application is a
/// different outcome and stays a failure: it answers `NoReply` or `Timeout`, or
/// does not answer at all.
///
/// Kept separate from [`ABSENT_MEMBER_ERRORS`] because the two are different
/// axes — "this object does not implement that member" versus "this bus name
/// has no owner any more".
const PEER_GONE_ERRORS: [&str; 2] = [
    "org.freedesktop.DBus.Error.ServiceUnknown",
    "org.freedesktop.DBus.Error.NameHasNoOwner",
];

/// Whether a failed D-Bus call means the peer left the bus (see
/// [`PEER_GONE_ERRORS`]).
///
/// Matches the structured error name for the same reason
/// [`is_absent_member`] does, never a substring of a formatted message.
fn is_peer_gone(err: &zbus::Error) -> bool {
    match err {
        zbus::Error::MethodError(name, ..) => PEER_GONE_ERRORS.contains(&name.as_str()),
        // Property reads go through `org.freedesktop.DBus.Properties`, whose
        // error replies zbus surfaces as a structured `fdo::Error` instead.
        zbus::Error::FDO(e) => PEER_GONE_ERRORS.contains(&e.name().as_str()),
        _ => false,
    }
}

/// Whether a failed D-Bus call means the object or its application is gone:
/// an absent member (see [`is_absent_member`]; GDBus answers a destroyed
/// object with `UnknownMethod`) or a departed peer (see [`is_peer_gone`]).
/// Only for members every accessible implements, where absence can mean
/// nothing else.
pub(crate) fn is_gone(err: &zbus::Error) -> bool {
    is_absent_member(err) || is_peer_gone(err)
}

/// Whether a failed D-Bus call means the member is absent (see
/// [`ABSENT_MEMBER_ERRORS`]) rather than that the call failed.
///
/// `Proxy::call_method` surfaces an error *reply* as
/// `zbus::Error::MethodError`, which keeps the error name structured — so
/// this matches the name itself, never a substring of a formatted message.
/// Every other `zbus::Error` (no reply, a timeout, a service that left the
/// bus, a broken connection) is a genuine failure and stays one.
fn is_absent_member(err: &zbus::Error) -> bool {
    match err {
        zbus::Error::MethodError(name, ..) => ABSENT_MEMBER_ERRORS.contains(&name.as_str()),
        zbus::Error::FDO(e) => ABSENT_MEMBER_ERRORS.contains(&e.name().as_str()),
        _ => false,
    }
}

/// Re-word a classification failure so it names the top-level that would not
/// answer and the application that had just listed it.
///
/// The shell scan classifies a frame by two D-Bus reads, and either failing
/// aborts the listing rather than being read as "not a panel" (tenet 1). What
/// the raw `GetRole failed: …` does not say is *whose* window it was — which
/// is the whole of what a consumer needs to know to go look (tenet 6).
fn classify_failure(app: &ElementData, top: &AccessibleRef, err: Error) -> Error {
    let name = app.name.as_deref().unwrap_or("<unnamed>");
    let pid = app.pid.map(|p| format!(" pid={p}")).unwrap_or_default();
    Error::Platform {
        code: match &err {
            Error::Platform { code, .. } => *code,
            _ => -1,
        },
        message: format!(
            "could not classify top-level {} of application \"{name}\"{pid} as a shell \
             surface: {err}",
            top.path
        ),
    }
}

/// Map an AT-SPI2 action name to its canonical `snake_case` xa11y action name.
///
/// Toolkit-specific aliases are normalised to the single canonical name:
///   "click" / "activate" / "press" / "invoke" / "dodefault" → "press"
///   "toggle" / "check" / "uncheck"            → "toggle"
///   "expand" / "open"                          → "expand"
///   "collapse" / "close"                       → "collapse"
///   "menu" / "showmenu" / "showcontextmenu" / "popup" / "show menu" → "show_menu"
///   "select"                                    → "select"
///   "increment"                                 → "increment"
///   "decrement"                                 → "decrement"
///
/// Returns `None` for unrecognised names.
fn map_atspi_action_name(action_name: &str) -> Option<String> {
    // Normalise by lowercasing and stripping underscores/spaces so that
    // "show_menu", "show menu", "showMenu" and "showContextMenu" all collapse
    // to the same canonical form. Chromium is the main motivator: it uses
    // `doDefault` as the default activation action on ~190 elements per
    // window (Back/Forward buttons, toolbar icons, menu items, sliders,
    // notifications) and `showContextMenu` as the context-menu action. The
    // previous table dropped both, leaving those elements with no `press`
    // mapping at all. See issue #101.
    let lower = action_name.to_lowercase();
    let collapsed: String = lower.chars().filter(|c| !matches!(c, '_' | ' ')).collect();
    let canonical = match collapsed.as_str() {
        "click" | "activate" | "press" | "invoke" | "dodefault" => "press",
        "toggle" | "check" | "uncheck" => "toggle",
        "expand" | "open" => "expand",
        "collapse" | "close" => "collapse",
        "select" => "select",
        "menu" | "showmenu" | "showcontextmenu" | "contextmenu" | "popup" => "show_menu",
        "increment" => "increment",
        "decrement" => "decrement",
        _ => return None,
    };
    Some(canonical.to_string())
}

/// Resolve AT-SPI's overloaded `menu` role using positive activation evidence.
///
/// GTK 3 reports both popup menu containers and submenu-bearing entries as
/// `menu`. Its entries implement Action with `click`, while the GtkMenu
/// container does not implement Action. Parentage and children cannot separate
/// the two: GTK reparents the popup under its owning entry and exposes submenu
/// items directly as children of that same entry.
fn refine_menu_role(role: Role, actions: &[String]) -> Role {
    if role == Role::Menu && actions.iter().any(|action| action == "press") {
        Role::MenuItem
    } else {
        role
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── #486 / #493: EditableText writes ─────────────────────────────────

    fn bool_reply(value: bool) -> zbus::Message {
        let call = zbus::Message::method_call("/org/a11y/atspi/accessible/9", "InsertText")
            .expect("method-call builder")
            .serial(std::num::NonZeroU32::new(1).expect("nonzero"))
            .build(&())
            .expect("build method call");
        zbus::Message::method_return(&call.header())
            .expect("reply builder")
            .serial(std::num::NonZeroU32::new(2).expect("nonzero"))
            .build(&value)
            .expect("build reply")
    }

    fn text_target() -> (AccessibleRef, ElementData) {
        (
            AccessibleRef {
                bus_name: ":1.9".into(),
                path: "/org/a11y/atspi/accessible/9".into(),
            },
            ElementData::for_role(Role::TextField),
        )
    }

    #[test]
    fn false_editable_text_reply_is_an_error_naming_method_and_element() {
        let (target, element) = text_target();
        assert!(atspi_bool_reply(
            bool_reply(true),
            "EditableText.InsertText",
            &target,
            &element
        )
        .is_ok());
        let err = atspi_bool_reply(
            bool_reply(false),
            "EditableText.InsertText",
            &target,
            &element,
        )
        .expect_err("FALSE is the toolkit refusing the write");
        let msg = err.to_string();
        assert!(msg.contains("EditableText.InsertText"), "{msg}");
        assert!(msg.contains(":1.9/org/a11y/atspi/accessible/9"), "{msg}");
        assert!(msg.contains("FALSE"), "{msg}");
    }

    #[test]
    fn editable_text_call_errors_keep_the_reason() {
        let (target, _) = text_target();
        let unimplemented = zbus::Error::MethodError(
            zbus::names::OwnedErrorName::try_from("org.freedesktop.DBus.Error.UnknownInterface")
                .expect("error name"),
            None,
            bool_reply(true),
        );
        assert!(matches!(
            editable_text_call_error("InsertText", unimplemented, &target),
            Error::TextValueNotSupported
        ));
        // A destroyed object or an unanswered call is not "cannot take text".
        for name in [
            "org.freedesktop.DBus.Error.UnknownObject",
            "org.freedesktop.DBus.Error.NoReply",
            "org.freedesktop.DBus.Error.NotSupported",
        ] {
            let e = zbus::Error::MethodError(
                zbus::names::OwnedErrorName::try_from(name).expect("error name"),
                None,
                bool_reply(true),
            );
            assert!(
                matches!(editable_text_call_error("InsertText", e, &target),
                    Error::Platform { message, .. } if message.contains(name)),
                "{name}"
            );
        }
    }

    #[test]
    fn false_set_selection_reply_is_an_error() {
        let (target, element) = text_target();
        let err = atspi_bool_reply(
            bool_reply(false),
            "Text.SetSelection(2, 5)",
            &target,
            &element,
        )
        .expect_err("a refused selection is not success");
        assert!(err.to_string().contains("Text.SetSelection(2, 5)"), "{err}");
    }

    #[test]
    fn only_an_absent_toolkit_name_defaults_to_bytes() {
        let absent = zbus::Error::from(zbus::fdo::Error::UnknownProperty("ToolkitName".into()));
        assert!(is_property_absent(&absent));
        let absent = zbus::Error::from(zbus::fdo::Error::UnknownInterface("x".into()));
        assert!(is_property_absent(&absent));
        // A peer that did not answer, or answered with anything else, is not
        // "no toolkit" — it must not silently size Qt text in bytes.
        for e in [
            zbus::Error::from(zbus::fdo::Error::NoReply("hung".into())),
            zbus::Error::from(zbus::fdo::Error::ServiceUnknown("gone".into())),
            zbus::Error::InputOutput(std::sync::Arc::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out",
            ))),
        ] {
            assert!(!is_property_absent(&e), "{e}");
        }
    }

    #[test]
    fn text_writes_require_editable_and_not_read_only() {
        assert!(text_writable(STATE_EDITABLE));
        assert!(!text_writable(0));
        assert!(!text_writable(STATE_READ_ONLY));
        assert!(!text_writable(STATE_EDITABLE | STATE_READ_ONLY));
        // READ_ONLY lives in the second state word (bit 43 = word 1, bit 11).
        assert_eq!(STATE_READ_ONLY >> 32, 1 << 11);
    }

    #[test]
    fn insert_text_length_uses_utf16_units_for_qt_only() {
        // ASCII: every convention agrees.
        assert_eq!(insert_text_length_for(Some("Qt"), "abc"), 3);
        assert_eq!(insert_text_length_for(Some("GTK"), "abc"), 3);
        // Accented: 1 UTF-16 unit, 2 UTF-8 bytes.
        assert_eq!(insert_text_length_for(Some("Qt"), "é"), 1);
        assert_eq!(insert_text_length_for(Some("GTK"), "é"), 2);
        // Non-BMP emoji: a surrogate pair in UTF-16, 4 bytes in UTF-8.
        assert_eq!(insert_text_length_for(Some("Qt"), "a😀"), 3);
        assert_eq!(insert_text_length_for(Some("qt"), "a😀"), 3);
        assert_eq!(insert_text_length_for(Some("GTK"), "a😀"), 5);
        // Unknown toolkit keeps libatspi's documented byte count.
        assert_eq!(insert_text_length_for(None, "a😀"), 5);
    }

    // ── #489: SHOWING and sentinel extents ───────────────────────────────

    #[test]
    fn visible_requires_showing() {
        const SHOWING: u64 = 1 << 25;
        const VISIBLE: u64 = 1 << 30;
        assert!(atspi_visible(VISIBLE | SHOWING));
        // Would be shown if its parents were — e.g. inside a hidden dialog.
        assert!(!atspi_visible(VISIBLE));
        assert!(!atspi_visible(SHOWING));
        assert!(!atspi_visible(0));
        // The event decoder uses the same rule.
        assert!(!crate::events::states_from_bits(VISIBLE, Role::Button, true, false).visible);
        assert!(
            crate::events::states_from_bits(VISIBLE | SHOWING, Role::Button, true, false).visible
        );
    }

    #[test]
    fn sentinel_extents_are_no_bounds() {
        assert_eq!(
            extents_to_rect(10, 20, 30, 40),
            Some(Rect {
                x: 10,
                y: 20,
                width: 30,
                height: 40
            })
        );
        // Off-screen to the left/top is a real position.
        assert!(extents_to_rect(-50, -5, 30, 40).is_some());
        assert_eq!(extents_to_rect(i32::MIN, i32::MIN, 0, 0), None);
        assert_eq!(extents_to_rect(i32::MIN, 0, 30, 40), None);
        assert_eq!(extents_to_rect(0, i32::MIN, 30, 40), None);
        assert_eq!(extents_to_rect(-1, -1, -1, -1), None);
        assert_eq!(extents_to_rect(5, 5, -3, 10), None);
        assert_eq!(extents_to_rect(5, 5, 0, 0), None);
        // N×0 and 0×N are thin but real (a separator), as on Windows.
        assert!(extents_to_rect(5, 5, 30, 0).is_some());
        assert!(extents_to_rect(5, 5, 0, 30).is_some());
    }

    // ── #485: role number before role name ───────────────────────────────

    #[test]
    fn role_number_wins_over_a_disagreeing_name() {
        // GTK 4: a window frame named "application" is a window, not a
        // nested application to flatten away.
        assert_eq!(
            role_from_atspi(ATSPI_ROLE_FRAME, "application"),
            Role::Window
        );
        // GTK 4 "text box" numbered as an entry / text.
        assert_eq!(role_from_atspi(79, "text box"), Role::TextField);
        assert_eq!(role_from_atspi(61, "text box"), Role::TextArea);
        // "generic" is no name we map, and the number answers first anyway.
        assert_eq!(role_from_atspi(39, "generic"), Role::Group);
        // The application number is still the application.
        assert_eq!(
            role_from_atspi(ATSPI_ROLE_APPLICATION, ""),
            Role::Application
        );
    }

    #[test]
    fn role_name_is_the_fallback_for_an_unmapped_number() {
        // AccessKit: numbers only, no names.
        assert_eq!(role_from_atspi(43, ""), Role::Button);
        // Unreadable role (defaults to 0, Invalid) with a readable name.
        assert_eq!(role_from_atspi(0, "push button"), Role::Button);
        // AtspiRole::Unknown defers to a known name…
        assert_eq!(
            role_from_atspi(ATSPI_ROLE_UNKNOWN, "check box"),
            Role::CheckBox
        );
        // …and is otherwise the platform's own "unknown", not an unmapped role
        // (this must not hit the strict-roles panic).
        assert_eq!(role_from_atspi(ATSPI_ROLE_UNKNOWN, ""), Role::Unknown);
    }

    #[test]
    fn hidden_top_levels_are_not_listed_but_minimized_ones_are() {
        const ICONIFIED: u64 = 1 << 15;
        const SHOWING: u64 = 1 << 25;
        const VISIBLE: u64 = 1 << 30;
        // Qt's hidden dialog: none of the three.
        assert!(!is_listable_top_level(0));
        assert!(!is_listable_top_level(1 << 8)); // ENABLED alone
        assert!(is_listable_top_level(VISIBLE | SHOWING));
        // Unmapped (another workspace): VISIBLE without SHOWING.
        assert!(is_listable_top_level(VISIBLE));
        // SHOWING without VISIBLE is not a shown window.
        assert!(!is_listable_top_level(SHOWING));
        // Minimized: no SHOWING, but ICONIFIED keeps it listed.
        assert!(is_listable_top_level(ICONIFIED));
    }

    #[test]
    fn gtk4_is_detected_from_toolkit_name_and_major_version() {
        assert!(is_gtk4_toolkit("GTK", "4.14.5"));
        assert!(is_gtk4_toolkit("gtk", "4.16.0"));
        assert!(!is_gtk4_toolkit("GTK", "3.24.41"));
        assert!(!is_gtk4_toolkit("GTK", "40.1"));
        assert!(!is_gtk4_toolkit("Qt", "4.8.7"));
        assert!(!is_gtk4_toolkit("GTK", ""));
    }

    #[test]
    fn test_role_mapping() {
        assert_eq!(map_atspi_role("push button"), Role::Button);
        assert_eq!(map_atspi_role("toggle button"), Role::Switch);
        assert_eq!(map_atspi_role("check box"), Role::CheckBox);
        assert_eq!(map_atspi_role("entry"), Role::TextField);
        assert_eq!(map_atspi_role("label"), Role::StaticText);
        assert_eq!(map_atspi_role("window"), Role::Window);
        assert_eq!(map_atspi_role("frame"), Role::Window);
        assert_eq!(map_atspi_role("dialog"), Role::Dialog);
        assert_eq!(map_atspi_role("combo box"), Role::ComboBox);
        assert_eq!(map_atspi_role("slider"), Role::Slider);
        assert_eq!(map_atspi_role("panel"), Role::Group);
        assert_eq!(map_atspi_role("unknown_thing"), Role::Unknown);
    }

    #[test]
    fn menu_with_native_activation_is_a_menu_item() {
        assert_eq!(
            refine_menu_role(Role::Menu, &["press".to_string()]),
            Role::MenuItem
        );
    }

    #[test]
    fn non_activatable_menu_stays_a_container() {
        assert_eq!(refine_menu_role(Role::Menu, &[]), Role::Menu);
        assert_eq!(
            refine_menu_role(Role::Menu, &["show_menu".to_string()]),
            Role::Menu
        );
    }

    #[test]
    fn activation_does_not_change_other_roles() {
        assert_eq!(
            refine_menu_role(Role::MenuBar, &["press".to_string()]),
            Role::MenuBar
        );
        assert_eq!(
            refine_menu_role(Role::MenuItem, &["press".to_string()]),
            Role::MenuItem
        );
    }

    #[test]
    fn test_numeric_role_mapping() {
        // ToggleButton (62) must map to Switch, not Button.
        // GTK4's Gtk.Switch and Gtk.ToggleButton both report numeric role 62.
        assert_eq!(map_atspi_role_number(62), Role::Switch);
        // Sanity-check a few well-established numeric mappings.
        assert_eq!(map_atspi_role_number(43), Role::Button); // PushButton
        assert_eq!(map_atspi_role_number(7), Role::CheckBox);
        assert!(try_map_atspi_role_number(67).is_none()); // AT-SPI Unknown: defers to the name
    }

    #[test]
    fn grab_focus_notsupported_is_action_not_supported() {
        // The GTK4 adapter answers a button GrabFocus with the D-Bus
        // NotSupported error instead of a false reply (issue #98); the
        // classifier must surface it as ActionNotSupported so the suites'
        // xfail contract holds, and keep everything else a platform error
        // (tenet 1).
        let declined = grab_focus_error(
            "org.freedesktop.DBus.Error.NotSupported".into(),
            "focusing",
            "focus",
            Role::Button,
        );
        match declined {
            Error::ActionNotSupported { action, role } => {
                assert_eq!(action, "focus");
                assert_eq!(role, Role::Button);
            }
            other => panic!("expected ActionNotSupported, got {other:?}"),
        }

        let broken = grab_focus_error(
            "org.freedesktop.DBus.Error.UnknownMethod".into(),
            "activating",
            "activate",
            Role::Window,
        );
        match broken {
            Error::Platform { code, .. } => assert_eq!(code, -1),
            other => panic!("expected Platform error, got {other:?}"),
        }
    }

    /// The full AtspiRole enum (atspi 2.52) as (number, GetRoleName-style
    /// name) pairs. The name form is the enum nick with dashes replaced by
    /// spaces, which is what providers hand back over D-Bus.
    const ATSPI_ROLE_ENUM: &[(u32, &str)] = &[
        (0, "invalid"),
        (1, "accelerator label"),
        (2, "alert"),
        (3, "animation"),
        (4, "arrow"),
        (5, "calendar"),
        (6, "canvas"),
        (7, "check box"),
        (8, "check menu item"),
        (9, "color chooser"),
        (10, "column header"),
        (11, "combo box"),
        (12, "date editor"),
        (13, "desktop icon"),
        (14, "desktop frame"),
        (15, "dial"),
        (16, "dialog"),
        (17, "directory pane"),
        (18, "drawing area"),
        (19, "file chooser"),
        (20, "filler"),
        (21, "focus traversable"),
        (22, "font chooser"),
        (23, "frame"),
        (24, "glass pane"),
        (25, "html container"),
        (26, "icon"),
        (27, "image"),
        (28, "internal frame"),
        (29, "label"),
        (30, "layered pane"),
        (31, "list"),
        (32, "list item"),
        (33, "menu"),
        (34, "menu bar"),
        (35, "menu item"),
        (36, "option pane"),
        (37, "page tab"),
        (38, "page tab list"),
        (39, "panel"),
        (40, "password text"),
        (41, "popup menu"),
        (42, "progress bar"),
        (43, "push button"),
        (44, "radio button"),
        (45, "radio menu item"),
        (46, "root pane"),
        (47, "row header"),
        (48, "scroll bar"),
        (49, "scroll pane"),
        (50, "separator"),
        (51, "slider"),
        (52, "spin button"),
        (53, "split pane"),
        (54, "status bar"),
        (55, "table"),
        (56, "table cell"),
        (57, "table column header"),
        (58, "table row header"),
        (59, "tearoff menu item"),
        (60, "terminal"),
        (61, "text"),
        (62, "toggle button"),
        (63, "tool bar"),
        (64, "tool tip"),
        (65, "tree"),
        (66, "tree table"),
        (67, "unknown"),
        (68, "viewport"),
        (69, "window"),
        (70, "extended"),
        (71, "header"),
        (72, "footer"),
        (73, "paragraph"),
        (74, "ruler"),
        (75, "application"),
        (76, "autocomplete"),
        (77, "editbar"),
        (78, "embedded"),
        (79, "entry"),
        (80, "chart"),
        (81, "caption"),
        (82, "document frame"),
        (83, "heading"),
        (84, "page"),
        (85, "section"),
        (86, "redundant object"),
        (87, "form"),
        (88, "link"),
        (89, "input method window"),
        (90, "table row"),
        (91, "tree item"),
        (92, "document spreadsheet"),
        (93, "document presentation"),
        (94, "document text"),
        (95, "document web"),
        (96, "document email"),
        (97, "comment"),
        (98, "list box"),
        (99, "grouping"),
        (100, "image map"),
        (101, "notification"),
        (102, "info bar"),
        (103, "level bar"),
        (104, "title bar"),
        (105, "block quote"),
        (106, "audio"),
        (107, "video"),
        (108, "definition"),
        (109, "article"),
        (110, "landmark"),
        (111, "log"),
        (112, "marquee"),
        (113, "math"),
        (114, "rating"),
        (115, "timer"),
        (116, "static"),
        (117, "math fraction"),
        (118, "math root"),
        (119, "subscript"),
        (120, "superscript"),
        (121, "description list"),
        (122, "description term"),
        (123, "description value"),
        (124, "footnote"),
        (125, "content deletion"),
        (126, "content insertion"),
        (127, "mark"),
        (128, "suggestion"),
        (129, "push button menu"),
    ];

    #[test]
    fn numeric_and_name_role_maps_agree() {
        // The provider resolves roles by number on some paths (resolve_role)
        // and by name on others (build_element_data). If the two maps drift,
        // the same element reports different roles depending on how it was
        // reached — this happened for StatusBar (54, name-only), ToolTip
        // (64, and 93 was mis-numbered as Tooltip), and Landmark (110).
        // Sweep the whole enum so any future addition lands in both maps.
        for &(num, name) in ATSPI_ROLE_ENUM {
            let by_num = map_atspi_role_number(num);
            let by_name = map_atspi_role(name);
            assert_eq!(
                by_num, by_name,
                "AT-SPI role {num} ({name:?}) diverges: numeric map gives \
                 {by_num:?}, name map gives {by_name:?}"
            );
        }
    }

    #[test]
    fn test_action_name_mapping() {
        assert_eq!(map_atspi_action_name("click"), Some("press".to_string()));
        assert_eq!(map_atspi_action_name("activate"), Some("press".to_string()));
        assert_eq!(map_atspi_action_name("press"), Some("press".to_string()));
        assert_eq!(map_atspi_action_name("invoke"), Some("press".to_string()));
        // Chromium uses `doDefault` for default activation on ~190 elements.
        assert_eq!(
            map_atspi_action_name("doDefault"),
            Some("press".to_string())
        );
        assert_eq!(
            map_atspi_action_name("do_default"),
            Some("press".to_string())
        );
        assert_eq!(map_atspi_action_name("toggle"), Some("toggle".to_string()));
        assert_eq!(map_atspi_action_name("check"), Some("toggle".to_string()));
        assert_eq!(map_atspi_action_name("uncheck"), Some("toggle".to_string()));
        assert_eq!(map_atspi_action_name("expand"), Some("expand".to_string()));
        assert_eq!(map_atspi_action_name("open"), Some("expand".to_string()));
        assert_eq!(
            map_atspi_action_name("collapse"),
            Some("collapse".to_string())
        );
        assert_eq!(map_atspi_action_name("close"), Some("collapse".to_string()));
        assert_eq!(map_atspi_action_name("select"), Some("select".to_string()));
        assert_eq!(map_atspi_action_name("menu"), Some("show_menu".to_string()));
        assert_eq!(
            map_atspi_action_name("showmenu"),
            Some("show_menu".to_string())
        );
        assert_eq!(
            map_atspi_action_name("popup"),
            Some("show_menu".to_string())
        );
        assert_eq!(
            map_atspi_action_name("show menu"),
            Some("show_menu".to_string())
        );
        // Chrome / Chromium expose the URL-bar context-menu action as
        // `showContextMenu`; the previous table missed both spellings.
        assert_eq!(
            map_atspi_action_name("showContextMenu"),
            Some("show_menu".to_string())
        );
        assert_eq!(
            map_atspi_action_name("show_context_menu"),
            Some("show_menu".to_string())
        );
        assert_eq!(
            map_atspi_action_name("increment"),
            Some("increment".to_string())
        );
        assert_eq!(
            map_atspi_action_name("decrement"),
            Some("decrement".to_string())
        );
        assert_eq!(map_atspi_action_name("foobar"), None);
    }

    /// All known AT-SPI2 aliases map to one of the well-known action names,
    /// and re-mapping the canonical name produces the same canonical name.
    #[test]
    fn test_action_name_aliases_roundtrip() {
        let atspi_names = [
            "click",
            "activate",
            "press",
            "invoke",
            "doDefault",
            "do_default",
            "toggle",
            "check",
            "uncheck",
            "expand",
            "open",
            "collapse",
            "close",
            "select",
            "menu",
            "showmenu",
            "showContextMenu",
            "show_context_menu",
            "popup",
            "show menu",
            "increment",
            "decrement",
        ];
        for name in atspi_names {
            let canonical = map_atspi_action_name(name).unwrap_or_else(|| {
                panic!("AT-SPI2 name {:?} should map to a canonical name", name)
            });
            // Re-mapping the canonical name must produce itself.
            let back = map_atspi_action_name(&canonical)
                .unwrap_or_else(|| panic!("canonical {:?} should map back to itself", canonical));
            assert_eq!(
                canonical, back,
                "AT-SPI2 {:?} -> {:?} -> {:?} (expected {:?})",
                name, canonical, back, canonical
            );
        }
    }

    /// Case-insensitive mapping works.
    #[test]
    fn test_action_name_case_insensitive() {
        assert_eq!(map_atspi_action_name("Click"), Some("press".to_string()));
        assert_eq!(map_atspi_action_name("TOGGLE"), Some("toggle".to_string()));
        assert_eq!(
            map_atspi_action_name("Increment"),
            Some("increment".to_string())
        );
    }

    /// The GTK press-fallback's actionable-role set must include the inner
    /// toggle-button pattern used by `GtkMenuButton` / `AdwMenuButton`, plus
    /// the other standard activatable roles we're willing to synthesise a
    /// click for.
    #[test]
    fn test_gtk_fallback_actionable_roles() {
        for role in [
            "push button",
            "toggle button",
            "check box",
            "radio button",
            "menu item",
            "link",
            "tab",
            "list item",
            "tree item",
        ] {
            assert!(
                is_actionable_atspi_role(role),
                "{role:?} should be actionable"
            );
        }
    }

    /// Never treat static / decorative accessibles as fallback candidates.
    /// Particularly important for `label`, whose synthesised text-editing
    /// actions (`clipboard.copy`, `selection.delete`) must never be invoked
    /// by a press heuristic.
    #[test]
    fn test_gtk_fallback_non_actionable_roles() {
        for role in [
            "label",
            "panel",
            "filler",
            "section",
            "group",
            "image",
            "separator",
            "static",
            "frame",
            "window",
        ] {
            assert!(
                !is_actionable_atspi_role(role),
                "{role:?} must not be treated as actionable"
            );
        }
    }

    /// The BFS stops at static/decorative roles. `label` in particular must
    /// never be descended into — its children are text spans with bogus
    /// actions.
    #[test]
    fn test_gtk_fallback_never_descend_roles() {
        for role in ["label", "separator", "image", "icon", "static", "caption"] {
            assert!(
                is_never_descend_atspi_role(role),
                "{role:?} must block BFS descent"
            );
        }
        // Containers — panel / filler / section / group / frame — stay
        // walkable so the BFS can reach a wrapped inner actionable.
        for role in ["panel", "filler", "section", "group", "frame"] {
            assert!(
                !is_never_descend_atspi_role(role),
                "container role {role:?} must remain descendable"
            );
        }
    }

    fn attrs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// A panel frame is the `window-type:dock` attribute and nothing else —
    /// xfce4-panel's rows are unnamed frames, so no name test can stand in
    /// for it.
    #[test]
    fn test_dock_frame_detection() {
        assert!(is_dock_frame(
            ATSPI_ROLE_FRAME,
            &attrs(&[("toolkit", "GTK"), ("window-type", "dock")])
        ));
        // An ordinary application window: same role, different window type.
        assert!(!is_dock_frame(
            ATSPI_ROLE_FRAME,
            &attrs(&[("window-type", "normal")])
        ));
        // Most toolkits set no `window-type` at all.
        assert!(!is_dock_frame(
            ATSPI_ROLE_FRAME,
            &attrs(&[("toolkit", "GTK")])
        ));
        assert!(!is_dock_frame(ATSPI_ROLE_FRAME, &HashMap::new()));
    }

    /// The frame role number is the one `GetRoleName` would spell "frame" —
    /// the classification moved to `GetRole` because AccessKit's bridge
    /// implements only the numeric method.
    #[test]
    fn test_frame_role_number_is_the_atspi_frame_role() {
        let (number, name) = ATSPI_ROLE_ENUM
            .iter()
            .find(|(_, name)| *name == "frame")
            .copied()
            .expect("the AtspiRole table must carry `frame`");
        assert_eq!(number, ATSPI_ROLE_FRAME, "{name} moved");
    }

    /// `is_password_role` must agree with the AtspiRole table by number and
    /// by name, and must leave the mapped role at `TextField` so existing
    /// `text_field` selectors keep matching password entries (#522).
    #[test]
    fn test_password_text_is_flagged_by_name_or_number() {
        let (number, name) = ATSPI_ROLE_ENUM
            .iter()
            .find(|(_, name)| *name == "password text")
            .copied()
            .expect("the AtspiRole table must carry `password text`");
        assert_eq!(number, ATSPI_ROLE_PASSWORD_TEXT, "{name} moved");
        assert!(is_password_role("", number), "AccessKit: number only");
        assert!(is_password_role("password text", 0), "name only");
        assert!(is_password_role("Password Text", number));
        assert!(!is_password_role("entry", 61));
        assert!(!is_password_role("", 0), "an unread role is not a password");
        assert_eq!(map_atspi_role_number(number), Role::TextField);
        assert_eq!(map_atspi_role(name), Role::TextField);
    }

    /// The dock attribute only makes a surface on a top-level frame. `panel`
    /// (39) is the AT-SPI role for an ordinary grouping container (it maps to
    /// `Role::Group`), and `window` (69) / `dialog` (16) are not what panels
    /// register as — `Window` in particular shares `Role::Window` with
    /// `Frame`, so only the raw number tells them apart.
    #[test]
    fn test_dock_frame_requires_frame_role() {
        let dock = attrs(&[("window-type", "dock")]);
        for (number, name) in ATSPI_ROLE_ENUM
            .iter()
            .filter(|(number, _)| *number != ATSPI_ROLE_FRAME)
        {
            assert!(
                !is_dock_frame(*number, &dock),
                "role {number} ({name:?}) must not be reported as a shell surface"
            );
        }
    }

    /// Build the `zbus::Error` a D-Bus error reply of `name` arrives as, the
    /// way `Proxy::call_method` hands it back.
    fn method_error(name: &str) -> zbus::Error {
        let call = zbus::Message::method_call("/org/a11y/atspi/accessible/7", "GetAttributes")
            .expect("method-call builder")
            .serial(std::num::NonZeroU32::new(1).expect("nonzero"))
            .build(&())
            .expect("build method call");
        let reply = zbus::Message::error(&call.header(), name)
            .expect("error builder")
            .serial(std::num::NonZeroU32::new(2).expect("nonzero"))
            .build(&"boom")
            .expect("build error reply");
        zbus::Error::from(reply)
    }

    #[test]
    fn children_errors_classify_only_unknown_object_as_stale() {
        let aref = AccessibleRef {
            bus_name: ":1.42".into(),
            path: "/org/a11y/atspi/accessible/7".into(),
        };
        let err = children_call_error(
            method_error("org.freedesktop.DBus.Error.UnknownObject"),
            &aref,
        );
        assert!(matches!(err, Error::ElementStale { selector }
            if selector.contains(&aref.bus_name) && selector.contains(&aref.path)));
        for name in [
            "org.freedesktop.DBus.Error.UnknownMethod",
            "org.freedesktop.DBus.Error.UnknownInterface",
            "org.freedesktop.DBus.Error.NoReply",
            "org.freedesktop.DBus.Error.Timeout",
            "org.freedesktop.DBus.Error.AccessDenied",
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
        ] {
            assert!(
                matches!(children_call_error(method_error(name), &aref),
                Error::Platform { message, .. } if message.contains(name)),
                "{name}"
            );
        }
        // Text resembling an error name is never enough to prune a subtree.
        assert!(matches!(
            children_call_error(
                zbus::Error::Failure("org.freedesktop.DBus.Error.UnknownObject".into()),
                &aref
            ),
            Error::Platform { .. }
        ));
    }

    /// AT-SPI peer for the #492 window filter: a role, a state set, and a
    /// `GetState` that can fail the way a destroyed or hung object does.
    struct StatePeer {
        name: &'static str,
        role: &'static str,
        children: Vec<&'static str>,
        state: u64,
        state_error: Option<&'static str>,
    }

    #[zbus::interface(name = "org.a11y.atspi.Accessible")]
    impl StatePeer {
        fn get_children(&self) -> Vec<(String, zbus::zvariant::OwnedObjectPath)> {
            self.children
                .iter()
                .map(|path| {
                    (
                        "org.test.States".into(),
                        zbus::zvariant::OwnedObjectPath::try_from(*path).expect("test path"),
                    )
                })
                .collect()
        }

        fn get_role_name(&self) -> &str {
            self.role
        }

        fn get_role(&self) -> u32 {
            match self.role {
                "application" => 75,
                "frame" => 23,
                "dialog" => 16,
                "push button" => 43,
                other => panic!("no role number for fixture role {other:?}"),
            }
        }

        fn get_state(&self) -> zbus::fdo::Result<Vec<u32>> {
            match self.state_error {
                None => Ok(vec![self.state as u32, (self.state >> 32) as u32]),
                Some("UnknownObject") => Err(zbus::fdo::Error::UnknownObject("destroyed".into())),
                Some("NoReply") => Err(zbus::fdo::Error::NoReply("unresponsive".into())),
                Some("UnknownMethod") => Err(zbus::fdo::Error::UnknownMethod(
                    "Object does not exist at path".into(),
                )),
                Some(other) => panic!("unsupported test error {other:?}"),
            }
        }

        #[zbus(property)]
        fn name(&self) -> &str {
            self.name
        }
    }

    /// Serve `peers` (path, peer) under an application at `/root` and return
    /// a provider connected to them plus the application snapshot.
    fn state_peer_app(
        peers: Vec<(&'static str, StatePeer)>,
    ) -> (Connection, LinuxProvider, ElementData) {
        let (server_socket, client_socket) =
            std::os::unix::net::UnixStream::pair().expect("socket pair");
        let server = std::thread::spawn(move || {
            let root = StatePeer {
                name: "App",
                role: "application",
                children: peers.iter().map(|(path, _)| *path).collect(),
                state: 0,
                state_error: None,
            };
            let mut builder =
                zbus::blocking::connection::Builder::async_io_unix_stream(server_socket)
                    .server(zbus::Guid::generate())
                    .expect("server GUID")
                    .p2p()
                    .serve_at("/root", root)
                    .expect("root interface");
            for (path, peer) in peers {
                builder = builder.serve_at(path, peer).expect("peer interface");
            }
            builder.build().expect("server connection")
        });
        let connection = zbus::blocking::connection::Builder::async_io_unix_stream(client_socket)
            .p2p()
            .build()
            .expect("client connection");
        let server = server.join().expect("server thread");
        let provider = LinuxProvider {
            a11y_bus: Mutex::new(connection),
            handle_cache: Mutex::new(HashMap::new()),
            action_indices: Mutex::new(HashMap::new()),
            window_manager: WindowManager::new(),
            gtk4: Gtk4Geometry::default(),
        };
        let root = provider.build_element_data(
            &AccessibleRef {
                bus_name: "org.test.States".into(),
                path: "/root".into(),
            },
            None,
        );
        (server, provider, root)
    }

    fn state_peer(name: &'static str, role: &'static str, state: u64) -> StatePeer {
        StatePeer {
            name,
            role,
            children: vec![],
            state,
            state_error: None,
        }
    }

    /// Issue #492: Qt's hidden dialog (neither VISIBLE nor ICONIFIED) is not
    /// a window; a minimized or unmapped one is; a window destroyed since the
    /// children read is gone, whether the peer says `UnknownObject` (zbus) or
    /// `UnknownMethod` (GDBus); a non-window child is never filtered.
    #[test]
    fn app_windows_drops_hidden_top_levels_and_keeps_minimized_ones() {
        const ICONIFIED: u64 = 1 << 15;
        const SHOWING: u64 = 1 << 25;
        const VISIBLE: u64 = 1 << 30;
        let mut gone = state_peer("Gone", "frame", VISIBLE | SHOWING);
        gone.state_error = Some("UnknownObject");
        // GDBus (GTK 4) answers a destroyed object with UnknownMethod.
        let mut gdbus_gone = state_peer("GdbusGone", "frame", VISIBLE | SHOWING);
        gdbus_gone.state_error = Some("UnknownMethod");
        let (_server, provider, app) = state_peer_app(vec![
            ("/main", state_peer("Main", "frame", VISIBLE | SHOWING)),
            ("/hidden", state_peer("Sample Dialog", "dialog", 1 << 8)),
            ("/minimized", state_peer("Minimized", "frame", ICONIFIED)),
            ("/unmapped", state_peer("Unmapped", "dialog", VISIBLE)),
            ("/gdbus_gone", gdbus_gone),
            ("/gone", gone),
            ("/button", state_peer("Stray", "push button", 0)),
        ]);
        let windows = provider.app_windows(&app).expect("app windows");
        let names: Vec<_> = windows
            .iter()
            .map(|w| w.name.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(names, vec!["Main", "Minimized", "Unmapped", "Stray"]);
    }

    /// A state read that fails for any reason but a destroyed object fails
    /// the listing: the window is neither listed nor hidden on a guess.
    #[test]
    fn app_windows_surfaces_a_failed_state_read() {
        let mut hung = state_peer("Hung", "frame", 0);
        hung.state_error = Some("NoReply");
        let (_server, provider, app) = state_peer_app(vec![("/hung", hung)]);
        let err = provider.app_windows(&app).expect_err("NoReply must fail");
        assert!(
            matches!(&err, Error::Platform { message, .. }
                if message.contains("GetState") && message.contains("/hung")),
            "{err:?}"
        );
    }

    /// Accessible half of the #491 fixture: a parent link and a child list
    /// that can disagree, the way a destroyed GTK 3 widget's do.
    struct AttachPeer {
        parent: &'static str,
        children: Vec<&'static str>,
        /// What `GetIndexInParent` answers: -1 for GTK 3's detached zombie.
        index: i32,
        state: u64,
    }

    #[zbus::interface(name = "org.a11y.atspi.Accessible")]
    impl AttachPeer {
        fn get_children(&self) -> Vec<(String, zbus::zvariant::OwnedObjectPath)> {
            panic!("ensure_attached must not enumerate the siblings");
        }

        fn get_index_in_parent(&self) -> i32 {
            self.index
        }

        fn get_child_at_index(
            &self,
            index: i32,
        ) -> zbus::fdo::Result<(String, zbus::zvariant::OwnedObjectPath)> {
            let path = usize::try_from(index)
                .ok()
                .and_then(|i| self.children.get(i))
                .ok_or_else(|| zbus::fdo::Error::InvalidArgs("no such child".into()))?;
            Ok((
                "org.test.Attach".into(),
                zbus::zvariant::OwnedObjectPath::try_from(*path).expect("test path"),
            ))
        }

        fn get_state(&self) -> Vec<u32> {
            vec![self.state as u32, (self.state >> 32) as u32]
        }

        #[zbus(property)]
        fn parent(&self) -> (String, zbus::zvariant::OwnedObjectPath) {
            (
                "org.test.Attach".into(),
                zbus::zvariant::OwnedObjectPath::try_from(self.parent).expect("test path"),
            )
        }
    }

    /// Action half: `DoAction` always claims success (GTK 3's zombie reply)
    /// and counts the calls, so a test can tell whether it was reached.
    struct CountingAction(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    #[zbus::interface(name = "org.a11y.atspi.Action")]
    impl CountingAction {
        fn do_action(&self, _index: i32) -> bool {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        }
    }

    /// Issue #491: a destroyed GTK 3 widget still answers `DoAction` with
    /// `true`, but its parent no longer lists it. The action must fail as
    /// stale without reaching `DoAction`; an attached sibling still acts.
    #[test]
    fn an_action_on_a_detached_object_is_stale_not_a_false_success() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_calls = calls.clone();
        let (server_socket, client_socket) =
            std::os::unix::net::UnixStream::pair().expect("socket pair");
        let server = std::thread::spawn(move || {
            const DEFUNCT: u64 = 1 << 6;
            const MANAGES_DESCENDANTS: u64 = 1 << 31;
            let peer = |parent, children, index, state| AttachPeer {
                parent,
                children,
                index,
                state,
            };
            let action = |calls: &std::sync::Arc<_>| CountingAction(std::sync::Arc::clone(calls));
            zbus::blocking::connection::Builder::async_io_unix_stream(server_socket)
                .server(zbus::Guid::generate())
                .expect("server GUID")
                .p2p()
                .serve_at("/panel", peer("/org/a11y/atspi/null", vec!["/live"], 0, 0))
                .expect("panel")
                .serve_at("/live", peer("/panel", vec![], 0, 0))
                .expect("live")
                .serve_at("/live", action(&server_calls))
                .expect("live action")
                // GTK 3's zombie: still answers, index -1.
                .serve_at("/zombie", peer("/panel", vec![], -1, 0))
                .expect("zombie")
                .serve_at("/zombie", action(&server_calls))
                .expect("zombie action")
                // Claims an index whose slot now holds a different object.
                .serve_at("/usurped", peer("/panel", vec![], 0, 0))
                .expect("usurped")
                .serve_at("/usurped", action(&server_calls))
                .expect("usurped action")
                // A table that manages descendants: only DEFUNCT counts.
                .serve_at(
                    "/table",
                    peer("/org/a11y/atspi/null", vec![], 0, MANAGES_DESCENDANTS),
                )
                .expect("table")
                .serve_at("/cell", peer("/table", vec![], 7, 0))
                .expect("cell")
                .serve_at("/cell", action(&server_calls))
                .expect("cell action")
                .serve_at("/dead_cell", peer("/table", vec![], 8, DEFUNCT))
                .expect("dead cell")
                .serve_at("/dead_cell", action(&server_calls))
                .expect("dead cell action")
                .build()
                .expect("server connection")
        });
        let connection = zbus::blocking::connection::Builder::async_io_unix_stream(client_socket)
            .p2p()
            .build()
            .expect("client connection");
        let _server = server.join().expect("server thread");
        let provider = LinuxProvider {
            a11y_bus: Mutex::new(connection),
            handle_cache: Mutex::new(HashMap::new()),
            action_indices: Mutex::new(HashMap::new()),
            window_manager: WindowManager::new(),
        };
        let at = |path: &str| AccessibleRef {
            bus_name: "org.test.Attach".into(),
            path: path.into(),
        };

        provider
            .do_atspi_action_by_index(&at("/live"), 0)
            .expect("an attached object acts");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let err = provider
            .do_atspi_action_by_index(&at("/zombie"), 0)
            .expect_err("a detached object is stale");
        assert!(
            matches!(&err, Error::ElementStale { selector } if selector == "org.test.Attach/zombie"),
            "{err:?}"
        );
        for path in ["/usurped", "/dead_cell", "/gone"] {
            assert!(
                matches!(
                    provider.do_atspi_action_by_index(&at(path), 0),
                    Err(Error::ElementStale { .. })
                ),
                "{path}"
            );
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "DoAction must not be reached on a detached object"
        );
        // A live cell of a MANAGES_DESCENDANTS table acts without a
        // membership walk.
        provider
            .do_atspi_action_by_index(&at("/cell"), 0)
            .expect("a live managed cell acts");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// Minimal AT-SPI peer: the first child disappears when its children are
    /// read, while a later sibling stays live. Exercises the real D-Bus reply
    /// path deterministically instead of relying on a scheduling race in Qt.
    struct LiveTreePeer {
        name: &'static str,
        role: &'static str,
        children: Vec<&'static str>,
        children_error: Option<&'static str>,
        /// Overrides the role number derived from `role`, for a toolkit
        /// whose name and number disagree (GTK 4).
        role_number: Option<u32>,
    }

    #[zbus::interface(name = "org.a11y.atspi.Accessible")]
    impl LiveTreePeer {
        fn get_children(
            &self,
        ) -> zbus::fdo::Result<Vec<(String, zbus::zvariant::OwnedObjectPath)>> {
            if let Some(error) = self.children_error {
                return Err(match error {
                    "UnknownObject" => zbus::fdo::Error::UnknownObject("destroyed".into()),
                    "NoReply" => zbus::fdo::Error::NoReply("unresponsive".into()),
                    _ => panic!("unsupported test error"),
                });
            }
            Ok(self
                .children
                .iter()
                .map(|path| {
                    (
                        "org.test.LiveTree".into(),
                        zbus::zvariant::OwnedObjectPath::try_from(*path).expect("test path"),
                    )
                })
                .collect())
        }

        fn get_role_name(&self) -> &str {
            self.role
        }

        /// The number for each role name these fixtures use: the provider
        /// keys on the number and reads the name only as a fallback.
        fn get_role(&self) -> u32 {
            if let Some(number) = self.role_number {
                return number;
            }
            match self.role {
                "application" => ATSPI_ROLE_APPLICATION,
                "panel" => 39,
                "push button" => 43,
                other => panic!("no role number for fixture role {other:?}"),
            }
        }

        /// Visible and showing: `App::windows` lists a top-level window only
        /// when it is `VISIBLE` or `ICONIFIED` (#492).
        fn get_state(&self) -> Vec<u32> {
            const SHOWING: u64 = 1 << 25;
            const VISIBLE: u64 = 1 << 30;
            let bits = SHOWING | VISIBLE;
            vec![bits as u32, (bits >> 32) as u32]
        }

        #[zbus(property)]
        fn name(&self) -> &str {
            self.name
        }
    }

    fn live_tree_peer(
        error: &'static str,
        vanished_role: &'static str,
    ) -> (Connection, std::sync::Arc<LinuxProvider>, ElementData) {
        let (server_socket, client_socket) =
            std::os::unix::net::UnixStream::pair().expect("socket pair");
        let server = std::thread::spawn(move || {
            zbus::blocking::connection::Builder::async_io_unix_stream(server_socket)
                .server(zbus::Guid::generate())
                .expect("server GUID")
                .p2p()
                .serve_at(
                    "/root",
                    LiveTreePeer {
                        name: "App",
                        role: "application",
                        children: vec!["/vanished", "/survivor"],
                        children_error: None,
                        role_number: None,
                    },
                )
                .expect("root interface")
                .serve_at(
                    "/vanished",
                    LiveTreePeer {
                        name: "Vanished",
                        role: vanished_role,
                        children: vec![],
                        children_error: Some(error),
                        role_number: None,
                    },
                )
                .expect("vanished interface")
                .serve_at(
                    "/survivor",
                    LiveTreePeer {
                        name: "Survivor",
                        role: "push button",
                        children: vec![],
                        children_error: None,
                        role_number: None,
                    },
                )
                .expect("survivor interface")
                .build()
                .expect("server connection")
        });
        let connection = zbus::blocking::connection::Builder::async_io_unix_stream(client_socket)
            .p2p()
            .build()
            .expect("client connection");
        let server = server.join().expect("server thread");
        let provider = std::sync::Arc::new(LinuxProvider {
            a11y_bus: Mutex::new(connection),
            handle_cache: Mutex::new(HashMap::new()),
            action_indices: Mutex::new(HashMap::new()),
            window_manager: WindowManager::new(),
            gtk4: Gtk4Geometry::default(),
        });
        let root_ref = AccessibleRef {
            bus_name: "org.test.LiveTree".into(),
            path: "/root".into(),
        };
        let root = provider.build_element_data(&root_ref, None);
        (server, provider, root)
    }

    #[test]
    fn dump_tree_and_locators_survive_a_vanished_subtree() {
        let (_server, provider, root) = live_tree_peer("UnknownObject", "panel");
        let app = xa11y_core::App::from_data(provider.clone(), root);
        let tree = app.tree(None).expect("tree prunes vanished descendants");
        assert!(tree
            .children
            .iter()
            .any(|child| child.name.as_deref() == Some("Survivor")));
        let dump = app.dump(None).expect("dump survives UnknownObject");
        assert!(dump.contains("Survivor"));
        assert_eq!(
            app.locator("button")
                .elements()
                .expect("locator survives")
                .len(),
            1
        );
        let vanished = app
            .children()
            .expect("children")
            .into_iter()
            .find(|child| child.name.as_deref() == Some("Vanished"))
            .expect("vanished snapshot");
        assert!(vanished
            .children()
            .expect("destroyed node has no live children")
            .is_empty());
        assert!(matches!(
            provider.get_atspi_children(&AccessibleRef {
                bus_name: "org.test.LiveTree".into(),
                path: "/vanished".into(),
            }),
            Err(Error::ElementStale { .. })
        ));
    }

    #[test]
    fn nested_application_disappearance_does_not_drop_live_siblings() {
        let (_server, provider, root) = live_tree_peer("UnknownObject", "application");
        let app = xa11y_core::App::from_data(provider, root);
        let dump = app.dump(None).expect("nested application pruned");
        assert!(dump.contains("Survivor"));
        assert!(!dump.contains("Vanished"));
    }

    /// GTK 4 names its top-level window `application` while numbering it a
    /// frame (23). The application flattening keys on the number, so the
    /// window survives as a window instead of being dissolved into its
    /// children (issue #485); a real nested application (number 75) is still
    /// flattened.
    #[test]
    fn a_window_named_application_survives_window_flattening() {
        let (server_socket, client_socket) =
            std::os::unix::net::UnixStream::pair().expect("socket pair");
        let peer = |name, role, children, role_number| LiveTreePeer {
            name,
            role,
            children,
            children_error: None,
            role_number,
        };
        let server = std::thread::spawn(move || {
            zbus::blocking::connection::Builder::async_io_unix_stream(server_socket)
                .server(zbus::Guid::generate())
                .expect("server GUID")
                .p2p()
                .serve_at(
                    "/root",
                    peer("App", "application", vec!["/frame", "/nested"], None),
                )
                .expect("root interface")
                .serve_at(
                    "/frame",
                    peer(
                        "Main Window",
                        "application",
                        vec!["/inner"],
                        Some(ATSPI_ROLE_FRAME),
                    ),
                )
                .expect("frame interface")
                .serve_at("/inner", peer("Inner", "push button", vec![], None))
                .expect("inner interface")
                .serve_at(
                    "/nested",
                    peer("Nested", "application", vec!["/hoisted"], None),
                )
                .expect("nested interface")
                .serve_at("/hoisted", peer("Hoisted", "push button", vec![], None))
                .expect("hoisted interface")
                .build()
                .expect("server connection")
        });
        let connection = zbus::blocking::connection::Builder::async_io_unix_stream(client_socket)
            .p2p()
            .build()
            .expect("client connection");
        let _server = server.join().expect("server thread");
        let provider = LinuxProvider {
            a11y_bus: Mutex::new(connection),
            handle_cache: Mutex::new(HashMap::new()),
            action_indices: Mutex::new(HashMap::new()),
            window_manager: WindowManager::new(),
            gtk4: Gtk4Geometry::default(),
        };
        let root = provider.build_element_data(
            &AccessibleRef {
                bus_name: "org.test.LiveTree".into(),
                path: "/root".into(),
            },
            None,
        );
        let windows = provider.app_windows(&root).expect("app windows");
        let names: Vec<_> = windows
            .iter()
            .map(|w| (w.name.as_deref().unwrap_or(""), w.role))
            .collect();
        assert_eq!(
            names,
            vec![("Main Window", Role::Window), ("Hoisted", Role::Button)],
            "the frame named `application` stays a window; the real nested \
             application is flattened into its children"
        );
    }

    #[test]
    fn dump_and_tree_preserve_real_children_failures() {
        let (_server, provider, root) = live_tree_peer("NoReply", "panel");
        let app = xa11y_core::App::from_data(provider, root);
        for result in [app.tree(None).map(|_| ()), app.dump(None).map(|_| ())] {
            assert!(matches!(result, Err(Error::Platform { message, .. })
                if message.contains("NoReply")));
        }
    }

    /// An object that does not implement the method it was asked for is
    /// absence, not failure: AccessKit's AT-SPI bridge (egui and friends)
    /// answers `GetRole` but has no `GetRoleName`, and reading that reply as a
    /// platform error failed the whole shell listing for every desktop running
    /// one such app.
    #[test]
    fn an_unimplemented_member_reads_as_absence() {
        for name in ABSENT_MEMBER_ERRORS {
            assert!(
                is_absent_member(&method_error(name)),
                "{name} must read as absence"
            );
        }
    }

    /// Everything else stays a failure — including the D-Bus errors that look
    /// superficially similar but mean the far end is broken or gone.
    /// An application that quits between `list_apps()` and the shell scan's
    /// reads answers `ServiceUnknown`. That is the process being gone, not a
    /// failure worth aborting a whole-desktop listing for — and it used to
    /// abort it, making `ShellSurface::list()` nondeterministic on a busy
    /// desktop. A hung application is a different outcome and still fails.
    #[test]
    fn a_departed_peer_is_absence_not_failure() {
        for name in [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
        ] {
            assert!(
                is_peer_gone(&method_error(name)),
                "{name} means the peer left"
            );
            // The two axes stay separate: a departed peer is not a statement
            // about which members an object implements.
            assert!(!is_absent_member(&method_error(name)), "{name}");
        }
    }

    #[test]
    fn a_hung_or_broken_peer_is_still_a_failure() {
        for name in [
            "org.freedesktop.DBus.Error.NoReply",
            "org.freedesktop.DBus.Error.Timeout",
            "org.freedesktop.DBus.Error.Disconnected",
            "org.freedesktop.DBus.Error.AccessDenied",
            "org.freedesktop.DBus.Error.UnknownMethod",
        ] {
            assert!(
                !is_peer_gone(&method_error(name)),
                "{name} must not be read as the peer having left"
            );
        }
        assert!(!is_peer_gone(&zbus::Error::InvalidReply));
    }

    #[test]
    fn other_dbus_failures_are_not_absence() {
        for name in [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NoReply",
            "org.freedesktop.DBus.Error.Timeout",
            "org.freedesktop.DBus.Error.Disconnected",
            "org.freedesktop.DBus.Error.AccessDenied",
            "org.freedesktop.DBus.Error.UnknownProperty",
        ] {
            assert!(
                !is_absent_member(&method_error(name)),
                "{name} must stay a failure"
            );
        }
        // A transport-level failure carries no error name at all.
        assert!(!is_absent_member(&zbus::Error::InvalidReply));
    }

    /// A frame the scan could not classify must fail the listing, naming the
    /// app and the object path. `Panel` is the only kind Linux vends, so
    /// swallowing this would render a broken panel process as a desktop with
    /// no panels at all — indistinguishable from the honest empty answer.
    #[test]
    fn a_classification_failure_names_the_app_and_the_top_level() {
        let mut app = ElementData::for_role(Role::Application);
        app.name = Some("xfce4-panel".to_string());
        app.pid = Some(4242);
        let top = AccessibleRef {
            bus_name: ":1.42".to_string(),
            path: "/org/a11y/atspi/accessible/7".to_string(),
        };
        let err = classify_failure(
            &app,
            &top,
            Error::Platform {
                code: -1,
                message: "GetRole failed: connection closed".to_string(),
            },
        );
        let Error::Platform { code, message } = err else {
            panic!("a D-Bus failure must stay a platform error");
        };
        assert_eq!(code, -1);
        assert!(message.contains("xfce4-panel"), "{message}");
        assert!(message.contains("pid=4242"), "{message}");
        assert!(
            message.contains("/org/a11y/atspi/accessible/7"),
            "{message}"
        );
        // Never masks the underlying D-Bus error (tenet 1).
        assert!(message.contains("GetRole failed"), "{message}");
    }

    /// An unnamed application still produces a usable message — the object
    /// path is what a consumer actually goes looking with.
    #[test]
    fn a_classification_failure_survives_an_unnamed_app() {
        let app = ElementData::for_role(Role::Application);
        let top = AccessibleRef {
            bus_name: ":1.9".to_string(),
            path: "/org/a11y/atspi/accessible/3".to_string(),
        };
        let err = classify_failure(
            &app,
            &top,
            Error::Platform {
                code: -1,
                message: "GetAttributes failed".to_string(),
            },
        );
        let msg = err.to_string();
        assert!(msg.contains("<unnamed>"), "{msg}");
        assert!(msg.contains("/org/a11y/atspi/accessible/3"), "{msg}");
    }

    /// Per-object counters for the Text / Value reads a fake atk-bridge
    /// object answers even when it does not report those interfaces.
    #[derive(Default)]
    struct InterfaceProbes {
        text: std::sync::atomic::AtomicUsize,
        value: std::sync::atomic::AtomicUsize,
    }

    impl InterfaceProbes {
        fn text(&self) -> usize {
            self.text.load(Ordering::SeqCst)
        }
        fn value(&self) -> usize {
            self.value.load(Ordering::SeqCst)
        }
    }

    /// AT-SPI peer whose `GetInterfaces` reply is configurable.
    struct InterfacePeer {
        name: &'static str,
        role: &'static str,
        children: Vec<&'static str>,
        interfaces: Vec<&'static str>,
    }

    #[zbus::interface(name = "org.a11y.atspi.Accessible")]
    impl InterfacePeer {
        fn get_children(&self) -> Vec<(String, zbus::zvariant::OwnedObjectPath)> {
            self.children
                .iter()
                .map(|path| {
                    (
                        "org.test.Ifaces".into(),
                        zbus::zvariant::OwnedObjectPath::try_from(*path).expect("test path"),
                    )
                })
                .collect()
        }

        fn get_role_name(&self) -> &str {
            self.role
        }

        fn get_interfaces(&self) -> Vec<String> {
            self.interfaces.iter().map(|s| s.to_string()).collect()
        }

        #[zbus(property)]
        fn name(&self) -> &str {
            self.name
        }
    }

    /// Text interface served on every object, the way atk-bridge registers
    /// its handlers regardless of what the underlying AtkObject implements.
    /// Each read is counted so a test can prove it was never issued.
    struct FakeText {
        text: &'static str,
        probes: std::sync::Arc<InterfaceProbes>,
    }

    #[zbus::interface(name = "org.a11y.atspi.Text")]
    impl FakeText {
        fn get_text(&self, start: i32, end: i32) -> String {
            self.probes.text.fetch_add(1, Ordering::SeqCst);
            self.text
                .chars()
                .skip(start.max(0) as usize)
                .take((end - start).max(0) as usize)
                .collect()
        }

        #[zbus(property)]
        fn character_count(&self) -> i32 {
            self.probes.text.fetch_add(1, Ordering::SeqCst);
            self.text.chars().count() as i32
        }
    }

    struct FakeValue {
        value: f64,
        probes: std::sync::Arc<InterfaceProbes>,
    }

    #[zbus::interface(name = "org.a11y.atspi.Value")]
    impl FakeValue {
        #[zbus(property)]
        fn current_value(&self) -> f64 {
            self.probes.value.fetch_add(1, Ordering::SeqCst);
            self.value
        }

        #[zbus(property)]
        fn minimum_value(&self) -> f64 {
            self.probes.value.fetch_add(1, Ordering::SeqCst);
            0.0
        }

        #[zbus(property)]
        fn maximum_value(&self) -> f64 {
            self.probes.value.fetch_add(1, Ordering::SeqCst);
            100.0
        }
    }

    struct InterfaceFixture {
        _server: Connection,
        provider: std::sync::Arc<LinuxProvider>,
        root: ElementData,
        /// Push button: reports neither Text nor Value.
        plain: std::sync::Arc<InterfaceProbes>,
        /// Text field: reports Text *and* Value, like AccessKit, whose
        /// CurrentValue is a meaningless 0.0 on text.
        entry: std::sync::Arc<InterfaceProbes>,
        /// Slider: reports Value only.
        slider: std::sync::Arc<InterfaceProbes>,
    }

    fn interface_peer() -> InterfaceFixture {
        const ACC: &str = "org.a11y.atspi.Accessible";
        const TEXT: &str = "org.a11y.atspi.Text";
        const VALUE: &str = "org.a11y.atspi.Value";
        let plain = std::sync::Arc::new(InterfaceProbes::default());
        let entry = std::sync::Arc::new(InterfaceProbes::default());
        let slider = std::sync::Arc::new(InterfaceProbes::default());
        let (server_socket, client_socket) =
            std::os::unix::net::UnixStream::pair().expect("socket pair");
        let objects = [
            (
                "/plain",
                "push button",
                "OK",
                vec![ACC],
                "",
                7.0,
                plain.clone(),
            ),
            (
                "/entry",
                "entry",
                "Entry",
                vec![ACC, TEXT, VALUE],
                "hello",
                0.0,
                entry.clone(),
            ),
            (
                "/slider",
                "slider",
                "Volume",
                vec![ACC, VALUE],
                "",
                42.0,
                slider.clone(),
            ),
        ];
        let server = std::thread::spawn(move || {
            let mut builder =
                zbus::blocking::connection::Builder::async_io_unix_stream(server_socket)
                    .server(zbus::Guid::generate())
                    .expect("server GUID")
                    .p2p()
                    .serve_at(
                        "/root",
                        InterfacePeer {
                            name: "App",
                            role: "application",
                            children: vec!["/plain", "/entry", "/slider"],
                            interfaces: vec![ACC],
                        },
                    )
                    .expect("root interface");
            for (path, role, name, interfaces, text, value, probes) in objects {
                builder = builder
                    .serve_at(
                        path,
                        InterfacePeer {
                            name,
                            role,
                            children: vec![],
                            interfaces,
                        },
                    )
                    .expect("accessible")
                    .serve_at(
                        path,
                        FakeText {
                            text,
                            probes: probes.clone(),
                        },
                    )
                    .expect("text")
                    .serve_at(path, FakeValue { value, probes })
                    .expect("value");
            }
            builder.build().expect("server connection")
        });
        let connection = zbus::blocking::connection::Builder::async_io_unix_stream(client_socket)
            .p2p()
            .build()
            .expect("client connection");
        let server = server.join().expect("server thread");
        let provider = std::sync::Arc::new(LinuxProvider {
            a11y_bus: Mutex::new(connection),
            handle_cache: Mutex::new(HashMap::new()),
            action_indices: Mutex::new(HashMap::new()),
            window_manager: WindowManager::new(),
            gtk4: Gtk4Geometry::default(),
        });
        let root = provider.build_element_data(
            &AccessibleRef {
                bus_name: "org.test.Ifaces".into(),
                path: "/root".into(),
            },
            None,
        );
        InterfaceFixture {
            _server: server,
            provider,
            root,
            plain,
            entry,
            slider,
        }
    }

    /// Issue #465: atk-bridge answers Text / Value reads on every object and
    /// logs a CRITICAL for each one whose AtkObject lacks the interface. A
    /// tree read must only ask a node for the interfaces it reports, while
    /// still reading values from the nodes that do.
    #[test]
    fn tree_reads_only_probe_interfaces_the_node_reports() {
        let fx = interface_peer();
        let app = xa11y_core::App::from_data(fx.provider.clone(), fx.root.clone());
        let tree = app.tree(None).expect("tree");
        let child = |name: &str| {
            tree.children
                .iter()
                .find(|c| c.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("{name} in tree"))
        };

        assert_eq!(child("OK").value, None);
        assert_eq!(fx.plain.text(), 0, "Text read on a node without Text");
        assert_eq!(fx.plain.value(), 0, "Value read on a node without Value");

        // Text is still preferred over Value where both are reported.
        assert_eq!(child("Entry").value.as_deref(), Some("hello"));
        assert!(fx.entry.text() > 0);

        assert_eq!(child("Volume").value.as_deref(), Some("42"));
        let slider = app
            .children()
            .expect("children")
            .into_iter()
            .find(|c| c.name.as_deref() == Some("Volume"))
            .expect("slider element");
        assert_eq!(slider.numeric_value, Some(42.0));
        assert_eq!(slider.max_value, Some(100.0));
        assert_eq!(fx.slider.text(), 0, "Text read on a Value-only node");

        // The selector fast path resolves `value` without building the
        // whole element, and must obey the same gate.
        assert_eq!(
            app.locator(r#"[value="hello"]"#)
                .elements()
                .expect("value locator")
                .len(),
            1
        );
        assert_eq!(
            fx.plain.text(),
            0,
            "matcher read Text on a node without Text"
        );
        assert_eq!(
            fx.plain.value(),
            0,
            "matcher read Value on a node without Value"
        );
    }

    // ── #484: method timeout and unresponsive-peer errors ────────────────

    #[test]
    fn process_state_parses_after_the_last_paren() {
        assert_eq!(
            parse_process_state("1234 (xa11y-test-app) T 1 1234 1234 0 -1"),
            Some('T')
        );
        // A command name containing ") S (" must not be read as the state.
        assert_eq!(
            parse_process_state("42 (evil) S (name) t 1 42 42 0 -1"),
            Some('t')
        );
        assert_eq!(parse_process_state("garbage"), None);
        assert_eq!(parse_process_state("1 (x)"), None);
    }

    #[test]
    fn own_process_state_is_readable() {
        let state = read_process_state(std::process::id()).expect("own /proc stat");
        assert!(matches!(state, 'R' | 'S'), "unexpected own state {state}");
    }

    #[test]
    fn unresponsive_peer_error_names_app_pid_and_state() {
        let aref = AccessibleRef {
            bus_name: ":1.42".into(),
            path: "/org/a11y/atspi/accessible/root".into(),
        };
        let err = unresponsive_peer_error(&aref, "Accessible.Name", Some(4321), Some('T'));
        let Error::Timeout { elapsed, diagnosis } = &err else {
            panic!("expected Timeout, got {err:?}");
        };
        assert_eq!(*elapsed, ATSPI_METHOD_TIMEOUT);
        let d = diagnosis.as_deref().expect("diagnosis attached");
        let condition = d.condition.as_deref().expect("condition");
        assert!(condition.contains(":1.42") && condition.contains("pid 4321"));
        assert!(condition.contains("Accessible.Name"));
        let observed = d.last_observed.as_deref().expect("last_observed");
        assert!(observed.contains("stopped (SIGSTOP"), "{observed}");
        assert!(observed.contains("/org/a11y/atspi/accessible/root"));
        // Rendered message carries it too, for consumers that only print.
        assert!(err.to_string().contains("pid 4321"));

        let err = unresponsive_peer_error(&aref, "Accessible.Name", None, None);
        let d = err.diagnosis().expect("diagnosis");
        assert!(d.condition.as_deref().unwrap_or("").contains("pid unknown"));
    }

    #[test]
    fn timeout_classification_is_the_io_kind_only() {
        let timed_out = zbus::Error::InputOutput(std::sync::Arc::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out",
        )));
        assert!(is_timeout(&timed_out));
        let reset = zbus::Error::InputOutput(std::sync::Arc::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset",
        )));
        assert!(!is_timeout(&reset));
        // A peer's own Timeout *reply* is not our method timeout expiring.
        assert!(!is_timeout(&method_error(
            "org.freedesktop.DBus.Error.Timeout"
        )));
    }

    #[test]
    fn property_read_errors_classify_like_method_errors() {
        let gone = zbus::Error::from(zbus::fdo::Error::ServiceUnknown("gone".into()));
        assert!(is_peer_gone(&gone));
        let absent = zbus::Error::from(zbus::fdo::Error::UnknownProperty("Name".into()));
        assert!(!is_peer_gone(&absent));
        let absent = zbus::Error::from(zbus::fdo::Error::UnknownInterface("x".into()));
        assert!(is_absent_member(&absent));
        let no_reply = zbus::Error::from(zbus::fdo::Error::NoReply("hung".into()));
        assert!(!is_peer_gone(&no_reply) && !is_absent_member(&no_reply));
    }

    /// An application whose `Name` getter never returns in time: what a
    /// SIGSTOPped process looks like from the bus.
    struct HungPeer;

    #[zbus::interface(name = "org.a11y.atspi.Accessible")]
    impl HungPeer {
        #[zbus(property)]
        fn name(&self) -> String {
            std::thread::sleep(std::time::Duration::from_secs(2));
            "late".into()
        }
    }

    #[test]
    fn hung_peer_name_read_times_out_with_a_named_error() {
        let (server_socket, client_socket) =
            std::os::unix::net::UnixStream::pair().expect("socket pair");
        let server = std::thread::spawn(move || {
            zbus::blocking::connection::Builder::async_io_unix_stream(server_socket)
                .server(zbus::Guid::generate())
                .expect("server GUID")
                .p2p()
                .serve_at("/org/a11y/atspi/accessible/root", HungPeer)
                .expect("hung interface")
                .build()
                .expect("server connection")
        });
        let connection = zbus::blocking::connection::Builder::async_io_unix_stream(client_socket)
            .p2p()
            .method_timeout(std::time::Duration::from_millis(200))
            .build()
            .expect("client connection");
        let _server = server.join().expect("server thread");
        let provider = LinuxProvider {
            a11y_bus: Mutex::new(connection),
            handle_cache: Mutex::new(HashMap::new()),
            action_indices: Mutex::new(HashMap::new()),
            window_manager: WindowManager::new(),
            gtk4: Gtk4Geometry::default(),
        };
        let aref = AccessibleRef {
            bus_name: "org.test.Hung".into(),
            path: "/org/a11y/atspi/accessible/root".into(),
        };
        let start = std::time::Instant::now();
        let err = provider
            .read_app_name(&aref)
            .expect_err("a hung peer must not read as a name or as absent");
        assert!(
            start.elapsed() < std::time::Duration::from_millis(1500),
            "the read waited for the peer instead of timing out: {:?}",
            start.elapsed()
        );
        let d = err.diagnosis().expect("timeout carries a diagnosis");
        assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
        assert!(d
            .condition
            .as_deref()
            .unwrap_or("")
            .contains("org.test.Hung"));
    }

    /// A registry listing one application whose own `GetChildren` hangs.
    struct RegistryRoot;

    #[zbus::interface(name = "org.a11y.atspi.Accessible")]
    impl RegistryRoot {
        fn get_children(&self) -> Vec<(String, zbus::zvariant::OwnedObjectPath)> {
            vec![(
                "org.test.Hung".into(),
                zbus::zvariant::OwnedObjectPath::try_from("/hungapp").expect("path"),
            )]
        }
    }

    struct HungChildren;

    #[zbus::interface(name = "org.a11y.atspi.Accessible")]
    impl HungChildren {
        fn get_children(&self) -> Vec<(String, zbus::zvariant::OwnedObjectPath)> {
            std::thread::sleep(std::time::Duration::from_secs(2));
            vec![]
        }
    }

    #[test]
    fn focused_app_reports_a_hung_app_instead_of_skipping_it() {
        let (server_socket, client_socket) =
            std::os::unix::net::UnixStream::pair().expect("socket pair");
        let server = std::thread::spawn(move || {
            zbus::blocking::connection::Builder::async_io_unix_stream(server_socket)
                .server(zbus::Guid::generate())
                .expect("server GUID")
                .p2p()
                .serve_at("/org/a11y/atspi/accessible/root", RegistryRoot)
                .expect("registry interface")
                .serve_at("/hungapp", HungChildren)
                .expect("hung interface")
                .build()
                .expect("server connection")
        });
        let connection = zbus::blocking::connection::Builder::async_io_unix_stream(client_socket)
            .p2p()
            .method_timeout(std::time::Duration::from_millis(200))
            .build()
            .expect("client connection");
        let _server = server.join().expect("server thread");
        let provider = LinuxProvider {
            a11y_bus: Mutex::new(connection),
            handle_cache: Mutex::new(HashMap::new()),
            action_indices: Mutex::new(HashMap::new()),
            window_manager: WindowManager::new(),
            gtk4: Gtk4Geometry::default(),
        };
        let err = provider
            .focused_app()
            .expect_err("a hung app must not read as 'nothing focused'");
        assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
        let condition = err
            .diagnosis()
            .and_then(|d| d.condition.clone())
            .unwrap_or_default();
        assert!(
            condition.contains("org.test.Hung") && condition.contains("GetChildren"),
            "{condition}"
        );
    }

    /// A connected client/server pair over a socket pair: the server end
    /// stands in for the bus, so dropping it is the bus hanging up.
    fn p2p_pair() -> (Connection, Connection) {
        let (server_socket, client_socket) =
            std::os::unix::net::UnixStream::pair().expect("socket pair");
        let server = std::thread::spawn(move || {
            zbus::blocking::connection::Builder::async_io_unix_stream(server_socket)
                .server(zbus::Guid::generate())
                .expect("server GUID")
                .p2p()
                .build()
                .expect("server connection")
        });
        let client = zbus::blocking::connection::Builder::async_io_unix_stream(client_socket)
            .p2p()
            .build()
            .expect("client connection");
        (server.join().expect("server thread"), client)
    }

    fn wait_closed(conn: &Connection) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !conn.is_closed() {
            assert!(
                std::time::Instant::now() < deadline,
                "peer hang-up never closed the client connection"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn closed_connection_is_replaced_and_old_handles_go_stale() {
        let (server, client) = p2p_pair();
        let provider = LinuxProvider {
            a11y_bus: Mutex::new(client.clone()),
            handle_cache: Mutex::new(HashMap::new()),
            action_indices: Mutex::new(HashMap::new()),
            window_manager: WindowManager::new(),
            gtk4: Gtk4Geometry::default(),
        };
        let handle = provider.cache_element(AccessibleRef {
            bus_name: ":1.7".into(),
            path: "/org/a11y/atspi/accessible/3".into(),
        });
        provider
            .action_indices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle, HashMap::from([("press".to_string(), 0)]));

        // While the connection is live, nothing is reconnected or forgotten.
        let live = provider
            .connection_with(|| panic!("reconnected a live connection"))
            .expect("live connection");
        assert!(!live.is_closed());
        assert!(provider.get_cached(handle).is_ok());

        drop(server);
        wait_closed(&client);

        // A failed reconnect is returned as-is, and the next call retries.
        let err = provider
            .connection_with(|| {
                Err(Error::Platform {
                    code: -1,
                    message: "bus still down".into(),
                })
            })
            .expect_err("reconnect failure surfaces");
        assert!(err.to_string().contains("bus still down"));

        let (_server2, client2) = p2p_pair();
        let fresh = provider
            .connection_with(|| Ok(client2))
            .expect("reconnected");
        assert!(!fresh.is_closed(), "handed out the closed connection");
        // A restarted bus reuses `:1.N` names, so a handle from the old
        // connection must not resolve to whoever owns that name now.
        assert!(matches!(
            provider.get_cached(handle),
            Err(Error::ElementStale { .. })
        ));
        assert!(provider.get_action_index(handle, "press").is_err());
    }
}
