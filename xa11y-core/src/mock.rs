//! In-memory mock Provider and test tree for binding tests.
//!
//! Gated behind the `test-support` feature so it only ships when a downstream
//! crate explicitly opts in (bindings' test builds, examples). The tree and
//! Provider impl are shared between `xa11y-python` and `xa11y-js` — neither
//! binding needed a bespoke mock; only their wrapper shapes differ.
//!
//! # Topology
//!
//! ```text
//! application "TestApp" (stable_id="app-root", desc="Test application")
//! └── window "Main Window" (focused, active)
//!     ├── toolbar "Navigation"
//!     │   ├── button "Back" (stable_id="btn-back", desc="Go back")
//!     │   └── button "Forward" (disabled)
//!     └── group "Content"
//!         ├── text_field "Search" (value="hello", editable, desc="Search field")
//!         ├── check_box "Agree" (checked=on)
//!         ├── slider "Volume" (numeric=75, min=0, max=100)
//!         ├── static_text "Status" (value="Loading...", visible=false)
//!         └── list "Items" (expanded=true)
//!             ├── list_item "Item 1" (selected)
//!             └── list_item "Item 2"
//! ```
//!
//! # Shell surfaces
//!
//! Two parentless roots model the OS shell (see [`crate::shell`]). They are
//! reachable only through [`Provider::list_shell_surfaces`] — deliberately not
//! from `list_apps` / `get_children(None)`, so shell UI stays invisible to code
//! that only asks for applications:
//!
//! ```text
//! toolbar "Taskbar" (shell surface: taskbar, pid=MOCK_SHELL_PID)
//! ├── button "Show Hidden Icons" (stable_id="systray-chevron")
//! └── button "Volume" (stable_id="SystemTrayIcon")
//!
//! list "Desktop" (shell surface: desktop, pid=MOCK_SHELL_PID)
//! └── list_item "Trash"
//! ```
//!
//! Call [`build_provider`] to get an `Arc<dyn Provider>`. The provider records
//! actions into an internal log; use [`MockProviderHandle::actions`] to inspect
//! them from tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::element::{ElementData, Rect, StateSet, Toggled};
use crate::error::{Error, Result};
use crate::event_provider::Subscription;
use crate::provider::Provider;
use crate::role::Role;
use crate::shell::ShellSurfaceKind;

/// Pid the mock reports for its shell-surface roots and their subtrees.
/// Distinct from the test app's 1234 so `ShellSurface::pid` is checkable and
/// so shell nodes can't be confused with app nodes.
pub const MOCK_SHELL_PID: u32 = 4242;

/// Index of the first shell node. Everything from here on belongs to a shell
/// surface rather than to the test application.
const FIRST_SHELL_NODE: usize = 13;

/// The mock's shell surfaces, as `(kind, node index)`. The indices are
/// positions in the element table `build_provider` builds below — the shell
/// roots are the parentless nodes appended after the application subtree.
const SHELL_SURFACES: [(ShellSurfaceKind, usize); 2] = [
    (ShellSurfaceKind::Taskbar, FIRST_SHELL_NODE),
    (ShellSurfaceKind::Desktop, FIRST_SHELL_NODE + 3),
];

/// Tuple describing one row in the mock element table.
///
/// Kept as a type alias so clippy's `type_complexity` lint stays happy.
type MockElementSpec<'a> = (
    Role,
    Option<&'a str>, // name
    Option<&'a str>, // value
    Option<&'a str>, // description
    Option<Rect>,
    Vec<&'a str>, // actions
    StateSet,
    Option<f64>,                                // numeric_value
    Option<f64>,                                // min_value
    Option<f64>,                                // max_value
    Option<&'a str>,                            // stable_id
    Option<HashMap<String, serde_json::Value>>, // raw
);

/// One entry in the mock's action log. `(handle, action_name, optional_argument)`.
pub type ActionLogEntry = (u64, String, Option<String>);

