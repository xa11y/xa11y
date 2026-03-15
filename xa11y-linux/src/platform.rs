use std::sync::Mutex;

use tokio::runtime::Runtime;
use xa11y_core::*;
use zbus::zvariant::OwnedObjectPath;

use crate::atspi::{self, ElementRef};
use crate::mapping;

/// Linux accessibility provider using AT-SPI2 over D-Bus.
pub struct LinuxProvider {
    rt: Runtime,
    conn: Mutex<Option<zbus::Connection>>,
    /// Cached element references from the last snapshot, indexed by NodeId.
    elements: Mutex<Vec<ElementRef>>,
    /// Last query target and options, for re-traversal on action dispatch.
    last_target: Mutex<Option<AppTarget>>,
    last_query: Mutex<Option<QueryOptions>>,
}

impl LinuxProvider {
    pub fn new() -> Self {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime");
        Self {
            rt,
            conn: Mutex::new(None),
            elements: Mutex::new(Vec::new()),
            last_target: Mutex::new(None),
            last_query: Mutex::new(None),
        }
    }

    fn connection(&self) -> Result<zbus::Connection> {
        let mut guard = self.conn.lock().unwrap();
        if let Some(ref conn) = *guard {
            return Ok(conn.clone());
        }
        let conn = self.rt.block_on(atspi::connect())?;
        *guard = Some(conn.clone());
        Ok(conn)
    }
}

impl Default for LinuxProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for LinuxProvider {
    fn check_permissions(&self) -> Result<PermissionStatus> {
        let conn = self.connection()?;
        let result: Result<()> = self.rt.block_on(async {
            let registry = registry_root()?;
            let proxy = atspi::accessible_proxy(&conn, &registry.bus_name, &registry.path)
                .await
                .map_err(|e| Error::Platform(format!("AT-SPI2 registry unavailable: {e}")))?;
            let _count: i32 = proxy
                .child_count()
                .await
                .map_err(|e| Error::Platform(format!("AT-SPI2 registry query failed: {e}")))?;
            Ok(())
        });

        match result {
            Ok(()) => Ok(PermissionStatus::Granted),
            Err(Error::Platform(msg)) => Ok(PermissionStatus::Denied {
                instructions: format!(
                    "AT-SPI2 is not available: {msg}. \
                     Ensure at-spi2-core is installed and accessibility is enabled: \
                     gsettings set org.gnome.desktop.interface toolkit-accessibility true"
                ),
            }),
            Err(e) => Err(e),
        }
    }

    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        let conn = self.connection()?;
        self.rt.block_on(list_apps_async(&conn))
    }

    fn get_app_tree(&self, target: &AppTarget, opts: &QueryOptions) -> Result<Tree> {
        let conn = self.connection()?;
        let (tree, elements) = self.rt.block_on(get_app_tree_async(&conn, target, opts))?;

        *self.elements.lock().unwrap() = elements;
        *self.last_target.lock().unwrap() = Some(target.clone());
        *self.last_query.lock().unwrap() = Some(opts.clone());

        Ok(tree)
    }

    fn get_all_apps(&self, opts: &QueryOptions) -> Result<Tree> {
        let conn = self.connection()?;
        let apps = self.rt.block_on(list_apps_async(&conn))?;
        let screen_size = get_screen_size();

        let all_nodes: Vec<Node> = apps
            .iter()
            .enumerate()
            .map(|(i, app)| Node {
                id: i as NodeId,
                role: Role::Application,
                name: Some(app.name.clone()),
                value: None,
                description: None,
                bounds: None,
                bounds_normalized: None,
                actions: vec![],
                states: StateSet::default(),
                children: vec![],
                parent: None,
                depth: 0,
                app_name: Some(app.name.clone()),
                raw: None,
            })
            .collect();

        Ok(Tree::new(
            String::new(),
            0,
            screen_size,
            all_nodes,
            opts.clone(),
        ))
    }

    fn perform_action(
        &self,
        node_id: NodeId,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        let conn = self.connection()?;
        let elements = self.elements.lock().unwrap();

        let elem = elements
            .get(node_id as usize)
            .ok_or(Error::NodeNotFound(node_id))?
            .clone();
        drop(elements);

        self.rt
            .block_on(perform_action_async(&conn, &elem, action, data))
    }
}

