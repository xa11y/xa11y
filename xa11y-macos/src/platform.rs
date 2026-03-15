use std::collections::HashSet;
use std::sync::Mutex;

use xa11y_core::*;

use crate::ax_ffi::AXElement;
use crate::mapping;

/// macOS accessibility provider using AXUIElement APIs.
pub struct MacOSProvider {
    /// Cached element references from the last snapshot, indexed by NodeId.
    elements: Mutex<Vec<AXElement>>,
}

impl MacOSProvider {
    pub fn new() -> Self {
        Self {
            elements: Mutex::new(Vec::new()),
        }
    }
}

impl Default for MacOSProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for MacOSProvider {
    fn check_permissions(&self) -> Result<PermissionStatus> {
        let trusted = unsafe { crate::ax_ffi::AXIsProcessTrusted() };
        if trusted {
            Ok(PermissionStatus::Granted)
        } else {
            Ok(PermissionStatus::Denied {
                instructions: "Accessibility access is not enabled. \
                    Go to System Settings > Privacy & Security > Accessibility \
                    and add this application."
                    .into(),
            })
        }
    }

    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        list_running_apps()
    }

    fn get_app_tree(&self, target: &AppTarget, opts: &QueryOptions) -> Result<Tree> {
        let apps = list_running_apps()?;
        let app = find_target_app(&apps, target)?;

        let ax_app = AXElement::application(app.pid as i32).ok_or_else(|| {
            Error::Platform(format!("Failed to create AXUIElement for pid {}", app.pid))
        })?;

        let screen_size = get_screen_size();
        let mut nodes: Vec<Node> = Vec::new();
        let mut elements: Vec<AXElement> = Vec::new();
        let mut next_id: NodeId = 0;

        traverse_element(
            &ax_app,
            None,
            0,
            &app.name,
            opts,
            screen_size,
            &mut nodes,
            &mut elements,
            &mut next_id,
        );

        *self.elements.lock().unwrap() = elements;

        Ok(Tree::new(
            app.name.clone(),
            app.pid,
            screen_size,
            nodes,
            opts.clone(),
        ))
    }

    fn get_all_apps(&self, opts: &QueryOptions) -> Result<Tree> {
        let apps = list_running_apps()?;
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
        let elements = self.elements.lock().unwrap();
        let elem = elements
            .get(node_id as usize)
            .ok_or(Error::NodeNotFound(node_id))?;

        match action {
            Action::Focus => {
                // AXRaise on the window, or set AXFocused
                if !elem.set_string_attribute("AXFocused", "1") {
                    // Try AXRaise action
                    elem.perform_action("AXRaise");
                }
                Ok(())
            }
            Action::SetValue => {
                let data =
                    data.ok_or_else(|| Error::ActionNotSupported("SetValue requires data".into()))?;
                match data {
                    ActionData::Value(text) => {
                        if !elem.set_string_attribute("AXValue", &text) {
                            return Err(Error::Platform("Failed to set AXValue".into()));
                        }
                        Ok(())
                    }
                    ActionData::NumericValue(v) => {
                        if !elem.set_number_attribute("AXValue", v) {
                            return Err(Error::Platform("Failed to set AXValue".into()));
                        }
                        Ok(())
                    }
                    _ => Err(Error::ActionNotSupported(
                        "SetValue requires Value or NumericValue data".into(),
                    )),
                }
            }
            Action::ScrollIntoView => {
                // AXScrollToVisible is not a standard action; try setting AXVisibleCharacterRange
                // or use AXPress as fallback
                elem.perform_action("AXScrollToVisible");
                Ok(())
            }
            _ => {
                if let Some(ax_action) = mapping::ax_action_for(&action) {
                    if elem.perform_action(ax_action) {
                        Ok(())
                    } else {
                        Err(Error::ActionNotSupported(format!(
                            "action {action:?} failed on this element"
                        )))
                    }
                } else {
                    Err(Error::ActionNotSupported(format!(
                        "action {action:?} not supported on macOS"
                    )))
                }
            }
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────

fn find_target_app(apps: &[AppInfo], target: &AppTarget) -> Result<AppInfo> {
    let found = apps.iter().find(|app| match target {
        AppTarget::ByName(ref name) => app.name.to_lowercase().contains(&name.to_lowercase()),
        AppTarget::ByPid(pid) => app.pid == *pid,
        AppTarget::ByWindow(_) => false,
    });

    found.cloned().ok_or_else(|| {
        Error::AppNotFound(match target {
            AppTarget::ByName(n) => n.clone(),
            AppTarget::ByPid(p) => format!("pid:{p}"),
            AppTarget::ByWindow(h) => format!("window:{}", h.id),
        })
    })
}

/// List running apps using NSWorkspace via the `ps` command as a portable fallback.
fn list_running_apps() -> Result<Vec<AppInfo>> {
    // Use ps to get running GUI apps. On macOS, apps with a window server connection
    // typically have a bundle and are in /Applications or similar.
    let output = std::process::Command::new("ps")
        .args(["-eo", "pid,comm"])
        .output()
        .map_err(|e| Error::Platform(format!("failed to run ps: {e}")))?;

    if !output.status.success() {
        return Err(Error::Platform("ps command failed".into()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut apps = Vec::new();
    let mut seen_names = HashSet::new();

    for line in stdout.lines().skip(1) {
        let trimmed = line.trim();
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let pid_str = match parts.next() {
            Some(s) => s.trim(),
            None => continue,
        };
        let comm = match parts.next() {
            Some(s) => s.trim(),
            None => continue,
        };

        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        // Filter to .app bundle executables
        if !comm.contains(".app/") {
            continue;
        }

        // Extract app name from path like /Applications/Safari.app/Contents/MacOS/Safari
        let app_name = if let Some(idx) = comm.rfind(".app/") {
            let before = &comm[..idx];
            before.rsplit('/').next().unwrap_or(comm).to_string()
        } else {
            comm.rsplit('/').next().unwrap_or(comm).to_string()
        };

        if app_name.is_empty() || !seen_names.insert(app_name.clone()) {
            continue;
        }

        // Try to extract bundle_id from the path
        let bundle_id = extract_bundle_id(comm);

        apps.push(AppInfo {
            name: app_name,
            pid,
            bundle_id,
        });
    }

    Ok(apps)
}

fn extract_bundle_id(path: &str) -> Option<String> {
    // Try to read Info.plist from the .app bundle
    if let Some(idx) = path.find(".app/") {
        let bundle_path = format!("{}/Contents/Info.plist", &path[..idx + 4]);
        if let Ok(output) = std::process::Command::new("defaults")
            .args(["read", &bundle_path, "CFBundleIdentifier"])
            .output()
        {
            if output.status.success() {
                let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !id.is_empty() {
                    return Some(id);
                }
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn traverse_element(
    elem: &AXElement,
    parent_id: Option<NodeId>,
    depth: u32,
    app_name: &str,
    opts: &QueryOptions,
    screen_size: (u32, u32),
    nodes: &mut Vec<Node>,
    elements: &mut Vec<AXElement>,
    next_id: &mut NodeId,
) {
    if depth > opts.max_depth || *next_id >= opts.max_elements {
        return;
    }

    let ax_role = elem.string_attribute("AXRole").unwrap_or_default();
    let ax_subrole = elem.string_attribute("AXSubrole");
    let role = mapping::map_role(&ax_role, ax_subrole.as_deref());

    if let Some(ref roles) = opts.roles {
        if !roles.contains(&role) {
            return;
        }
    }

    let name = elem
        .string_attribute("AXTitle")
        .or_else(|| elem.string_attribute("AXDescription"))
        .filter(|s| !s.is_empty());

    let description = elem.string_attribute("AXHelp").filter(|s| !s.is_empty());

    // Bounds
    let bounds: Option<Rect> = match (elem.position(), elem.size()) {
        (Some((x, y)), Some((w, h))) => Some(Rect {
            x: x as i32,
            y: y as i32,
            width: w as i32,
            height: h as i32,
        }),
        _ => None,
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

    // States
    let enabled = elem.bool_attribute("AXEnabled").unwrap_or(true);
    let focused = elem.bool_attribute("AXFocused").unwrap_or(false);
    let selected = elem.bool_attribute("AXSelected").unwrap_or(false);

    let value_int = elem.number_attribute("AXValue").map(|v| v as i64);
    let expanded = if matches!(
        role,
        Role::TreeItem | Role::ComboBox | Role::Group | Role::MenuItem
    ) {
        elem.bool_attribute("AXExpanded")
    } else {
        None
    };

    let mut states = mapping::map_states(enabled, focused, selected, value_int, expanded, role);

    // Check editable
    if matches!(role, Role::TextField | Role::TextArea) {
        states.editable = true;
    }

    if opts.visible_only && !states.visible {
        return;
    }

    let my_id = *next_id;
    *next_id += 1;

    // Value
    let value: Option<String> = get_element_value(elem, role);

    // Actions
    let actions: Vec<Action> = get_element_actions(elem, role);

    // Raw platform data
    let raw: Option<RawPlatformData> = if opts.include_raw {
        let ax_identifier = elem.string_attribute("AXIdentifier");
        Some(RawPlatformData::MacOS {
            ax_role,
            ax_subrole,
            ax_identifier,
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

    // Traverse children
    let children = elem.children();
    let mut child_ids: Vec<NodeId> = Vec::new();

    for child in &children {
        if *next_id >= opts.max_elements {
            break;
        }
        let child_id = *next_id;
        let prev_count = nodes.len();

        traverse_element(
            child,
            Some(my_id),
            depth + 1,
            app_name,
            opts,
            screen_size,
            nodes,
            elements,
            next_id,
        );

        if nodes.len() > prev_count {
            child_ids.push(child_id);
        }
    }

    nodes[node_idx].children = child_ids;
}

fn get_element_value(elem: &AXElement, role: Role) -> Option<String> {
    match role {
        Role::TextField | Role::TextArea | Role::StaticText | Role::Heading | Role::Link => {
            elem.string_attribute("AXValue").filter(|s| !s.is_empty())
        }
        Role::Slider | Role::ProgressBar | Role::ScrollBar => {
            elem.number_attribute("AXValue").map(|v| v.to_string())
        }
        _ => None,
    }
}

fn get_element_actions(elem: &AXElement, role: Role) -> Vec<Action> {
    let ax_names = elem.action_names();
    let mut actions: Vec<Action> = Vec::new();
    let mut seen = HashSet::new();

    for name in &ax_names {
        if let Some(action) = mapping::map_action_name(name) {
            if seen.insert(action) {
                actions.push(action);
            }
        }
    }

    // Always allow Focus if element supports it
    if !seen.contains(&Action::Focus) {
        actions.push(Action::Focus);
    }

    // SetValue for editable fields and sliders
    if matches!(
        role,
        Role::TextField | Role::TextArea | Role::Slider | Role::ComboBox
    ) && !seen.contains(&Action::SetValue)
    {
        actions.push(Action::SetValue);
    }

    // Toggle for checkboxes
    if matches!(role, Role::CheckBox | Role::RadioButton) && !seen.contains(&Action::Toggle) {
        actions.push(Action::Toggle);
    }

    actions
}

/// Get screen size from the main display.
fn get_screen_size() -> (u32, u32) {
    // Use system_profiler or CGMainDisplayID as fallback
    // For now, use a simpler approach via the system_profiler command
    if let Ok(output) = std::process::Command::new("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .output()
    {
        if output.status.success() {
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                if let Some(displays) = val.get("SPDisplaysDataType").and_then(|d| d.as_array()) {
                    for gpu in displays {
                        if let Some(ndisplays) =
                            gpu.get("spdisplays_ndrvs").and_then(|d| d.as_array())
                        {
                            for display in ndisplays {
                                let res = display
                                    .get("_spdisplays_resolution")
                                    .and_then(|r| r.as_str());
                                if let Some(res_str) = res {
                                    // Format: "2560 x 1440" or similar
                                    let parts: Vec<&str> = res_str.split(" x ").collect();
                                    if parts.len() == 2 {
                                        if let (Ok(w), Ok(h)) = (
                                            parts[0].trim().parse::<u32>(),
                                            parts[1]
                                                .trim()
                                                .split_whitespace()
                                                .next()
                                                .unwrap_or("")
                                                .parse::<u32>(),
                                        ) {
                                            return (w, h);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    (1920, 1080)
}
