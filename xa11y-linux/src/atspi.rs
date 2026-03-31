//! Real AT-SPI2 backend implementation using zbus D-Bus bindings.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use xa11y_core::selector::{AttrName, Combinator, MatchOp, SelectorSegment};
use xa11y_core::{
    Action, ActionData, CancelHandle, ElementData, Error, Event, EventReceiver, EventType,
    Provider, Rect, Result, Role, Selector, StateSet, Subscription, Toggled,
};
use zbus::blocking::{Connection, Proxy};

/// Global handle counter for mapping ElementData back to AccessibleRefs.
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Linux accessibility provider using AT-SPI2 over D-Bus.
pub struct LinuxProvider {
    a11y_bus: Connection,
    /// Cached AT-SPI accessible refs keyed by handle ID.
    handle_cache: Mutex<HashMap<u64, AccessibleRef>>,
}

/// AT-SPI2 accessible reference: (bus_name, object_path).
#[derive(Debug, Clone)]
struct AccessibleRef {
    bus_name: String,
    path: String,
}

impl LinuxProvider {
    /// Create a new Linux accessibility provider.
    ///
    /// Connects to the AT-SPI2 bus. Falls back to the session bus
    /// if the dedicated AT-SPI bus is unavailable.
    pub fn new() -> Result<Self> {
        let a11y_bus = Self::connect_a11y_bus()?;
        Ok(Self {
            a11y_bus,
            handle_cache: Mutex::new(HashMap::new()),
        })
    }

    fn connect_a11y_bus() -> Result<Connection> {
        // Try getting the AT-SPI bus address from the a11y bus launcher,
        // then connect to it. If that fails, fall back to the session bus
        // (AT-SPI2 may use the session bus directly).
        if let Ok(session) = Connection::session() {
            let proxy = Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus")
                .map_err(|e| Error::Platform {
                    code: -1,
                    message: format!("Failed to create a11y bus proxy: {}", e),
                })?;

            if let Ok(addr_reply) = proxy.call_method("GetAddress", &()) {
                if let Ok(address) = addr_reply.body().deserialize::<String>() {
                    if let Ok(addr) = zbus::Address::try_from(address.as_str()) {
                        if let Ok(Ok(conn)) =
                            zbus::blocking::connection::Builder::address(addr).map(|b| b.build())
                        {
                            return Ok(conn);
                        }
                    }
                }
            }

            // Fall back to session bus
            return Ok(session);
        }

        Connection::session().map_err(|e| Error::Platform {
            code: -1,
            message: format!("Failed to connect to D-Bus session bus: {}", e),
        })
    }

    fn make_proxy(&self, bus_name: &str, path: &str, interface: &str) -> Result<Proxy<'_>> {
        // Use uncached proxy to avoid GetAll calls — Qt's AT-SPI adaptor
        // doesn't support GetAll on all objects, causing spurious errors.
        zbus::blocking::proxy::Builder::<Proxy>::new(&self.a11y_bus)
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

    /// Check whether an accessible object implements a given interface.
    /// Queries the AT-SPI GetInterfaces method on the Accessible interface.
    fn has_interface(&self, aref: &AccessibleRef, iface: &str) -> bool {
        let proxy = match self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Accessible") {
            Ok(p) => p,
            Err(_) => return false,
        };
        let reply = match proxy.call_method("GetInterfaces", &()) {
            Ok(r) => r,
            Err(_) => return false,
        };
        let interfaces: Vec<String> = match reply.body().deserialize() {
            Ok(v) => v,
            Err(_) => return false,
        };
        interfaces.iter().any(|i| i.contains(iface))
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
        let reply = proxy
            .call_method("GetChildren", &())
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetChildren failed: {}", e),
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

    /// Get bounds via Component interface.
    /// Checks for Component support first to avoid GTK CRITICAL warnings
    /// on objects (e.g. TreeView cell renderers) that don't implement it.
    fn get_extents(&self, aref: &AccessibleRef) -> Option<Rect> {
        if !self.has_interface(aref, "Component") {
            return None;
        }
        let proxy = self
            .make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Component")
            .ok()?;
        // GetExtents(coord_type: u32) -> (x, y, width, height)
        // coord_type 0 = screen coordinates
        let reply = proxy.call_method("GetExtents", &(0u32,)).ok()?;
        let (x, y, w, h): (i32, i32, i32, i32) = reply.body().deserialize().ok()?;
        if w <= 0 && h <= 0 {
            return None;
        }
        Some(Rect {
            x,
            y,
            width: w.max(0) as u32,
            height: h.max(0) as u32,
        })
    }