// ─── Helpers ────────────────────────────────────────────────────

fn registry_root() -> Result<ElementRef> {
    Ok(ElementRef {
        bus_name: "org.a11y.atspi.Registry".into(),
        path: OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root")
            .map_err(|e| Error::Platform(format!("invalid path: {e}")))?,
    })
}

// ─── Async Implementation ───────────────────────────────────────

async fn list_apps_async(conn: &zbus::Connection) -> Result<Vec<AppInfo>> {
    let registry = registry_root()?;
    let proxy = atspi::accessible_proxy(conn, &registry.bus_name, &registry.path)
        .await
        .map_err(|e| Error::Platform(format!("AT-SPI2 registry unavailable: {e}")))?;

    let children: Vec<(String, OwnedObjectPath)> = proxy
        .get_children()
        .await
        .map_err(|e| Error::Platform(format!("failed to get registry children: {e}")))?;

    let mut apps = Vec::new();
    for (bus_name, path) in children {
        let child = ElementRef {
            bus_name: bus_name.clone(),
            path,
        };

        let name: String = match atspi::accessible_proxy(conn, &child.bus_name, &child.path).await {
            Ok(p) => p.name().await.unwrap_or_default(),
            Err(_) => continue,
        };

        let pid: u32 = match atspi::application_proxy(conn, &child.bus_name, &child.path).await {
            Ok(p) => p.id().await.unwrap_or(0) as u32,
            Err(_) => 0,
        };

        if name.is_empty() && pid == 0 {
            continue;
        }

        apps.push(AppInfo {
            name: if name.is_empty() {
                format!("pid:{pid}")
            } else {
                name
            },
            pid,
            bundle_id: None,
        });
    }

    Ok(apps)
}

async fn get_app_tree_async(
    conn: &zbus::Connection,
    target: &AppTarget,
    opts: &QueryOptions,
) -> Result<(Tree, Vec<ElementRef>)> {
    let registry = registry_root()?;
    let proxy = atspi::accessible_proxy(conn, &registry.bus_name, &registry.path)
        .await
        .map_err(|e| Error::Platform(format!("AT-SPI2 registry unavailable: {e}")))?;

    let children: Vec<(String, OwnedObjectPath)> = proxy
        .get_children()
        .await
        .map_err(|e| Error::Platform(format!("failed to get registry children: {e}")))?;

    // Find the target app
    let mut app_root: Option<ElementRef> = None;
    let mut app_name = String::new();
    let mut app_pid: u32 = 0;

    for (bus_name, path) in children {
        let child = ElementRef {
            bus_name: bus_name.clone(),
            path,
        };

        let name: String = match atspi::accessible_proxy(conn, &child.bus_name, &child.path).await {
            Ok(p) => p.name().await.unwrap_or_default(),
            Err(_) => continue,
        };

        let pid: u32 = match atspi::application_proxy(conn, &child.bus_name, &child.path).await {
            Ok(p) => p.id().await.unwrap_or(0) as u32,
            Err(_) => 0,
        };

        let matches = match target {
            AppTarget::ByName(ref target_name) => {
                name.to_lowercase().contains(&target_name.to_lowercase())
            }
            AppTarget::ByPid(target_pid) => pid == *target_pid,
            AppTarget::ByWindow(_) => false,
        };

        if matches {
            app_root = Some(child);
            app_name = name;
            app_pid = pid;
            break;
        }
    }

    let app_root = app_root.ok_or_else(|| {
        Error::AppNotFound(match target {
            AppTarget::ByName(n) => n.clone(),
            AppTarget::ByPid(p) => format!("pid:{p}"),
            AppTarget::ByWindow(h) => format!("window:{}", h.id),
        })
    })?;

    let screen_size = get_screen_size();

    let mut nodes: Vec<Node> = Vec::new();
    let mut elements: Vec<ElementRef> = Vec::new();
    let mut next_id: NodeId = 0;

    traverse_element(
        conn,
        &app_root,
        None,
        0,
        &app_name,
        opts,
        screen_size,
        &mut nodes,
        &mut elements,
        &mut next_id,
    )
    .await;

    let tree = Tree::new(app_name, app_pid, screen_size, nodes, opts.clone());
    Ok((tree, elements))
}