/// Mock provider backing the test tree.
pub struct MockProvider {
    /// Interior-mutable so window verbs can mutate state/bounds in place.
    nodes: Mutex<Vec<MockNode>>,
    actions: Mutex<Vec<ActionLogEntry>>,
}

impl MockProvider {
    /// Return a clone of the action log recorded so far.
    pub fn actions(&self) -> Vec<ActionLogEntry> {
        self.actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Clear the action log.
    pub fn clear_actions(&self) {
        self.actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    fn record(&self, el: &ElementData, action: &str, data: Option<String>) -> Result<()> {
        // Every generic action funnels through here, so this is where a
        // closed ancestor's invalidation is enforced for non-window verbs:
        // press/focus/etc. on a descendant of a closed window must fail, or
        // stale-subtree regressions would pass against the mock (the window
        // verbs validate with `live_window` up front instead).
        {
            let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(node) = nodes.get(el.handle as usize) {
                if node.closed {
                    return Err(Error::Platform {
                        code: -1,
                        message: format!(
                            "element handle {} belongs to a closed window; cannot act on it",
                            el.handle
                        ),
                    });
                }
            }
        }
        self.actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((el.handle, action.to_string(), data));
        Ok(())
    }

    /// Reject a stale or closed window handle before a window action.
    ///
    /// A closed window is gone (see `closed`): children and parent resolution
    /// both refuse it, so the verbs must too — an action on a closed window
    /// silently mutating the dead node and returning `Ok` would let
    /// stale-handle regressions pass against the mock (tenet 1, and the model
    /// `closed_window_stale_handle_is_dead` asserts).
    fn live_window(&self, el: &ElementData) -> Result<()> {
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        let node = nodes
            .get(el.handle as usize)
            .ok_or_else(|| Error::Platform {
                code: -1,
                message: format!("stale window handle: no live node {}", el.handle),
            })?;
        if node.closed {
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "window handle {} belongs to a closed window; cannot act on it",
                    el.handle
                ),
            });
        }
        Ok(())
    }
}

#[derive(Clone)]
struct MockNode {
    data: ElementData,
    children: Vec<usize>,
    parent: Option<usize>,
    /// A closed window is gone: unlike `minimized` (visible=false but still
    /// listed), a closed window must not be enumerated by `get_children` —
    /// real providers drop it from the tree.
    closed: bool,
}

impl Provider for MockProvider {
    fn get_children(&self, element: Option<&ElementData>) -> Result<Vec<ElementData>> {
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        match element {
            None => {
                if nodes.is_empty() {
                    return Ok(vec![]);
                }
                Ok(vec![nodes[0].data.clone()])
            }
            Some(el) => {
                let idx = el.handle as usize;
                // A closed element is gone (see `closed`): resolving its subtree
                // through a stale handle would fake a live window that real
                // providers have dropped.
                if idx >= nodes.len() || nodes[idx].closed {
                    return Ok(vec![]);
                }
                Ok(nodes[idx]
                    .children
                    .iter()
                    .filter(|&&i| !nodes[i].closed)
                    .map(|&i| nodes[i].data.clone())
                    .collect())
            }
        }
    }

    fn get_parent(&self, element: &ElementData) -> Result<Option<ElementData>> {
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        let idx = element.handle as usize;
        // A closed element is gone: no parent either (see `closed`).
        if idx >= nodes.len() || nodes[idx].closed {
            return Ok(None);
        }
        Ok(nodes[idx].parent.map(|i| nodes[i].data.clone()))
    }

    fn list_apps(&self) -> Result<Vec<ElementData>> {
        // The mock tree's root is a single Application node; expose it as
        // the lone "app" so Locator's rootless path enumerates it.
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        if nodes.is_empty() {
            return Ok(vec![]);
        }
        Ok(vec![nodes[0].data.clone()])
    }

    fn focused_app(&self) -> Result<ElementData> {
        // The mock has a single application root; treat it as the foreground
        // app so `App::is_foreground` / `find(|a| a.focused())` have something
        // to resolve against in binding and core tests.
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        if nodes.is_empty() {
            return Err(Error::selector_not_matched("focused application"));
        }
        Ok(nodes[0].data.clone())
    }