    /// Get available actions via Action interface.
    /// Probes the interface directly rather than relying on the Interfaces property,
    /// which some AT-SPI adapters (e.g. AccessKit) don't expose.
    fn get_actions(&self, aref: &AccessibleRef) -> Vec<Action> {
        let mut actions = Vec::new();

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
                        if let Some(action) = map_atspi_action(&name) {
                            if !actions.contains(&action) {
                                actions.push(action);
                            }
                        }
                    }
                }
            }
        }

        // Try Component interface for Focus
        if !actions.contains(&Action::Focus) {
            if let Ok(proxy) =
                self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Component")
            {
                // Verify the interface exists by trying a method
                if proxy.call_method("GetExtents", &(0u32,)).is_ok() {
                    actions.push(Action::Focus);
                }
            }
        }

        actions
    }

    /// Get value via Value or Text interface.
    /// Probes interfaces directly rather than relying on the Interfaces property.
    fn get_value(&self, aref: &AccessibleRef) -> Option<String> {
        // Try Text interface first for text content (text fields, labels, combo boxes).
        // This must come before Value because some AT-SPI adapters (e.g. AccessKit)
        // may expose both interfaces, and Value.CurrentValue returns 0.0 for text elements.
        let text_value = self.get_text_content(aref);
        if text_value.is_some() {
            return text_value;
        }
        // Try Value interface (sliders, progress bars, scroll bars, spinners)
        if let Ok(proxy) = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Value") {
            if let Ok(val) = proxy.get_property::<f64>("CurrentValue") {
                return Some(val.to_string());
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
        self.handle_cache.lock().unwrap().insert(handle, aref);
        handle
    }

    /// Look up a cached AccessibleRef by handle.
    fn get_cached(&self, handle: u64) -> Result<AccessibleRef> {
        self.handle_cache
            .lock()
            .unwrap()
            .get(&handle)
            .cloned()
            .ok_or(Error::ElementStale {
                selector: format!("handle:{}", handle),
            })
    }

    /// Build an ElementData from an AccessibleRef, caching the ref for later lookup.
    fn build_element_data(&self, aref: &AccessibleRef, pid: Option<u32>) -> ElementData {
        let role_name = self.get_role_name(aref).unwrap_or_default();
        let role_num = self.get_role_number(aref).unwrap_or(0);
        let role = {
            let by_name = if !role_name.is_empty() {
                map_atspi_role(&role_name)
            } else {
                Role::Unknown
            };
            let coarse = if by_name != Role::Unknown {
                by_name
            } else {
                // role_name is missing or unmapped — try numeric role.
                // Handles cases where a widget returns a role name string that
                // our table doesn't recognise (e.g. Qt returning "spinbox"
                // instead of the canonical "spin button").
                map_atspi_role_number(role_num)
            };
            // Refine TextArea → TextField for single-line text widgets.
            // Both QLineEdit and QTextEdit use the "text" AT-SPI role; the
            // MULTI_LINE state marks genuinely multi-line widgets. Elements
            // without MULTI_LINE (including QLineEdit) are mapped to TextField.
            // Qt's AT-SPI bridge does not reliably set SINGLE_LINE, so we
            // invert the check: no MULTI_LINE → TextField.
            if coarse == Role::TextArea && !self.is_multi_line(aref) {
                Role::TextField
            } else {
                coarse
            }
        };

        let mut name = self.get_name(aref).ok().filter(|s| !s.is_empty());
        let description = self.get_description(aref).ok().filter(|s| !s.is_empty());

        // Only fetch value/text for roles that have textual content
        let value = if role_has_value(role) {
            let v = self.get_value(aref);
            // For label/static text elements, AT-SPI may put content in the Text
            // interface (returned as value) rather than the Name property.
            if name.is_none() && role == Role::StaticText {
                if let Some(ref v) = v {
                    name = Some(v.clone());
                }
            }
            v
        } else {
            None
        };

        // Application nodes don't have visual bounds
        let bounds = if role != Role::Application {
            self.get_extents(aref)
        } else {
            None
        };
        let states = self.parse_states(aref, role);
        // Only probe action interfaces for interactive roles
        let actions = if role_has_actions(role) {
            self.get_actions(aref)
        } else {
            vec![]
        };

        let raw = {
            let raw_role = if role_name.is_empty() {
                format!("role_num:{}", role_num)
            } else {
                role_name
            };
            xa11y_core::RawPlatformData::Linux {
                atspi_role: raw_role,
                bus_name: aref.bus_name.clone(),
                object_path: aref.path.clone(),
            }
        };

        let (numeric_value, min_value, max_value) = if matches!(
            role,
            Role::Slider | Role::ProgressBar | Role::ScrollBar | Role::SpinButton
        ) {
            if let Ok(proxy) = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Value") {
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
        };

        let handle = self.cache_element(aref.clone());

        ElementData {
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
            pid,
            stable_id: Some(aref.path.clone()),
            raw,
            handle,
        }
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
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Get Parent property failed: {}", e),
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

    /// Parse AT-SPI2 state bitfield into xa11y StateSet.
    fn parse_states(&self, aref: &AccessibleRef, role: Role) -> StateSet {
        let state_bits = self.get_state(aref).unwrap_or_default();

        // AT-SPI2 states are a bitfield across two u32s
        let bits: u64 = if state_bits.len() >= 2 {
            (state_bits[0] as u64) | ((state_bits[1] as u64) << 32)
        } else if state_bits.len() == 1 {
            state_bits[0] as u64
        } else {
            0
        };

        // AT-SPI2 state bit positions (AtspiStateType enum values)
        const BUSY: u64 = 1 << 3;
        const CHECKED: u64 = 1 << 4;
        const EDITABLE: u64 = 1 << 7;
        const ENABLED: u64 = 1 << 8;
        const EXPANDABLE: u64 = 1 << 9;
        const EXPANDED: u64 = 1 << 10;
        const FOCUSABLE: u64 = 1 << 11;
        const FOCUSED: u64 = 1 << 12;
        const MODAL: u64 = 1 << 16;
        const SELECTED: u64 = 1 << 23;
        const SENSITIVE: u64 = 1 << 24;
        const SHOWING: u64 = 1 << 25;
        const VISIBLE: u64 = 1 << 30;
        const INDETERMINATE: u64 = 1 << 32;
        const REQUIRED: u64 = 1 << 33;

        let enabled = (bits & ENABLED) != 0 || (bits & SENSITIVE) != 0;
        let visible = (bits & VISIBLE) != 0 || (bits & SHOWING) != 0;

        let checked = match role {
            Role::CheckBox | Role::RadioButton | Role::MenuItem => {
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

        StateSet {
            enabled,
            visible,
            focused: (bits & FOCUSED) != 0,
            checked,
            selected: (bits & SELECTED) != 0,
            expanded,
            editable: (bits & EDITABLE) != 0,
            focusable: (bits & FOCUSABLE) != 0,
            modal: (bits & MODAL) != 0,
            required: (bits & REQUIRED) != 0,
            busy: (bits & BUSY) != 0,
        }
    }

    /// Find an application by PID.
    fn find_app_by_pid(&self, pid: u32) -> Result<AccessibleRef> {
        let registry = AccessibleRef {
            bus_name: "org.a11y.atspi.Registry".to_string(),
            path: "/org/a11y/atspi/accessible/root".to_string(),
        };
        let children = self.get_atspi_children(&registry)?;

        for child in &children {
            if child.path == "/org/a11y/atspi/null" {
                continue;
            }
            // Try Application.Id first
            if let Ok(proxy) =
                self.make_proxy(&child.bus_name, &child.path, "org.a11y.atspi.Application")
            {
                if let Ok(app_pid) = proxy.get_property::<i32>("Id") {
                    if app_pid as u32 == pid {
                        return Ok(child.clone());
                    }
                }
            }
            // Fall back to D-Bus connection PID
            if let Some(app_pid) = self.get_dbus_pid(&child.bus_name) {
                if app_pid == pid {
                    return Ok(child.clone());
                }
            }
        }

        Err(Error::Platform {
            code: -1,
            message: format!("No application found with PID {}", pid),
        })
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

    /// Perform an AT-SPI action by name.
    fn do_atspi_action(&self, aref: &AccessibleRef, action_name: &str) -> Result<()> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Action")?;
        // NActions may be returned as i32 or u32 depending on AT-SPI implementation.
        let n_actions = proxy
            .get_property::<i32>("NActions")
            .or_else(|_| proxy.get_property::<u32>("NActions").map(|n| n as i32))
            .unwrap_or(0);

        for i in 0..n_actions {
            if let Ok(reply) = proxy.call_method("GetName", &(i,)) {
                if let Ok(name) = reply.body().deserialize::<String>() {
                    // Case-insensitive match to handle implementations that
                    // capitalise action names (e.g. "Press" instead of "press").
                    if name.eq_ignore_ascii_case(action_name) {
                        let _ =
                            proxy
                                .call_method("DoAction", &(i,))
                                .map_err(|e| Error::Platform {
                                    code: -1,
                                    message: format!("DoAction failed: {}", e),
                                })?;
                        return Ok(());
                    }
                }
            }
        }

        Err(Error::Platform {
            code: -1,
            message: format!("Action '{}' not found", action_name),
        })
    }

    /// Get PID from Application interface, falling back to D-Bus connection PID.
    fn get_app_pid(&self, aref: &AccessibleRef) -> Option<u32> {
        // Try Application.Id first
        if let Ok(proxy) = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Application")
        {
            if let Ok(pid) = proxy.get_property::<i32>("Id") {
                if pid > 0 {
                    return Some(pid as u32);
                }
            }
        }

        // Fall back to D-Bus GetConnectionUnixProcessID
        if let Ok(proxy) = self.make_proxy(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        ) {
            if let Ok(reply) =
                proxy.call_method("GetConnectionUnixProcessID", &(aref.bus_name.as_str(),))
            {
                if let Ok(pid) = reply.body().deserialize::<u32>() {
                    if pid > 0 {
                        return Some(pid);
                    }
                }
            }
        }

        None
    }

    /// Resolve the mapped Role for an accessible ref (1-3 D-Bus calls).
    fn resolve_role(&self, aref: &AccessibleRef) -> Role {
        let role_name = self.get_role_name(aref).unwrap_or_default();
        let by_name = if !role_name.is_empty() {
            map_atspi_role(&role_name)
        } else {
            Role::Unknown
        };
        let coarse = if by_name != Role::Unknown {
            by_name
        } else {
            // Unmapped or missing role name — fall back to numeric role.
            let role_num = self.get_role_number(aref).unwrap_or(0);
            map_atspi_role_number(role_num)
        };
        // Refine TextArea → TextField for single-line text widgets.
        if coarse == Role::TextArea && !self.is_multi_line(aref) {
            Role::TextField
        } else {
            coarse
        }
    }

    /// Check if an accessible ref matches a simple selector, fetching only the
    /// attributes the selector actually requires.
    fn matches_ref(
        &self,
        aref: &AccessibleRef,
        simple: &xa11y_core::selector::SimpleSelector,
    ) -> bool {
        // Resolve role only if the selector needs it
        let needs_role =
            simple.role.is_some() || simple.filters.iter().any(|f| f.attr == AttrName::Role);
        let role = if needs_role {
            Some(self.resolve_role(aref))
        } else {
            None
        };

        if let Some(expected) = simple.role {
            if role != Some(expected) {
                return false;
            }
        }

        for filter in &simple.filters {
            let attr_value: Option<String> = match filter.attr {
                AttrName::Role => role.map(|r| r.to_snake_case().to_string()),
                AttrName::Name => {
                    let name = self.get_name(aref).ok().filter(|s| !s.is_empty());
                    // Mirror build_element_data: StaticText may have name in Text interface
                    if name.is_none() && role == Some(Role::StaticText) {
                        self.get_value(aref)
                    } else {
                        name
                    }
                }
                AttrName::Value => self.get_value(aref),
                AttrName::Description => self.get_description(aref).ok().filter(|s| !s.is_empty()),
            };

            let matches = match &filter.op {
                MatchOp::Exact => attr_value.as_deref() == Some(filter.value.as_str()),
                MatchOp::Contains => {
                    let fl = filter.value.to_lowercase();
                    attr_value
                        .as_deref()
                        .is_some_and(|v| v.to_lowercase().contains(&fl))
                }
                MatchOp::StartsWith => {
                    let fl = filter.value.to_lowercase();
                    attr_value
                        .as_deref()
                        .is_some_and(|v| v.to_lowercase().starts_with(&fl))
                }
                MatchOp::EndsWith => {
                    let fl = filter.value.to_lowercase();
                    attr_value
                        .as_deref()
                        .is_some_and(|v| v.to_lowercase().ends_with(&fl))
                }
            };

            if !matches {
                return false;
            }
        }

        true
    }

    /// DFS collect AccessibleRefs matching a SimpleSelector without building
    /// full ElementData. Only the attributes required by the selector are
    /// fetched for each candidate.
    fn collect_matching_refs(
        &self,
        parent: &AccessibleRef,
        simple: &xa11y_core::selector::SimpleSelector,
        depth: u32,
        max_depth: u32,
        results: &mut Vec<AccessibleRef>,
        limit: Option<usize>,
    ) -> Result<()> {
        if depth > max_depth {
            return Ok(());
        }
        if let Some(limit) = limit {
            if results.len() >= limit {
                return Ok(());
            }
        }

        let children = self.get_atspi_children(parent)?;
        for child in children {
            if child.path == "/org/a11y/atspi/null"
                || child.bus_name.is_empty()
                || child.path.is_empty()
            {
                continue;
            }

            // Flatten nested application nodes — Qt/PySide6 apps erroneously list
            // themselves as their own child. Skip the nested application node and
            // recurse directly into its children instead.
            let child_role = self.get_role_name(&child).unwrap_or_default();
            if child_role == "application" {
                let grandchildren = self.get_atspi_children(&child).unwrap_or_default();
                for gc in grandchildren {
                    if gc.path == "/org/a11y/atspi/null"
                        || gc.bus_name.is_empty()
                        || gc.path.is_empty()
                    {
                        continue;
                    }
                    let gc_role = self.get_role_name(&gc).unwrap_or_default();
                    if gc_role == "application" {
                        continue;
                    }
                    if self.matches_ref(&gc, simple) {
                        results.push(gc.clone());
                        if let Some(limit) = limit {
                            if results.len() >= limit {
                                return Ok(());
                            }
                        }
                    }
                    self.collect_matching_refs(&gc, simple, depth + 1, max_depth, results, limit)?;
                }
                continue;
            }

            if self.matches_ref(&child, simple) {
                results.push(child.clone());
                if let Some(limit) = limit {
                    if results.len() >= limit {
                        return Ok(());
                    }
                }
            }

            self.collect_matching_refs(&child, simple, depth + 1, max_depth, results, limit)?;
        }
        Ok(())
    }
}

impl Provider for LinuxProvider {
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        match element {
            None => {
                // Top-level: list all AT-SPI application elements
                let registry = AccessibleRef {
                    bus_name: "org.a11y.atspi.Registry".to_string(),
                    path: "/org/a11y/atspi/accessible/root".to_string(),
                };
                let children = self.get_atspi_children(&registry)?;
                let mut results = Vec::new();

                for child in &children {
                    if child.path == "/org/a11y/atspi/null" {
                        continue;
                    }
                    let app_name = self.get_name(child).unwrap_or_default();
                    if app_name.is_empty() {
                        continue;
                    }
                    let pid = self.get_app_pid(child);
                    let mut data = self.build_element_data(child, pid);
                    // Override name with the app name (more reliable than AT-SPI Name)
                    data.name = Some(app_name);
                    results.push(data);
                }

                Ok(results)
            }
            Some(element_data) => {
                let aref = self.get_cached(element_data.handle)?;
                let children = self.get_atspi_children(&aref).unwrap_or_default();
                let mut results = Vec::new();

                for child_ref in &children {
                    // Skip invalid refs
                    if child_ref.path == "/org/a11y/atspi/null"
                        || child_ref.bus_name.is_empty()
                        || child_ref.path.is_empty()
                    {
                        continue;
                    }
                    // Flatten nested application children — application nodes should only
                    // appear at the top level. Qt/PySide6 apps erroneously list themselves
                    // as their own child; we skip the duplicate but adopt its real children.
                    let child_role = self.get_role_name(child_ref).unwrap_or_default();
                    if child_role == "application" {
                        let grandchildren = self.get_atspi_children(child_ref).unwrap_or_default();
                        for gc_ref in &grandchildren {
                            if gc_ref.path == "/org/a11y/atspi/null"
                                || gc_ref.bus_name.is_empty()
                                || gc_ref.path.is_empty()
                            {
                                continue;
                            }
                            let gc_role = self.get_role_name(gc_ref).unwrap_or_default();
                            if gc_role == "application" {
                                continue;
                            }
                            results.push(self.build_element_data(gc_ref, element_data.pid));
                        }
                        continue;
                    }

                    results.push(self.build_element_data(child_ref, element_data.pid));
                }

                Ok(results)
            }
        }
    }

    fn find_elements(
        &self,
        root: Option<&ElementData>,
        selector: &Selector,
        limit: Option<usize>,
        max_depth: Option<u32>,
    ) -> Result<Vec<ElementData>> {
        if selector.segments.is_empty() {
            return Ok(vec![]);
        }

        let max_depth_val = max_depth.unwrap_or(xa11y_core::MAX_TREE_DEPTH);

        // Phase 1: lightweight ref-based search for first segment.
        // Only the attributes the selector needs are fetched per candidate.
        let first = &selector.segments[0].simple;

        let phase1_limit = if selector.segments.len() == 1 {
            limit
        } else {
            None
        };
        let phase1_limit = match (phase1_limit, first.nth) {
            (Some(l), Some(n)) => Some(l.max(n)),
            (_, Some(n)) => Some(n),
            (l, None) => l,
        };

        // Applications are always direct children of the registry root
        let phase1_depth = if root.is_none() && first.role == Some(Role::Application) {
            0
        } else {
            max_depth_val
        };

        let start_ref = match root {
            None => AccessibleRef {
                bus_name: "org.a11y.atspi.Registry".to_string(),
                path: "/org/a11y/atspi/accessible/root".to_string(),
            },
            Some(el) => self.get_cached(el.handle)?,
        };

        let mut matching_refs = Vec::new();
        self.collect_matching_refs(
            &start_ref,
            first,
            0,
            phase1_depth,
            &mut matching_refs,
            phase1_limit,
        )?;

        let pid_from_root = root.and_then(|r| r.pid);

        // Single-segment: build ElementData only for matches, apply nth/limit
        if selector.segments.len() == 1 {
            if let Some(nth) = first.nth {
                if nth <= matching_refs.len() {
                    let aref = &matching_refs[nth - 1];
                    let pid = if root.is_none() {
                        self.get_app_pid(aref)
                            .or_else(|| self.get_dbus_pid(&aref.bus_name))
                    } else {
                        pid_from_root
                    };
                    return Ok(vec![self.build_element_data(aref, pid)]);
                } else {
                    return Ok(vec![]);
                }
            }

            if let Some(limit) = limit {
                matching_refs.truncate(limit);
            }

            return Ok(matching_refs
                .iter()
                .map(|aref| {
                    let pid = if root.is_none() {
                        self.get_app_pid(aref)
                            .or_else(|| self.get_dbus_pid(&aref.bus_name))
                    } else {
                        pid_from_root
                    };
                    self.build_element_data(aref, pid)
                })
                .collect());
        }

        // Multi-segment: build ElementData for phase 1 matches, then narrow
        // using standard matching on the (small) candidate set.
        let mut candidates: Vec<ElementData> = matching_refs
            .iter()
            .map(|aref| {
                let pid = if root.is_none() {
                    self.get_app_pid(aref)
                        .or_else(|| self.get_dbus_pid(&aref.bus_name))
                } else {
                    pid_from_root
                };
                self.build_element_data(aref, pid)
            })
            .collect();

        for segment in &selector.segments[1..] {
            let mut next_candidates = Vec::new();
            for candidate in &candidates {
                match segment.combinator {
                    Combinator::Child => {
                        let children = self.get_children(Some(candidate))?;
                        for child in children {
                            if xa11y_core::selector::matches_simple(&child, &segment.simple) {
                                next_candidates.push(child);
                            }
                        }
                    }
                    Combinator::Descendant => {
                        let sub_selector = Selector {
                            segments: vec![SelectorSegment {
                                combinator: Combinator::Root,
                                simple: segment.simple.clone(),
                            }],
                        };
                        let mut sub_results = xa11y_core::selector::find_elements_in_tree(
                            |el| self.get_children(el),
                            Some(candidate),
                            &sub_selector,
                            None,
                            Some(max_depth_val),
                        )?;
                        next_candidates.append(&mut sub_results);
                    }
                    Combinator::Root => unreachable!(),
                }
            }
            let mut seen = HashSet::new();
            next_candidates.retain(|e| seen.insert(e.handle));
            candidates = next_candidates;
        }

        // Apply :nth on last segment
        if let Some(nth) = selector.segments.last().and_then(|s| s.simple.nth) {
            if nth <= candidates.len() {
                candidates = vec![candidates.remove(nth - 1)];
            } else {
                candidates.clear();
            }
        }

        if let Some(limit) = limit {
            candidates.truncate(limit);
        }

        Ok(candidates)
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

    fn perform_action(
        &self,
        element: &ElementData,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        let target = self.get_cached(element.handle)?;

        match action {
            Action::Press => self
                .do_atspi_action(&target, "click")
                .or_else(|_| self.do_atspi_action(&target, "activate"))
                .or_else(|_| self.do_atspi_action(&target, "press"))
                // Qt radio buttons expose "toggle" or "check" rather than
                // "press" as their primary action name.
                .or_else(|_| self.do_atspi_action(&target, "toggle"))
                .or_else(|_| self.do_atspi_action(&target, "check")),
            Action::Focus => {
                // Try Component.GrabFocus first, then fall back to Action interface
                if let Ok(proxy) =
                    self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Component")
                {
                    if proxy.call_method("GrabFocus", &()).is_ok() {
                        return Ok(());
                    }
                }
                self.do_atspi_action(&target, "focus")
                    .or_else(|_| self.do_atspi_action(&target, "setFocus"))
            }
            Action::SetValue => match data {
                Some(ActionData::NumericValue(v)) => {
                    let proxy =
                        self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Value")?;
                    proxy
                        .set_property("CurrentValue", v)
                        .map_err(|e| Error::Platform {
                            code: -1,
                            message: format!("SetValue failed: {}", e),
                        })
                }
                Some(ActionData::Value(text)) => {
                    let proxy = self
                        .make_proxy(
                            &target.bus_name,
                            &target.path,
                            "org.a11y.atspi.EditableText",
                        )
                        .map_err(|_| Error::TextValueNotSupported)?;
                    let _ = proxy.call_method("DeleteText", &(0i32, i32::MAX));
                    proxy
                        .call_method("InsertText", &(0i32, &*text, text.len() as i32))
                        .map_err(|_| Error::TextValueNotSupported)?;
                    Ok(())
                }
                _ => Err(Error::Platform {
                    code: -1,
                    message: "SetValue requires ActionData".to_string(),
                }),
            },
            Action::Toggle => self
                .do_atspi_action(&target, "toggle")
                .or_else(|_| self.do_atspi_action(&target, "click"))
                .or_else(|_| self.do_atspi_action(&target, "activate")),
            Action::Expand => self
                .do_atspi_action(&target, "expand")
                .or_else(|_| self.do_atspi_action(&target, "open")),
            Action::Collapse => self
                .do_atspi_action(&target, "collapse")
                .or_else(|_| self.do_atspi_action(&target, "close")),
            Action::Select => self.do_atspi_action(&target, "select"),
            Action::ShowMenu => self
                .do_atspi_action(&target, "menu")
                .or_else(|_| self.do_atspi_action(&target, "showmenu")),
            Action::ScrollIntoView => {
                let proxy =
                    self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Component")?;
                proxy
                    .call_method("ScrollTo", &(0u32,))
                    .map_err(|e| Error::Platform {
                        code: -1,
                        message: format!("ScrollTo failed: {}", e),
                    })?;
                Ok(())
            }
            Action::Increment => self.do_atspi_action(&target, "increment").or_else(|_| {
                // Fall back to Value interface: current + step (or +1)
                let proxy =
                    self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Value")?;
                let current: f64 =
                    proxy
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
            }),
            Action::Decrement => self.do_atspi_action(&target, "decrement").or_else(|_| {
                let proxy =
                    self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Value")?;
                let current: f64 =
                    proxy
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
            }),
            Action::Blur => {
                // Grab focus on parent element to blur the current one
                if let Ok(Some(parent_ref)) = self.get_atspi_parent(&target) {
                    if parent_ref.path != "/org/a11y/atspi/null" {
                        if let Ok(p) = self.make_proxy(
                            &parent_ref.bus_name,
                            &parent_ref.path,
                            "org.a11y.atspi.Component",
                        ) {
                            let _ = p.call_method("GrabFocus", &());
                            return Ok(());
                        }
                    }
                }
                Ok(())
            }

            Action::ScrollDown | Action::ScrollRight => {
                let amount = match data {
                    Some(ActionData::ScrollAmount(amount)) => amount,
                    _ => {
                        return Err(Error::Platform {
                            code: -1,
                            message: "Scroll requires ActionData::ScrollAmount".to_string(),
                        })
                    }
                };
                let is_vertical = matches!(action, Action::ScrollDown);
                let (pos_name, neg_name) = if is_vertical {
                    ("scroll down", "scroll up")
                } else {
                    ("scroll right", "scroll left")
                };
                let action_name = if amount >= 0.0 { pos_name } else { neg_name };
                // Repeat scroll action for each logical unit (AT-SPI has no scroll-by-amount)
                let count = (amount.abs() as u32).max(1);
                for _ in 0..count {
                    if self.do_atspi_action(&target, action_name).is_err() {
                        // Fall back to Component.ScrollTo (single call, not repeatable)
                        let proxy = self.make_proxy(
                            &target.bus_name,
                            &target.path,
                            "org.a11y.atspi.Component",
                        )?;
                        let scroll_type: u32 = if is_vertical {
                            if amount >= 0.0 {
                                3
                            } else {
                                2
                            } // BOTTOM_EDGE / TOP_EDGE
                        } else if amount >= 0.0 {
                            5
                        } else {
                            4
                        }; // RIGHT_EDGE / LEFT_EDGE
                        proxy
                            .call_method("ScrollTo", &(scroll_type,))
                            .map_err(|e| Error::Platform {
                                code: -1,
                                message: format!("ScrollTo failed: {}", e),
                            })?;
                        return Ok(());
                    }
                }
                Ok(())
            }

            Action::SetTextSelection => {
                let (start, end) = match data {
                    Some(ActionData::TextSelection { start, end }) => (start, end),
                    _ => {
                        return Err(Error::Platform {
                            code: -1,
                            message: "SetTextSelection requires ActionData::TextSelection"
                                .to_string(),
                        })
                    }
                };
                let proxy =
                    self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Text")?;
                // Try SetSelection first, fall back to AddSelection
                if proxy
                    .call_method("SetSelection", &(0i32, start as i32, end as i32))
                    .is_err()
                {
                    proxy
                        .call_method("AddSelection", &(start as i32, end as i32))
                        .map_err(|e| Error::Platform {
                            code: -1,
                            message: format!("Text.AddSelection failed: {}", e),
                        })?;
                }
                Ok(())
            }

            Action::TypeText => {
                let text = match data {
                    Some(ActionData::Value(text)) => text,
                    _ => {
                        return Err(Error::Platform {
                            code: -1,
                            message: "TypeText requires ActionData::Value".to_string(),
                        })
                    }
                };
                // Insert text via EditableText interface (accessibility API, not input simulation).
                // Get cursor position from Text interface, then insert at that position.
                let text_proxy =
                    self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Text");
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
                proxy
                    .call_method("InsertText", &(pos, &*text, text.len() as i32))
                    .map_err(|e| Error::Platform {
                        code: -1,
                        message: format!("EditableText.InsertText failed: {}", e),
                    })?;
                Ok(())
            }
        }
    }

    fn subscribe(&self, element: &ElementData) -> Result<Subscription> {
        let pid = element.pid.ok_or(Error::Platform {
            code: -1,
            message: "Element has no PID for subscribe".to_string(),
        })?;
        let app_name = element.name.clone().unwrap_or_default();
        self.subscribe_impl(app_name, pid, pid)
    }
}

// ── Event subscription ──────────────────────────────────────────────────────

impl LinuxProvider {
    /// Spawn a polling thread that detects focus and structure changes.
    fn subscribe_impl(&self, app_name: String, app_pid: u32, pid: u32) -> Result<Subscription> {
        let (tx, rx) = std::sync::mpsc::channel();
        let poll_provider = LinuxProvider::new()?;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop.clone();

        let handle = std::thread::spawn(move || {
            let mut prev_focused: Option<String> = None;
            let mut prev_element_count: usize = 0;

            while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));

                // Find the app element by PID
                let app_ref = match poll_provider.find_app_by_pid(pid) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let app_data = poll_provider.build_element_data(&app_ref, Some(pid));

                // Walk the tree lazily to find focused element and count
                let mut stack = vec![app_data];
                let mut element_count: usize = 0;
                let mut focused_element: Option<ElementData> = None;
                let mut visited = HashSet::new();

                while let Some(el) = stack.pop() {
                    let path_key = format!("{:?}:{}", el.raw, el.handle);
                    if !visited.insert(path_key) {
                        continue;
                    }
                    element_count += 1;
                    if el.states.focused && focused_element.is_none() {
                        focused_element = Some(el.clone());
                    }
                    if let Ok(children) = poll_provider.get_children(Some(&el)) {
                        stack.extend(children);
                    }
                }

                let focused_name = focused_element.as_ref().and_then(|e| e.name.clone());
                if focused_name != prev_focused {
                    if prev_focused.is_some() {
                        let _ = tx.send(Event {
                            event_type: EventType::FocusChanged,
                            app_name: app_name.clone(),
                            app_pid,
                            target: focused_element,
                            state_flag: None,
                            state_value: None,
                            text_change: None,
                            timestamp: std::time::Instant::now(),
                        });
                    }
                    prev_focused = focused_name;
                }

                if element_count != prev_element_count && prev_element_count > 0 {
                    let _ = tx.send(Event {
                        event_type: EventType::StructureChanged,
                        app_name: app_name.clone(),
                        app_pid,
                        target: None,
                        state_flag: None,
                        state_value: None,
                        text_change: None,
                        timestamp: std::time::Instant::now(),
                    });
                }
                prev_element_count = element_count;
            }
        });

        let cancel = CancelHandle::new(move || {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = handle.join();
        });

        Ok(Subscription::new(EventReceiver::new(rx), cancel))
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

/// Map AT-SPI2 role name to xa11y Role.
fn map_atspi_role(role_name: &str) -> Role {
    match role_name.to_lowercase().as_str() {
        "application" => Role::Application,
        "window" | "frame" => Role::Window,
        "dialog" | "file chooser" => Role::Dialog,
        "alert" | "notification" => Role::Alert,
        "push button" | "push button menu" => Role::Button,
        "check box" | "check menu item" => Role::CheckBox,
        "radio button" | "radio menu item" => Role::RadioButton,
        "entry" | "password text" => Role::TextField,
        "spin button" => Role::SpinButton,
        "text" => Role::TextArea,
        "label" | "static" | "caption" => Role::StaticText,
        "combo box" => Role::ComboBox,
        "list" | "list box" => Role::List,
        "list item" => Role::ListItem,
        "menu" => Role::Menu,
        "menu item" | "tearoff menu item" => Role::MenuItem,
        "menu bar" => Role::MenuBar,
        "page tab" => Role::Tab,
        "page tab list" => Role::TabGroup,
        "table" | "tree table" => Role::Table,
        "table row" => Role::TableRow,
        "table cell" | "table column header" | "table row header" => Role::TableCell,
        "tool bar" => Role::Toolbar,
        "scroll bar" => Role::ScrollBar,
        "slider" => Role::Slider,
        "image" | "icon" | "desktop icon" => Role::Image,
        "link" => Role::Link,
        "panel" | "section" | "form" | "filler" | "viewport" | "scroll pane" => Role::Group,
        "progress bar" => Role::ProgressBar,
        "tree item" => Role::TreeItem,
        "document web" | "document frame" => Role::WebArea,
        "heading" => Role::Heading,
        "separator" => Role::Separator,
        "split pane" => Role::SplitGroup,
        "tooltip" | "tool tip" => Role::Tooltip,
        "status bar" | "statusbar" => Role::Status,
        "landmark" | "navigation" => Role::Navigation,
        _ => Role::Unknown,
    }
}

/// Map AT-SPI2 numeric role (AtspiRole enum) to xa11y Role.
/// Values from atspi-common Role enum (repr(u32)).
fn map_atspi_role_number(role: u32) -> Role {
    match role {
        2 => Role::Alert,        // Alert
        7 => Role::CheckBox,     // CheckBox
        8 => Role::CheckBox,     // CheckMenuItem
        11 => Role::ComboBox,    // ComboBox
        16 => Role::Dialog,      // Dialog
        19 => Role::Dialog,      // FileChooser
        20 => Role::Group,       // Filler
        23 => Role::Window,      // Frame
        26 => Role::Image,       // Icon
        27 => Role::Image,       // Image
        29 => Role::StaticText,  // Label
        31 => Role::List,        // List
        32 => Role::ListItem,    // ListItem
        33 => Role::Menu,        // Menu
        34 => Role::MenuBar,     // MenuBar
        35 => Role::MenuItem,    // MenuItem
        37 => Role::Tab,         // PageTab
        38 => Role::TabGroup,    // PageTabList
        39 => Role::Group,       // Panel
        40 => Role::TextField,   // PasswordText
        42 => Role::ProgressBar, // ProgressBar
        43 => Role::Button,      // Button (push button)
        44 => Role::RadioButton, // RadioButton
        45 => Role::RadioButton, // RadioMenuItem
        48 => Role::ScrollBar,   // ScrollBar
        49 => Role::Group,       // ScrollPane
        50 => Role::Separator,   // Separator
        51 => Role::Slider,      // Slider
        52 => Role::SpinButton,  // SpinButton
        53 => Role::SplitGroup,  // SplitPane
        55 => Role::Table,       // Table
        56 => Role::TableCell,   // TableCell
        57 => Role::TableCell,   // TableColumnHeader
        58 => Role::TableCell,   // TableRowHeader
        61 => Role::TextArea,    // Text
        62 => Role::Button,      // ToggleButton
        63 => Role::Toolbar,     // ToolBar
        65 => Role::Group,       // Tree
        66 => Role::Table,       // TreeTable
        67 => Role::Unknown,     // Unknown
        68 => Role::Group,       // Viewport
        69 => Role::Window,      // Window
        75 => Role::Application, // Application
        79 => Role::TextField,   // Entry
        82 => Role::WebArea,     // DocumentFrame
        83 => Role::Heading,     // Heading
        85 => Role::Group,       // Section
        86 => Role::Group,       // RedundantObject
        87 => Role::Group,       // Form
        88 => Role::Link,        // Link
        90 => Role::TableRow,    // TableRow
        91 => Role::TreeItem,    // TreeItem
        95 => Role::WebArea,     // DocumentWeb
        98 => Role::List,        // ListBox
        93 => Role::Tooltip,     // Tooltip
        97 => Role::Status,      // StatusBar
        101 => Role::Alert,      // Notification
        116 => Role::StaticText, // Static
        129 => Role::Button,     // PushButtonMenu
        _ => Role::Unknown,
    }
}

/// Map AT-SPI2 action name to xa11y Action.
fn map_atspi_action(action_name: &str) -> Option<Action> {
    match action_name.to_lowercase().as_str() {
        "click" | "activate" | "press" | "invoke" => Some(Action::Press),
        "toggle" | "check" | "uncheck" => Some(Action::Toggle),
        "expand" | "open" => Some(Action::Expand),
        "collapse" | "close" => Some(Action::Collapse),
        "select" => Some(Action::Select),
        "menu" | "showmenu" | "popup" | "show menu" => Some(Action::ShowMenu),
        "increment" => Some(Action::Increment),
        "decrement" => Some(Action::Decrement),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_mapping() {
        assert_eq!(map_atspi_role("push button"), Role::Button);
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
    fn test_action_mapping() {
        assert_eq!(map_atspi_action("click"), Some(Action::Press));
        assert_eq!(map_atspi_action("activate"), Some(Action::Press));
        assert_eq!(map_atspi_action("toggle"), Some(Action::Toggle));
        assert_eq!(map_atspi_action("expand"), Some(Action::Expand));
        assert_eq!(map_atspi_action("collapse"), Some(Action::Collapse));
        assert_eq!(map_atspi_action("select"), Some(Action::Select));
        assert_eq!(map_atspi_action("increment"), Some(Action::Increment));
        assert_eq!(map_atspi_action("foobar"), None);
    }
}