#[allow(clippy::too_many_arguments)]
async fn traverse_element(
    conn: &zbus::Connection,
    elem: &ElementRef,
    parent_id: Option<NodeId>,
    depth: u32,
    app_name: &str,
    opts: &QueryOptions,
    screen_size: (u32, u32),
    nodes: &mut Vec<Node>,
    elements: &mut Vec<ElementRef>,
    next_id: &mut NodeId,
) {
    if depth > opts.max_depth || *next_id >= opts.max_elements {
        return;
    }

    let Ok(proxy) = atspi::accessible_proxy(conn, &elem.bus_name, &elem.path).await else {
        return;
    };

    let name: Option<String> = proxy.name().await.ok().filter(|s| !s.is_empty());
    let description: Option<String> = proxy.description().await.ok().filter(|s| !s.is_empty());
    let role_id: u32 = proxy.get_role().await.unwrap_or(67);
    let role = mapping::map_role(role_id);
    let state_bits: Vec<u32> = proxy.get_state().await.unwrap_or_default();
    let states = mapping::map_states(&state_bits);

    if let Some(ref roles) = opts.roles {
        if !roles.contains(&role) {
            return;
        }
    }

    if opts.visible_only && !states.visible {
        return;
    }

    let my_id = *next_id;
    *next_id += 1;

    // Bounds from Component interface
    let bounds: Option<Rect> =
        if let Ok(comp) = atspi::component_proxy(conn, &elem.bus_name, &elem.path).await {
            comp.get_extents(0).await.ok().map(|(x, y, w, h)| Rect {
                x,
                y,
                width: w,
                height: h,
            })
        } else {
            None
        };

    let bounds_normalized: Option<NormalizedRect> = bounds.and_then(|b| {
        if screen_size.0 > 0 && screen_size.1 > 0 {
            Some(NormalizedRect {
                x1: b.x as f64 / screen_size.0 as f64,
                y1: b.y as f64 / screen_size.1 as f64,
                x2: (b.x + b.width) as f64 / screen_size.0 as f64,
                y2: (b.y + b.height) as f64 / screen_size.1 as f64,
            })
        } else {
            None
        }
    });

    let interfaces: Vec<String> = proxy.get_interfaces().await.unwrap_or_default();
    let value: Option<String> = get_element_value(conn, elem, role, &interfaces).await;
    let actions: Vec<Action> = get_element_actions(conn, elem, &interfaces).await;

    let raw: Option<RawPlatformData> = if opts.include_raw {
        Some(RawPlatformData::Linux {
            atspi_role: role_id.to_string(),
            bus_name: elem.bus_name.clone(),
            object_path: elem.path.to_string(),
        })
    } else {
        None
    };

    let node = Node {
        id: my_id,
        role,
        name,
        value,
        description,
        bounds,
        bounds_normalized,
        actions,
        states,
        children: vec![],
        parent: parent_id,
        depth,
        app_name: Some(app_name.to_string()),
        raw,
    };

    let node_idx = nodes.len();
    nodes.push(node);
    elements.push(elem.clone());

    let children_refs: Vec<(String, OwnedObjectPath)> =
        proxy.get_children().await.unwrap_or_default();
    let mut child_ids: Vec<NodeId> = Vec::new();

    for (bus_name, path) in children_refs {
        if *next_id >= opts.max_elements {
            break;
        }
        let child_elem = ElementRef { bus_name, path };
        let child_id = *next_id;
        let prev_count = nodes.len();

        Box::pin(traverse_element(
            conn,
            &child_elem,
            Some(my_id),
            depth + 1,
            app_name,
            opts,
            screen_size,
            nodes,
            elements,
            next_id,
        ))
        .await;

        if nodes.len() > prev_count {
            child_ids.push(child_id);
        }
    }

    nodes[node_idx].children = child_ids;
}