    fn list_shell_surfaces(&self) -> Result<Vec<(ShellSurfaceKind, ElementData)>> {
        // Fixed fixture: `SHELL_SURFACES`' indices name nodes `build_provider`
        // always creates, so indexing here cannot be out of range.
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        Ok(SHELL_SURFACES
            .iter()
            .map(|(kind, idx)| (*kind, nodes[*idx].data.clone()))
            .collect())
    }

    fn press(&self, el: &ElementData) -> Result<()> {
        self.record(el, "press", None)
    }
    fn focus(&self, el: &ElementData) -> Result<()> {
        self.record(el, "focus", None)
    }
    fn blur(&self, el: &ElementData) -> Result<()> {
        self.record(el, "blur", None)
    }
    fn toggle(&self, el: &ElementData) -> Result<()> {
        self.record(el, "toggle", None)
    }
    fn select(&self, el: &ElementData) -> Result<()> {
        self.record(el, "select", None)
    }
    fn expand(&self, el: &ElementData) -> Result<()> {
        self.record(el, "expand", None)
    }
    fn collapse(&self, el: &ElementData) -> Result<()> {
        self.record(el, "collapse", None)
    }
    fn show_menu(&self, el: &ElementData) -> Result<()> {
        self.record(el, "show_menu", None)
    }
    fn increment(&self, el: &ElementData) -> Result<()> {
        self.record(el, "increment", None)
    }
    fn decrement(&self, el: &ElementData) -> Result<()> {
        self.record(el, "decrement", None)
    }
    fn scroll_into_view(&self, el: &ElementData) -> Result<()> {
        self.record(el, "scroll_into_view", None)
    }
    fn set_value(&self, el: &ElementData, value: &str) -> Result<()> {
        self.record(el, "set_value", Some(value.to_string()))
    }
    fn set_numeric_value(&self, el: &ElementData, v: f64) -> Result<()> {
        self.record(el, "set_numeric_value", Some(format!("{v}")))
    }
    fn type_text(&self, el: &ElementData, text: &str) -> Result<()> {
        self.record(el, "type_text", Some(text.to_string()))
    }
    fn set_text_selection(&self, el: &ElementData, start: u32, end: u32) -> Result<()> {
        self.record(el, "set_text_selection", Some(format!("{start}..{end}")))
    }
    fn perform_action(&self, el: &ElementData, action: &str) -> Result<()> {
        // Nullary window verbs delegate to the typed methods, which validate
        // the target and mutate the model — the mock must not record success
        // for a call that changes nothing, or a test that fails on every real
        // provider would pass here. The payload verbs take no arguments on
        // the generic escape hatch and fail surfaceably with how to call
        // them, mirroring the real providers' contract (see uia.rs; tenet 1).
        match action {
            "raise" => self.raise(el),
            "minimize" => self.minimize(el),
            "maximize" => self.maximize(el),
            "restore" => self.restore(el),
            "close" => self.close(el),
            "move_to" => Err(Error::InvalidActionData {
                message: "perform_action(\"move_to\") requires coordinates; call move_to(x, y)"
                    .to_string(),
            }),
            "resize_to" => Err(Error::InvalidActionData {
                message: "perform_action(\"resize_to\") requires dimensions; call \
                           resize_to(width, height)"
                    .to_string(),
            }),
            _ => self.record(el, action, None),
        }
    }

    // ── Window management ──────────────────────────────────────────
    //
    // The mock models window state mutations in place so tests can verify
    // minimize → state → restore round-trips without a real platform.

    fn raise(&self, el: &ElementData) -> Result<()> {
        self.live_window(el)?;
        self.record(el, "raise", None)
    }

