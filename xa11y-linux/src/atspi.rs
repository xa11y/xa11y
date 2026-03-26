//! Real AT-SPI2 backend implementation using zbus D-Bus bindings.

use std::sync::Mutex;
use std::time::Duration;

use xa11y_core::{
    Action, ActionData, AppInfo, AppTarget, CancelHandle, ElementState, Error, Event, EventFilter,
    EventKind, EventProvider, EventReceiver, Node, NormalizedRect, PermissionStatus, Provider,
    QueryOptions, Rect, Result, Role, ScrollDirection, StateSet, Subscription, Toggled, Tree,
};
use zbus::blocking::{Connection, Proxy};

/// Linux accessibility provider using AT-SPI2 over D-Bus.
pub struct LinuxProvider {
    a11y_bus: Connection,
    /// Cached AT-SPI accessible refs for action dispatch (keyed by node index).
    cached_refs: Mutex<Vec<AccessibleRef>>,
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
            cached_refs: Mutex::new(Vec::new()),
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
        Proxy::new(
            &self.a11y_bus,
            bus_name.to_owned(),
            path.to_owned(),
            interface.to_owned(),
        )
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
    fn get_children(&self, aref: &AccessibleRef) -> Result<Vec<AccessibleRef>> {
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
            if let Ok(n_actions) = proxy.get_property::<i32>("NActions") {
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
        // may expose both interfaces, and Value.CurrentValue returns 0.0 for text nodes.
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

    /// Traverse the accessibility tree rooted at `aref`, building nodes.
    #[allow(clippy::too_many_arguments)]
    fn traverse(
        &self,
        aref: &AccessibleRef,
        opts: &QueryOptions,
        nodes: &mut Vec<Node>,
        refs: &mut Vec<AccessibleRef>,
        parent_idx: Option<u32>,
        depth: u32,
        screen_size: (u32, u32),
    ) {
        if let Some(max_depth) = opts.max_depth {
            if depth > max_depth {
                return;
            }
        }
        if let Some(max_elements) = opts.max_elements {
            if nodes.len() >= max_elements as usize {
                return;
            }
        }

        let role_name = self.get_role_name(aref).unwrap_or_default();
        let role_num = self.get_role_number(aref).unwrap_or(0);
        let role = if !role_name.is_empty() {
            map_atspi_role(&role_name)
        } else {
            map_atspi_role_number(role_num)
        };

        // Don't apply role/visibility filters to the root node (depth 0)
        // so the tree always has at least the application node.
        let is_root = depth == 0;

        // For role filtering, skip adding this node but still traverse children
        // so descendant nodes matching the filter can be found.
        let skip_for_role = if !is_root {
            if let Some(ref filter_roles) = opts.roles {
                !filter_roles.contains(&role)
            } else {
                false
            }
        } else {
            false
        };

        // If role-filtered, skip this node but still traverse children
        // so that descendant nodes matching the filter can be found.
        if skip_for_role {
            let children = self.get_children(aref).unwrap_or_default();
            for child_ref in &children {
                if let Some(max_elements) = opts.max_elements {
                    if nodes.len() >= max_elements as usize {
                        break;
                    }
                }
                if child_ref.path == "/org/a11y/atspi/null"
                    || child_ref.bus_name.is_empty()
                    || child_ref.path.is_empty()
                {
                    continue;
                }
                self.traverse(
                    child_ref,
                    opts,
                    nodes,
                    refs,
                    parent_idx,
                    depth + 1,
                    screen_size,
                );
            }
            return;
        }

        let mut name = self.get_name(aref).ok().filter(|s| !s.is_empty());
        let description = self.get_description(aref).ok().filter(|s| !s.is_empty());
        let value = self.get_value(aref);

        // For label/static text nodes, AT-SPI may put content in the Text interface
        // (returned as value) rather than the Name property. Use it as the name.
        if name.is_none() && role == Role::StaticText {
            if let Some(ref v) = value {
                name = Some(v.clone());
            }
        }
        let bounds = self.get_extents(aref);
        let states = self.parse_states(aref, role);
        let actions = self.get_actions(aref);

        if !is_root && opts.visible_only && !states.visible {
            return;
        }

        let bounds_normalized = bounds.map(|b| {
            let (sw, sh) = screen_size;
            if sw == 0 || sh == 0 {
                return NormalizedRect {
                    left: 0.0,
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
                };
            }
            NormalizedRect {
                left: b.x as f64 / sw as f64,
                top: b.y as f64 / sh as f64,
                right: (b.x as f64 + b.width as f64) / sw as f64,
                bottom: (b.y as f64 + b.height as f64) / sh as f64,
            }
        });

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

        let node_idx = nodes.len() as u32;
        nodes.push(Node {
            role,
            name,
            value,
            description,
            bounds,
            bounds_normalized,
            actions,
            states,
            numeric_value,
            min_value,
            max_value,
            stable_id: Some(aref.path.clone()),
            raw,
            index: node_idx,
            children_indices: vec![], // filled in below
            parent_index: parent_idx,
        });
        refs.push(aref.clone());

        // Get children
        let children = self.get_children(aref).unwrap_or_default();
        let mut child_ids = Vec::new();

        for child_ref in &children {
            if let Some(max_elements) = opts.max_elements {
                if nodes.len() >= max_elements as usize {
                    break;
                }
            }
            // Skip invalid refs
            if child_ref.path == "/org/a11y/atspi/null"
                || child_ref.bus_name.is_empty()
                || child_ref.path.is_empty()
            {
                continue;
            }
            let child_idx = nodes.len() as u32;
            child_ids.push(child_idx);
            self.traverse(
                child_ref,
                opts,
                nodes,
                refs,
                Some(node_idx),
                depth + 1,
                screen_size,
            );
        }

        // Update children list
        nodes[node_idx as usize].children_indices = child_ids;
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

    /// Get screen size.
    fn detect_screen_size() -> (u32, u32) {
        if let Ok(output) = std::process::Command::new("xdpyinfo").output() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("dimensions:") {
                    if let Some(dims) = trimmed.split_whitespace().nth(1) {
                        let parts: Vec<&str> = dims.split('x').collect();
                        if parts.len() == 2 {
                            if let (Ok(w), Ok(h)) = (parts[0].parse(), parts[1].parse()) {
                                return (w, h);
                            }
                        }
                    }
                }
            }
        }
        (1920, 1080)
    }

    /// Find an application by name.
    fn find_app_by_name(&self, name: &str) -> Result<AccessibleRef> {
        let registry = AccessibleRef {
            bus_name: "org.a11y.atspi.Registry".to_string(),
            path: "/org/a11y/atspi/accessible/root".to_string(),
        };
        let children = self.get_children(&registry)?;
        let name_lower = name.to_lowercase();

        for child in &children {
            if child.path == "/org/a11y/atspi/null" {
                continue;
            }
            if let Ok(app_name) = self.get_name(child) {
                if app_name.to_lowercase().contains(&name_lower) {
                    return Ok(child.clone());
                }
            }
        }

        Err(Error::AppNotFound {
            target: name.to_string(),
        })
    }

    /// Find an application by PID.
    fn find_app_by_pid(&self, pid: u32) -> Result<AccessibleRef> {
        let registry = AccessibleRef {
            bus_name: "org.a11y.atspi.Registry".to_string(),
            path: "/org/a11y/atspi/accessible/root".to_string(),
        };
        let children = self.get_children(&registry)?;

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

        Err(Error::AppNotFound {
            target: format!("PID {}", pid),
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
        let n_actions: i32 = proxy.get_property("NActions").unwrap_or(0);

        for i in 0..n_actions {
            if let Ok(reply) = proxy.call_method("GetName", &(i,)) {
                if let Ok(name) = reply.body().deserialize::<String>() {
                    if name == action_name {
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
}

impl Provider for LinuxProvider {
    fn get_app_tree(&self, target: &AppTarget, opts: &QueryOptions) -> Result<Tree> {
        let app_ref = match target {
            AppTarget::ByName(name) => self.find_app_by_name(name)?,
            AppTarget::ByPid(pid) => self.find_app_by_pid(*pid)?,
            AppTarget::ByWindow(_) => {
                return Err(Error::Platform {
                    code: -1,
                    message: "ByWindow not supported on Linux AT-SPI2".to_string(),
                });
            }
        };

        let app_name = self.get_name(&app_ref).unwrap_or_default();
        let screen_size = Self::detect_screen_size();
        let mut nodes = Vec::new();
        let mut refs = Vec::new();

        self.traverse(&app_ref, opts, &mut nodes, &mut refs, None, 0, screen_size);

        if nodes.is_empty() {
            return Err(Error::AppNotFound {
                target: format!("{:?}", target),
            });
        }

        // Cache refs for action dispatch
        *self.cached_refs.lock().unwrap() = refs;

        let pid = self.get_app_pid(&app_ref);

        Ok(Tree::new(app_name, pid, screen_size, nodes, opts.clone()))
    }

    fn get_all_apps(&self, opts: &QueryOptions) -> Result<Tree> {
        let screen_size = Self::detect_screen_size();
        let mut nodes = Vec::new();

        nodes.push(Node {
            role: Role::Application,
            name: Some("Desktop".to_string()),
            value: None,
            description: None,
            bounds: Some(Rect {
                x: 0,
                y: 0,
                width: screen_size.0,
                height: screen_size.1,
            }),
            bounds_normalized: Some(NormalizedRect {
                left: 0.0,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            }),
            actions: vec![],
            states: StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            raw: xa11y_core::RawPlatformData::Synthetic,
            index: 0,
            children_indices: vec![],
            parent_index: None,
        });

        let mut refs = Vec::new();
        refs.push(AccessibleRef {
            bus_name: String::new(),
            path: String::new(),
        }); // placeholder for desktop root

        let registry = AccessibleRef {
            bus_name: "org.a11y.atspi.Registry".to_string(),
            path: "/org/a11y/atspi/accessible/root".to_string(),
        };
        let children = self.get_children(&registry).unwrap_or_default();
        let mut root_children = Vec::new();

        for child in &children {
            if child.path == "/org/a11y/atspi/null" {
                continue;
            }
            let app_name = self.get_name(child).unwrap_or_default();
            if app_name.is_empty() {
                continue;
            }
            let child_idx = nodes.len() as u32;
            root_children.push(child_idx);
            self.traverse(child, opts, &mut nodes, &mut refs, Some(0), 1, screen_size);
        }

        nodes[0].children_indices = root_children;

        *self.cached_refs.lock().unwrap() = refs;

        Ok(Tree::new(
            "Desktop".to_string(),
            None,
            screen_size,
            nodes,
            opts.clone(),
        ))
    }

    fn perform_action(
        &self,
        tree: &Tree,
        node: &Node,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        let node_idx = tree.node_index(node);

        // Look up cached accessible ref for action dispatch
        let cache = self.cached_refs.lock().unwrap();
        let target = cache
            .get(node_idx as usize)
            .ok_or(Error::ElementStale {
                selector: format!("index:{}", node_idx),
            })?
            .clone();
        drop(cache);

        match action {
            Action::Press => self
                .do_atspi_action(&target, "click")
                .or_else(|_| self.do_atspi_action(&target, "activate"))
                .or_else(|_| self.do_atspi_action(&target, "press")),
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
                let proxy =
                    self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Accessible")?;
                if let Ok(reply) = proxy.call_method("GetParent", &()) {
                    if let Ok((bus, path)) = reply
                        .body()
                        .deserialize::<(String, zbus::zvariant::OwnedObjectPath)>()
                    {
                        let path_str = path.as_str();
                        if path_str != "/org/a11y/atspi/null" {
                            if let Ok(p) =
                                self.make_proxy(&bus, path_str, "org.a11y.atspi.Component")
                            {
                                let _ = p.call_method("GrabFocus", &());
                                return Ok(());
                            }
                        }
                    }
                }
                Ok(())
            }

            Action::Scroll => {
                let (direction, amount) = match data {
                    Some(ActionData::ScrollAmount { direction, amount }) => (direction, amount),
                    _ => {
                        return Err(Error::Platform {
                            code: -1,
                            message: "Scroll requires ActionData::ScrollAmount".to_string(),
                        })
                    }
                };
                // Repeat scroll action for each logical unit (AT-SPI has no scroll-by-amount)
                let count = (amount.abs() as u32).max(1);
                let action_name = match direction {
                    ScrollDirection::Up => "scroll up",
                    ScrollDirection::Down => "scroll down",
                    ScrollDirection::Left => "scroll left",
                    ScrollDirection::Right => "scroll right",
                };
                for _ in 0..count {
                    if self.do_atspi_action(&target, action_name).is_err() {
                        // Fall back to Component.ScrollTo (single call, not repeatable)
                        let proxy = self.make_proxy(
                            &target.bus_name,
                            &target.path,
                            "org.a11y.atspi.Component",
                        )?;
                        let scroll_type: u32 = match direction {
                            ScrollDirection::Up => 2,    // TOP_EDGE
                            ScrollDirection::Down => 3,  // BOTTOM_EDGE
                            ScrollDirection::Left => 4,  // LEFT_EDGE
                            ScrollDirection::Right => 5, // RIGHT_EDGE
                        };
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

    fn check_permissions(&self) -> Result<PermissionStatus> {
        let registry = AccessibleRef {
            bus_name: "org.a11y.atspi.Registry".to_string(),
            path: "/org/a11y/atspi/accessible/root".to_string(),
        };
        match self.get_children(&registry) {
            Ok(_) => Ok(PermissionStatus::Granted),
            Err(_) => Ok(PermissionStatus::Denied {
                instructions:
                    "Enable accessibility: gsettings set org.gnome.desktop.interface toolkit-accessibility true\nEnsure at-spi2-core is installed."
                        .to_string(),
            }),
        }
    }

    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        let registry = AccessibleRef {
            bus_name: "org.a11y.atspi.Registry".to_string(),
            path: "/org/a11y/atspi/accessible/root".to_string(),
        };
        let children = self.get_children(&registry)?;
        let mut apps = Vec::new();

        for child in &children {
            if child.path == "/org/a11y/atspi/null" {
                continue;
            }
            let name = self.get_name(child).unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let pid = self.get_app_pid(child);
            apps.push(AppInfo {
                name,
                pid: pid.unwrap_or(0),
                bundle_id: None,
            });
        }

        Ok(apps)
    }
}

// ── EventProvider ────────────────────────────────────────────────────────────

impl EventProvider for LinuxProvider {
    fn subscribe(&self, target: &AppTarget, filter: EventFilter) -> Result<Subscription> {
        let (tx, rx) = std::sync::mpsc::channel();

        let app_info = match target {
            AppTarget::ByName(name) => {
                let app_ref = self.find_app_by_name(name)?;
                let pid = self.get_app_pid(&app_ref).unwrap_or(0);
                AppInfo {
                    name: self.get_name(&app_ref).unwrap_or_default(),
                    pid,
                    bundle_id: None,
                }
            }
            AppTarget::ByPid(pid) => {
                let app_ref = self.find_app_by_pid(*pid)?;
                AppInfo {
                    name: self.get_name(&app_ref).unwrap_or_default(),
                    pid: *pid,
                    bundle_id: None,
                }
            }
            AppTarget::ByWindow(_) => {
                return Err(Error::Platform {
                    code: -1,
                    message: "ByWindow not supported for event subscription".to_string(),
                })
            }
        };

        // Create a separate provider for polling on the background thread
        let poll_provider = LinuxProvider::new()?;
        let target_clone = target.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop.clone();

        // Poll for tree changes on a background thread, emitting events for diffs
        let handle = std::thread::spawn(move || {
            let mut prev_focused: Option<String> = None;
            let mut prev_node_count: usize = 0;

            while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));

                let tree = match poll_provider.get_app_tree(&target_clone, &QueryOptions::default())
                {
                    Ok(t) => t,
                    Err(_) => continue,
                };

                // Detect focus changes
                let focused_name = tree
                    .iter()
                    .find(|n| n.states.focused)
                    .and_then(|n| n.name.clone());
                if focused_name != prev_focused {
                    if prev_focused.is_some() {
                        let kind = EventKind::FocusChanged;
                        if filter.kinds.is_empty() || filter.kinds.contains(&kind) {
                            let _ = tx.send(Event {
                                kind,
                                app: app_info.clone(),
                                target: tree.iter().find(|n| n.states.focused).cloned(),
                                state_flag: None,
                                state_value: None,
                                text_change: None,
                                timestamp: std::time::Instant::now(),
                            });
                        }
                    }
                    prev_focused = focused_name;
                }

                // Detect structure changes (node count changed)
                let node_count = tree.len();
                if node_count != prev_node_count && prev_node_count > 0 {
                    let kind = EventKind::StructureChanged;
                    if filter.kinds.is_empty() || filter.kinds.contains(&kind) {
                        let _ = tx.send(Event {
                            kind,
                            app: app_info.clone(),
                            target: None,
                            state_flag: None,
                            state_value: None,
                            text_change: None,
                            timestamp: std::time::Instant::now(),
                        });
                    }
                }
                prev_node_count = node_count;
            }
        });

        let cancel = CancelHandle::new(move || {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = handle.join();
        });

        Ok(Subscription::new(EventReceiver::new(rx), cancel))
    }

    fn wait_for_event(
        &self,
        target: &AppTarget,
        filter: EventFilter,
        timeout: Duration,
    ) -> Result<Event> {
        let sub = self.subscribe(target, filter)?;
        let start = std::time::Instant::now();
        loop {
            if let Some(event) = sub.try_recv() {
                return Ok(event);
            }
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(Error::Timeout { elapsed });
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for(
        &self,
        target: &AppTarget,
        selector: &str,
        state: ElementState,
        timeout: Duration,
    ) -> Result<Node> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(100);

        loop {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(Error::Timeout { elapsed });
            }

            let tree = self.get_app_tree(target, &QueryOptions::default())?;
            let matches = tree.query(selector).ok();
            let node = matches.as_ref().and_then(|m| m.first().copied());

            if state.is_met(node) {
                return Ok(node.cloned().unwrap_or_else(Node::synthetic_empty));
            }

            std::thread::sleep(poll_interval);
        }
    }
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