async fn get_element_value(
    conn: &zbus::Connection,
    elem: &ElementRef,
    role: Role,
    interfaces: &[String],
) -> Option<String> {
    let has_text = interfaces.iter().any(|i| i == "org.a11y.atspi.Text");
    let has_value = interfaces.iter().any(|i| i == "org.a11y.atspi.Value");

    if has_text
        && matches!(
            role,
            Role::TextField | Role::TextArea | Role::StaticText | Role::Heading | Role::Link
        )
    {
        if let Ok(text_proxy) = atspi::text_proxy(conn, &elem.bus_name, &elem.path).await {
            if let Ok(content) = text_proxy.get_text(0, -1).await {
                if !content.is_empty() {
                    return Some(content);
                }
            }
        }
    }

    if has_value
        && matches!(
            role,
            Role::Slider | Role::ProgressBar | Role::ScrollBar | Role::TextField
        )
    {
        if let Ok(val_proxy) = atspi::value_proxy(conn, &elem.bus_name, &elem.path).await {
            if let Ok(v) = val_proxy.current_value().await {
                return Some(v.to_string());
            }
        }
    }

    None
}

async fn get_element_actions(
    conn: &zbus::Connection,
    elem: &ElementRef,
    interfaces: &[String],
) -> Vec<Action> {
    let has_action = interfaces.iter().any(|i| i == "org.a11y.atspi.Action");
    if !has_action {
        // Still check for Focus/SetValue via other interfaces
        let mut actions = Vec::new();
        let has_component = interfaces.iter().any(|i| i == "org.a11y.atspi.Component");
        if has_component {
            actions.push(Action::Focus);
        }
        let has_editable = interfaces
            .iter()
            .any(|i| i == "org.a11y.atspi.EditableText" || i == "org.a11y.atspi.Value");
        if has_editable {
            actions.push(Action::SetValue);
        }
        return actions;
    }

    let Ok(proxy) = atspi::action_proxy(conn, &elem.bus_name, &elem.path).await else {
        return vec![];
    };

    let n: i32 = proxy.get_n_actions().await.unwrap_or(0);
    let mut actions: Vec<Action> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for i in 0..n {
        if let Ok(name) = proxy.get_name(i).await {
            if let Some(action) = mapping::map_action_name(&name) {
                if seen.insert(action) {
                    actions.push(action);
                }
            }
        }
    }

    let has_component = interfaces.iter().any(|i| i == "org.a11y.atspi.Component");
    if has_component && !seen.contains(&Action::Focus) {
        actions.push(Action::Focus);
    }

    let has_editable = interfaces
        .iter()
        .any(|i| i == "org.a11y.atspi.EditableText" || i == "org.a11y.atspi.Value");
    if has_editable && !seen.contains(&Action::SetValue) {
        actions.push(Action::SetValue);
    }

    actions
}