    fn minimize(&self, el: &ElementData) -> Result<()> {
        self.live_window(el)?;
        {
            let mut nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(node) = nodes.get_mut(el.handle as usize) {
                node.data.states.minimized = Some(true);
                // The mock decided the window is *not* maximized; the real
                // providers report the complementary flag as `Some(false)`
                // (UIA WindowVisualState_Minimized → (true, false)), and
                // `restore` below does the same. `None` means unknown, which
                // would be a lie about a decision the mock just made.
                node.data.states.maximized = Some(false);
                // Model an iconified window as off-screen. That is what makes
                // the Locator window verbs' enabled-only gate (no `visible`)
                // testable: a minimized window must still be actionable.
                node.data.states.visible = false;
            }
        }
        self.record(el, "minimize", None)
    }

    fn maximize(&self, el: &ElementData) -> Result<()> {
        self.live_window(el)?;
        {
            let mut nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(node) = nodes.get_mut(el.handle as usize) {
                node.data.states.maximized = Some(true);
                // Same convention as minimize: the complementary state is
                // decided, not unknown.
                node.data.states.minimized = Some(false);
                // A minimized window is modeled iconified (visible=false);
                // maximizing it must bring it back on-screen, or a
                // minimize → maximize round-trip would leave the mock
                // reporting maximized AND invisible.
                node.data.states.visible = true;
            }
        }
        self.record(el, "maximize", None)
    }

    fn restore(&self, el: &ElementData) -> Result<()> {
        self.live_window(el)?;
        {
            let mut nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(node) = nodes.get_mut(el.handle as usize) {
                node.data.states.minimized = Some(false);
                node.data.states.maximized = Some(false);
                node.data.states.visible = true;
            }
        }
        self.record(el, "restore", None)
    }

    fn close(&self, el: &ElementData) -> Result<()> {
        self.live_window(el)?;
        // Record before marking the node closed: `record` rejects closed
        // nodes, and once this window subtree is closed a "close" entry is
        // the last action that can be legitimately recorded for it.
        self.record(el, "close", None)?;
        {
            let mut nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(node) = nodes.get_mut(el.handle as usize) {
                // `closed` (not `visible`) is what removes it from the tree:
                // a minimized window is also visible=false but must stay
                // listed. Both flags together model the real provider result.
                node.closed = true;
                node.data.states.visible = false;
                // Destroying a real window invalidates its whole subtree, not
                // just the frame. A child handle captured before close must
                // not keep resolving a parent or acting against the mock —
                // stale-descendant regressions would otherwise pass here and
                // fail on every platform.
                let mut stack: Vec<usize> = node.children.clone();
                while let Some(idx) = stack.pop() {
                    if let Some(child) = nodes.get_mut(idx) {
                        child.closed = true;
                        stack.extend(child.children.iter().copied());
                    }
                }
            }
        }
        Ok(())
    }

    fn move_to(&self, el: &ElementData, x: i32, y: i32) -> Result<()> {
        self.live_window(el)?;
        {
            let mut nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(node) = nodes.get_mut(el.handle as usize) {
                if let Some(b) = node.data.bounds.as_mut() {
                    b.x = x;
                    b.y = y;
                }
            }
        }
        self.record(el, "move_to", Some(format!("{x},{y}")))
    }

    fn resize_to(&self, el: &ElementData, width: u32, height: u32) -> Result<()> {
        self.live_window(el)?;
        {
            let mut nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(node) = nodes.get_mut(el.handle as usize) {
                if let Some(b) = node.data.bounds.as_mut() {
                    b.width = width;
                    b.height = height;
                }
            }
        }
        self.record(el, "resize_to", Some(format!("{width}x{height}")))
    }

    fn subscribe(&self, _el: &ElementData) -> Result<Subscription> {
        Err(Error::Platform {
            code: -1,
            message: "MockProvider does not support subscribe".to_string(),
        })
    }
}

