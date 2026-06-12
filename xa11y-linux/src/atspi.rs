//! Real AT-SPI2 backend implementation using zbus D-Bus bindings.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use rayon::prelude::*;
use xa11y_core::{
    ElementData, Error, Provider, Rect, Result, Role, StateSet, Subscription, Toggled,
};
use zbus::blocking::{Connection, Proxy};

/// Global handle counter for mapping ElementData back to AccessibleRefs.
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Format a normalized state attribute as the same string `xa11y_core::selector::resolve_attr`
/// would produce, so fast-path matching agrees byte-for-byte with the slow path.
fn state_attr_to_string(name: &str, s: &StateSet) -> Option<String> {
    match name {
        "enabled" => Some(s.enabled.to_string()),
        "visible" => Some(s.visible.to_string()),
        "focused" => Some(s.focused.to_string()),
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
    a11y_bus: Connection,
    /// Cached AT-SPI accessible refs keyed by handle ID.
    handle_cache: Mutex<HashMap<u64, AccessibleRef>>,
    /// Cached AT-SPI2 action indices keyed by element handle.
    /// Maps each action name (snake_case) to the integer index used by `DoAction(i)`.
    action_indices: Mutex<HashMap<u64, HashMap<String, i32>>>,
}

/// AT-SPI2 accessible reference: (bus_name, object_path).
#[derive(Debug, Clone)]
pub(crate) struct AccessibleRef {
    pub(crate) bus_name: String,
    pub(crate) path: String,
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
            action_indices: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn connect_a11y_bus() -> Result<Connection> {
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
                Relaunch with `--force-renderer-accessibility` (or set the env var \
                `ACCESSIBILITY_ENABLED=1`) so the renderer accessibility bridge is initialised."
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
    /// unaffected.
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
                map_atspi_role_number(role_num)
            };
            self.refine_text_role(coarse, aref)
        };

        // Fetch all independent properties in parallel.
        // Left tree: (name+value, description)
        // Right tree: ((states, bounds), (actions, numeric_values))
        let (
            ((mut name, value), description),
            (
                (states, bounds),
                ((actions, action_index_map), (numeric_value, min_value, max_value)),
            ),
        ) = rayon::join(
            || {
                rayon::join(
                    || {
                        let name = self.get_name(aref).ok().filter(|s| !s.is_empty());
                        let value = if role_has_value(role) {
                            self.get_value(aref)
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
                            || self.parse_states(aref, role),
                            || {
                                if role != Role::Application {
                                    self.get_extents(aref)
                                } else {
                                    None
                                }
                            },
                        )
                    },
                    || {
                        rayon::join(
                            || {
                                if role_has_actions(role) {
                                    self.get_actions(aref)
                                } else {
                                    (vec![], HashMap::new())
                                }
                            },
                            || {
                                if matches!(
                                    role,
                                    Role::Slider
                                        | Role::ProgressBar
                                        | Role::ScrollBar
                                        | Role::SpinButton
                                ) {
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
    ///
    /// Used by `subscribe` to resolve the target app's D-Bus sender name so
    /// signal match rules can be scoped to it.
    pub(crate) fn find_app_by_pid(&self, pid: u32) -> Result<AccessibleRef> {
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
                    return Ok(child.clone());
                }
            }
            // Fall back to Application.Id for adapters that do set it to pid.
            if let Ok(proxy) =
                self.make_proxy(&child.bus_name, &child.path, "org.a11y.atspi.Application")
            {
                if let Ok(app_pid) = proxy.get_property::<i32>("Id") {
                    if app_pid as u32 == pid {
                        return Ok(child.clone());
                    }
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

    /// Perform an AT-SPI2 action by its integer index (from discovery).
    fn do_atspi_action_by_index(&self, aref: &AccessibleRef, index: i32) -> Result<()> {
        let proxy = self.make_proxy(&aref.bus_name, &aref.path, "org.a11y.atspi.Action")?;
        proxy
            .call_method("DoAction", &(index,))
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("DoAction({}) failed: {}", index, e),
            })?;
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
        self.refine_text_role(coarse, aref)
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

        for filter in &simple.filters {
            let attr = filter.attr.as_str();
            let resolved: Option<Option<String>> = match attr {
                "role" => Some(role.map(|r| r.to_snake_case().to_string())),
                "name" => {
                    let name = self.get_name(aref).ok().filter(|s| !s.is_empty());
                    // Mirror build_element_data: StaticText carries its name
                    // in the Text interface's Value when Name is empty.
                    let resolved = if name.is_none() && role == Some(Role::StaticText) {
                        self.get_value(aref)
                    } else {
                        name
                    };
                    Some(resolved)
                }
                "value" => Some(self.get_value(aref)),
                "description" => Some(self.get_description(aref).ok().filter(|s| !s.is_empty())),
                "enabled" | "visible" | "focused" | "focusable" | "selected" | "editable"
                | "modal" | "required" | "busy" | "expanded" | "checked" => {
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
                    let pid = None; // pid isn't selector-addressable
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
    ) -> Result<Vec<(usize, AccessibleRef)>> {
        if depth > max_depth {
            return Ok(vec![]);
        }

        let children = self.get_atspi_children(parent)?;

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

            if !parent_is_registry {
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
                        to_search.push(gc);
                    }
                    continue;
                }
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
                    if self.matches_ref(child, simple) {
                        child_results.push((idx, child.clone()));
                    }
                }
                match self.collect_matching_refs_group(child, clauses, depth + 1, max_depth, limit)
                {
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
                let children = self.get_atspi_children(&aref).unwrap_or_default();
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
                    let child_role = self.get_role_name(child_ref).unwrap_or_default();
                    if child_role == "application" {
                        let grandchildren = self.get_atspi_children(child_ref).unwrap_or_default();
                        for gc_ref in grandchildren {
                            if gc_ref.path == "/org/a11y/atspi/null"
                                || gc_ref.bus_name.is_empty()
                                || gc_ref.path.is_empty()
                            {
                                continue;
                            }
                            let gc_role = self.get_role_name(&gc_ref).unwrap_or_default();
                            if gc_role == "application" {
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
        if group.clauses.is_empty() {
            return Ok(vec![]);
        }
        // Reject any clause with zero segments early — `clause.segments[0]`
        // below would otherwise panic.
        if group.clauses.iter().any(|c| c.segments.is_empty()) {
            return Ok(vec![]);
        }

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

        let phase1: Vec<(usize, AccessibleRef)> =
            self.collect_matching_refs_group(&start_ref, &firsts, 0, max_depth_val, phase1_limit)?;

        let pid_from_root = root.pid;

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

        // Filter valid children first, then build in parallel
        let valid: Vec<(&AccessibleRef, String)> = children
            .iter()
            .filter(|c| c.path != "/org/a11y/atspi/null")
            .filter_map(|c| {
                let name = self.get_name(c).unwrap_or_default();
                if name.is_empty() {
                    None
                } else {
                    Some((c, name))
                }
            })
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
                    let mut data = self.build_element_data(child, Some(p));
                    // Mirror list_apps: prefer the registry-reported name.
                    let name = self.get_name(child).unwrap_or_default();
                    if !name.is_empty() {
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
                xa11y_core::Diagnosis {
                    last_observed: Some(format!(
                        "{total} AT-SPI registry entries examined, {unresolved} without a \
                         resolvable pid"
                    )),
                    ..Default::default()
                },
            ),
        )
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
        // Try Component.GrabFocus first, then fall back to stored action index.
        // GrabFocus returns a boolean indicating success — we must check it.
        // Fixes GitHub issue #98.
        if let Ok(proxy) =
            self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Component")
        {
            if let Ok(reply) = proxy.call_method("GrabFocus", &()) {
                // GrabFocus returns boolean: true if focus was grabbed, false otherwise
                if let Ok(true) = reply.body().deserialize::<bool>() {
                    return Ok(());
                }
                // GrabFocus returned false — fall through to action index fallback
            }
        }
        if let Ok(index) = self.get_action_index(element.handle, "focus") {
            return self.do_atspi_action_by_index(&target, index);
        }
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

    fn set_value(&self, element: &ElementData, value: &str) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        let proxy = self
            .make_proxy(
                &target.bus_name,
                &target.path,
                "org.a11y.atspi.EditableText",
            )
            .map_err(|_| Error::TextValueNotSupported)?;
        // Try SetTextContents first (WebKit2GTK exposes this but not InsertText).
        if proxy.call_method("SetTextContents", &(value,)).is_ok() {
            return Ok(());
        }
        // Fall back to delete-then-insert for other AT-SPI2 implementations.
        // Capture the underlying D-Bus error so callers can distinguish an
        // absent `EditableText` interface (common on Chromium — the Chrome
        // URL bar only exposes read-only `Text`; see issue #101) from other
        // failures. Collapsing to `TextValueNotSupported` hides the reason.
        let classify_editable_text_error = |op: &str, e: zbus::Error| -> Error {
            let msg = e.to_string();
            if msg.contains("UnknownMethod") || msg.contains("UnknownInterface") {
                Error::TextValueNotSupported
            } else {
                Error::Platform {
                    code: -1,
                    message: format!("EditableText.{} failed: {}", op, msg),
                }
            }
        };
        if let Err(e) = proxy.call_method("DeleteText", &(0i32, i32::MAX)) {
            return Err(classify_editable_text_error("DeleteText", e));
        }
        if let Err(e) = proxy.call_method("InsertText", &(0i32, value, value.len() as i32)) {
            return Err(classify_editable_text_error("InsertText", e));
        }
        Ok(())
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
        // Insert text via EditableText interface (accessibility API, not input simulation).
        // Get cursor position from Text interface, then insert at that position.
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
        proxy
            .call_method("InsertText", &(pos, text, text.len() as i32))
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("EditableText.InsertText failed: {}", e),
            })?;
        Ok(())
    }

    fn set_text_selection(&self, element: &ElementData, start: u32, end: u32) -> Result<()> {
        let target = self.get_cached(element.handle)?;
        let proxy = self.make_proxy(&target.bus_name, &target.path, "org.a11y.atspi.Text")?;
        proxy
            .call_method("SetSelection", &(0i32, start as i32, end as i32))
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("Text.SetSelection failed: {}", e),
            })?;
        Ok(())
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
pub(crate) fn map_atspi_role(role_name: &str) -> Role {
    match role_name.to_lowercase().as_str() {
        "application" => Role::Application,
        "window" | "frame" => Role::Window,
        "dialog" | "file chooser" => Role::Dialog,
        "alert" | "notification" => Role::Alert,
        "push button" | "push button menu" => Role::Button,
        "toggle button" => Role::Switch,
        "check box" | "check menu item" => Role::CheckBox,
        "radio button" | "radio menu item" => Role::RadioButton,
        "entry" | "password text" => Role::TextField,
        "spin button" | "spinbutton" => Role::SpinButton,
        // "textbox" is the ARIA role name returned by WebKit2GTK for both
        // <input type="text"> and <textarea>.  Map to TextArea here so the
        // multi-line refinement below can downgrade single-line ones to TextField.
        "text" | "textbox" => Role::TextArea,
        "label" | "static" | "caption" => Role::StaticText,
        "combo box" | "combobox" => Role::ComboBox,
        // "listbox" is the ARIA role name returned by WebKit2GTK for role="listbox".
        "list" | "list box" | "listbox" => Role::List,
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
        _ => xa11y_core::unknown_role(role_name),
    }
}

/// Map AT-SPI2 numeric role (AtspiRole enum) to xa11y Role.
/// Values from atspi-common Role enum (repr(u32)).
pub(crate) fn map_atspi_role_number(role: u32) -> Role {
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
        62 => Role::Switch,      // ToggleButton
        63 => Role::Toolbar,     // ToolBar
        65 => Role::Group,       // Tree
        66 => Role::Table,       // TreeTable
        67 => Role::Unknown,     // Unknown
        68 => Role::Group,       // Viewport
        69 => Role::Window,      // Window
        75 => Role::Application, // Application
        78 => Role::TextArea, // Embedded — WebKit2GTK uses this for <input type="text"> and <textarea>;
        // multi-line refinement below downgrades single-line ones to TextField
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
        97 => Role::List,        // WebKit2GTK uses this for <ul role="listbox">
        98 => Role::List,        // ListBox
        93 => Role::Tooltip,     // Tooltip
        101 => Role::Alert,      // Notification
        116 => Role::StaticText, // Static
        129 => Role::Button,     // PushButtonMenu
        _ => xa11y_core::unknown_role(&format!("AT-SPI role number {role}")),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_numeric_role_mapping() {
        // ToggleButton (62) must map to Switch, not Button.
        // GTK4's Gtk.Switch and Gtk.ToggleButton both report numeric role 62.
        assert_eq!(map_atspi_role_number(62), Role::Switch);
        // Sanity-check a few well-established numeric mappings.
        assert_eq!(map_atspi_role_number(43), Role::Button); // PushButton
        assert_eq!(map_atspi_role_number(7), Role::CheckBox);
        assert_eq!(map_atspi_role_number(67), Role::Unknown); // AT-SPI Unknown
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
}