async fn perform_action_async(
    conn: &zbus::Connection,
    elem: &ElementRef,
    action: Action,
    data: Option<ActionData>,
) -> Result<()> {
    let bn = &elem.bus_name;
    let p = &elem.path;
    match action {
        Action::Focus => {
            let proxy = atspi::component_proxy(conn, bn, p)
                .await
                .map_err(|e| Error::ActionNotSupported(format!("no Component interface: {e}")))?;
            let _ok: bool = proxy
                .grab_focus()
                .await
                .map_err(|e| Error::Platform(format!("GrabFocus failed: {e}")))?;
            Ok(())
        }
        Action::SetValue => {
            let value = data
                .ok_or_else(|| Error::ActionNotSupported("SetValue requires ActionData".into()))?;
            match value {
                ActionData::Value(text) => {
                    let proxy = atspi::editable_text_proxy(conn, bn, p).await.map_err(|e| {
                        Error::ActionNotSupported(format!("no EditableText interface: {e}"))
                    })?;
                    let _ok: bool = proxy
                        .set_text_contents(&text)
                        .await
                        .map_err(|e| Error::Platform(format!("SetTextContents failed: {e}")))?;
                    Ok(())
                }
                ActionData::NumericValue(v) => {
                    let proxy = atspi::value_proxy(conn, bn, p).await.map_err(|e| {
                        Error::ActionNotSupported(format!("no Value interface: {e}"))
                    })?;
                    proxy
                        .set_current_value(v)
                        .await
                        .map_err(|e| Error::Platform(format!("SetCurrentValue failed: {e}")))?;
                    Ok(())
                }
                _ => Err(Error::ActionNotSupported(
                    "SetValue requires Value or NumericValue data".into(),
                )),
            }
        }
        Action::ScrollIntoView => {
            let proxy = atspi::component_proxy(conn, bn, p)
                .await
                .map_err(|e| Error::ActionNotSupported(format!("no Component interface: {e}")))?;
            let _ok: bool = proxy
                .scroll_to(0)
                .await
                .map_err(|e| Error::Platform(format!("ScrollTo failed: {e}")))?;
            Ok(())
        }
        _ => {
            let proxy = atspi::action_proxy(conn, bn, p)
                .await
                .map_err(|e| Error::ActionNotSupported(format!("no Action interface: {e}")))?;

            let n: i32 = proxy
                .get_n_actions()
                .await
                .map_err(|e| Error::Platform(format!("GetNActions failed: {e}")))?;

            let target_names = mapping::atspi_names_for_action(&action);

            for i in 0..n {
                if let Ok(name) = proxy.get_name(i).await {
                    let lower = name.to_lowercase();
                    if target_names.iter().any(|t| *t == lower) {
                        let _ok: bool = proxy
                            .do_action(i)
                            .await
                            .map_err(|e| Error::Platform(format!("DoAction failed: {e}")))?;
                        return Ok(());
                    }
                }
            }

            Err(Error::ActionNotSupported(format!(
                "action {action:?} not available on this element"
            )))
        }
    }
}

/// Detect screen size using available tools, with a fallback default.
fn get_screen_size() -> (u32, u32) {
    // Try xdpyinfo (X11)
    if let Ok(output) = std::process::Command::new("xdpyinfo").output() {
        if output.status.success() {
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
    }

    // Try swaymsg (Sway/Wayland)
    if let Ok(output) = std::process::Command::new("swaymsg")
        .args(["-t", "get_outputs", "--raw"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&stdout) {
                if let Some(arr) = val.as_array() {
                    for output in arr {
                        if let (Some(w), Some(h)) = (
                            output
                                .get("current_mode")
                                .and_then(|m| m.get("width"))
                                .and_then(|v| v.as_u64()),
                            output
                                .get("current_mode")
                                .and_then(|m| m.get("height"))
                                .and_then(|v| v.as_u64()),
                        ) {
                            return (w as u32, h as u32);
                        }
                    }
                }
            }
        }
    }

    // Try wlr-randr (wlroots Wayland compositors)
    if let Ok(output) = std::process::Command::new("wlr-randr").output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let trimmed = line.trim();
                if trimmed.contains("px") {
                    if let Some(dims) = trimmed.split_whitespace().next() {
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
    }

    (1920, 1080)
}