/// Build the standard test tree (Python/JS bindings share this).
///
/// Returns an `Arc<MockProvider>` so callers can inspect the action log via
/// [`MockProvider::actions`] while also using it as a `Provider` (via
/// `Arc<dyn Provider>`, supported by the blanket `&T: Provider` impl and
/// `Arc`'s `Deref` coercion).
pub fn build_provider() -> Arc<MockProvider> {
    use serde_json::json;

    let elements: Vec<MockElementSpec> = vec![
        (
            Role::Application,
            Some("TestApp"),
            None,
            Some("Test application"),
            Some(Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }),
            vec![],
            StateSet::default(),
            None,
            None,
            None,
            Some("app-root"),
            // Example raw metadata — gives the tests a concrete value to
            // assert on via Element.raw.
            Some(HashMap::from([(
                "ax_role".to_string(),
                json!("AXApplication"),
            )])),
        ),
        (
            Role::Window,
            Some("Main Window"),
            None,
            None,
            Some(Rect {
                x: 100,
                y: 50,
                width: 800,
                height: 600,
            }),
            vec![
                "raise",
                "minimize",
                "maximize",
                "restore",
                "close",
                "move_to",
                "resize_to",
            ],
            // The mock models the foreground app, so its main window is the
            // active window — mirrors `focused_app` returning the app root.
            StateSet {
                focused: true,
                active: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
            None,
        ),
        (
            Role::Toolbar,
            Some("Navigation"),
            None,
            None,
            None,
            vec![],
            StateSet::default(),
            None,
            None,
            None,
            None,
            None,
        ),
        (
            Role::Button,
            Some("Back"),
            None,
            Some("Go back"),
            Some(Rect {
                x: 110,
                y: 60,
                width: 50,
                height: 30,
            }),
            vec!["press", "focus"],
            StateSet {
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            Some("btn-back"),
            None,
        ),
        (
            Role::Button,
            Some("Forward"),
            None,
            None,
            Some(Rect {
                x: 170,
                y: 60,
                width: 50,
                height: 30,
            }),
            vec!["press", "focus"],
            StateSet {
                enabled: false,
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
            None,
        ),
        (
            Role::Group,
            Some("Content"),
            None,
            None,
            None,
            vec![],
            StateSet::default(),
            None,
            None,
            None,
            None,
            None,
        ),
        (
            Role::TextField,
            Some("Search"),
            Some("hello"),
            Some("Search field"),
            Some(Rect {
                x: 200,
                y: 120,
                width: 300,
                height: 25,
            }),
            vec!["focus", "set_value", "type_text"],
            StateSet {
                editable: true,
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
            None,
        ),
        (
            Role::CheckBox,
            Some("Agree"),
            None,
            None,
            None,
            vec!["press", "focus"],
            StateSet {
                checked: Some(Toggled::On),
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
            None,
        ),
        (
            Role::Slider,
            Some("Volume"),
            Some("75"),
            None,
            None,
            vec!["increment", "decrement", "set_value", "focus"],
            StateSet {
                focusable: true,
                ..StateSet::default()
            },
            Some(75.0),
            Some(0.0),
            Some(100.0),
            None,
            None,
        ),
        (
            Role::StaticText,
            Some("Status"),
            Some("Loading..."),
            None,
            None,
            vec![],
            StateSet {
                visible: false,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
            None,
        ),
        (
            Role::List,
            Some("Items"),
            None,
            None,
            None,
            vec![],
            StateSet {
                expanded: Some(true),
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
            None,
        ),
        (
            Role::ListItem,
            Some("Item 1"),
            None,
            None,
            None,
            vec!["select", "focus"],
            StateSet {
                selected: true,
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
            None,
        ),
        (
            Role::ListItem,
            Some("Item 2"),
            None,
            None,
            None,
            vec!["select", "focus"],
            StateSet {
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
            None,
        ),
        // ── Shell surfaces (indices 13.., parentless) ─────────────────
        // The taskbar surface: a chevron that opens the tray overflow plus
        // one visible tray icon — the shape the Windows overflow workflow
        // drives.
        (
            Role::Toolbar,
            Some("Taskbar"),
            None,
            None,
            Some(Rect {
                x: 0,
                y: 1040,
                width: 1920,
                height: 40,
            }),
            vec![],
            StateSet::default(),
            None,
            None,
            None,
            Some("Shell_TrayWnd"),
            None,
        ),
        (
            Role::Button,
            Some("Show Hidden Icons"),
            None,
            Some("Open the tray overflow"),
            Some(Rect {
                x: 1700,
                y: 1045,
                width: 30,
                height: 30,
            }),
            vec!["press", "focus"],
            StateSet {
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            Some("systray-chevron"),
            None,
        ),
        (
            Role::Button,
            Some("Volume"),
            None,
            None,
            Some(Rect {
                x: 1740,
                y: 1045,
                width: 30,
                height: 30,
            }),
            vec!["press", "focus"],
            StateSet {
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            Some("SystemTrayIcon"),
            None,
        ),
        // A second surface of a different kind, so kind filtering and the
        // ambiguity refusal have something to discriminate between.
        (
            Role::List,
            Some("Desktop"),
            None,
            None,
            None,
            vec![],
            StateSet::default(),
            None,
            None,
            None,
            Some("Progman"),
            None,
        ),
        (
            Role::ListItem,
            Some("Trash"),
            None,
            None,
            None,
            vec!["select", "focus"],
            StateSet {
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
            None,
        ),
    ];

    // Parent/child topology indexed by position in `elements`.
    let children_map: Vec<Vec<usize>> = vec![
        vec![1],              // 0: application
        vec![2, 5],           // 1: window
        vec![3, 4],           // 2: toolbar
        vec![],               // 3: button Back
        vec![],               // 4: button Forward
        vec![6, 7, 8, 9, 10], // 5: group
        vec![],               // 6: text_field
        vec![],               // 7: check_box
        vec![],               // 8: slider
        vec![],               // 9: static_text
        vec![11, 12],         // 10: list
        vec![],               // 11: list_item 1
        vec![],               // 12: list_item 2
        vec![14, 15],         // 13: taskbar surface root
        vec![],               // 14: button Show Hidden Icons
        vec![],               // 15: button Volume
        vec![17],             // 16: desktop surface root
        vec![],               // 17: list_item Trash
    ];
    let parent_map: Vec<Option<usize>> = vec![
        None,
        Some(0),
        Some(1),
        Some(2),
        Some(2),
        Some(1),
        Some(5),
        Some(5),
        Some(5),
        Some(5),
        Some(5),
        Some(10),
        Some(10),
        // Shell surface roots are top-level in their own right: they have no
        // parent, and nothing in the application subtree points at them.
        None,
        Some(13),
        Some(13),
        None,
        Some(16),
    ];

    let mut nodes = Vec::with_capacity(elements.len());
    for (i, (role, name, value, desc, bounds, actions, states, nv, minv, maxv, sid, raw)) in
        elements.into_iter().enumerate()
    {
        // Shell surfaces are hosted by the mock's shell process, not by the
        // test app, so they carry their own pid.
        let pid = if i >= FIRST_SHELL_NODE {
            MOCK_SHELL_PID
        } else {
            1234
        };
        let data = ElementData {
            role,
            name: name.map(String::from),
            value: value.map(String::from),
            description: desc.map(String::from),
            bounds,
            actions: actions.iter().map(|s| s.to_string()).collect(),
            states,
            numeric_value: nv,
            min_value: minv,
            max_value: maxv,
            stable_id: sid.map(String::from),
            pid: Some(pid),
            raw: raw.unwrap_or_default(),
            handle: i as u64,
        };
        nodes.push(MockNode {
            data,
            children: children_map[i].clone(),
            parent: parent_map[i],
            closed: false,
        });
    }

    Arc::new(MockProvider {
        nodes: Mutex::new(nodes),
        actions: Mutex::new(Vec::new()),
    })
}

/// Build a [`Subscription`] whose underlying sender has already been dropped.
///
/// Used by binding tests to verify that subscriber loops terminate cleanly on
/// disconnect (rather than hanging or silently swallowing the end-of-stream
/// signal).
pub fn disconnected_subscription() -> Subscription {
    use crate::event_provider::{CancelHandle, EventReceiver};

    let (tx, rx) = std::sync::mpsc::channel::<crate::event::Event>();
    drop(tx); // immediate disconnect
    Subscription::new(EventReceiver::new(rx), CancelHandle::noop())
}
