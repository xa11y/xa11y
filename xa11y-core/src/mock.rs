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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::element::{ElementData, HiddenReason, Rect, StateSet, Toggled};
use crate::error::{Error, Result};
use crate::event_provider::Subscription;
use crate::provider::Provider;
use crate::role::Role;
use crate::shell::ShellSurfaceKind;
use crate::table::{CellParts, CollectionParts, Count, HeaderKind, RowParts, TableParts};
use crate::text_api::{
    check_in_bounds, slice_utf16, utf16_len, TextAttributeParts, TextRange, TextRun, TextUnit,
};

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
    /// Scripted geometry per node index, consumed by [`MockProvider::animate`].
    frames: Mutex<HashMap<usize, VecDeque<MockFrame>>>,
    /// Text selection per node handle, set by `set_text_selection`. A
    /// selection with `start == end` is a caret.
    selections: Mutex<HashMap<u64, TextRange>>,
}

/// One scripted observation of a node: its bounds and whether it is visible.
pub type MockFrame = (Option<Rect>, bool);

impl MockProvider {
    /// Return a clone of the action log recorded so far.
    pub fn actions(&self) -> Vec<ActionLogEntry> {
        self.actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Script how the node named `name` looks to successive tree reads.
    ///
    /// Each time `get_children` reports the node, the next frame's bounds and
    /// visibility are applied first; once the frames run out the last one
    /// persists. Deterministic stand-in for an animating widget, so
    /// `Locator::wait_stable` can be tested without a clock.
    pub fn animate(&self, name: &str, frames: Vec<MockFrame>) {
        let idx = self
            .nodes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .position(|n| n.data.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("mock fixture has no node named {name:?}"));
        self.frames
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(idx, frames.into());
    }

    /// Apply the next scripted frame (if any) to node `idx`.
    fn advance_frame(&self, nodes: &mut [MockNode], idx: usize) {
        let mut frames = self.frames.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(queue) = frames.get_mut(&idx) {
            // Keep the last frame: it is where the animation settles.
            let frame = if queue.len() > 1 {
                queue.pop_front()
            } else {
                queue.front().copied()
            };
            if let Some((bounds, visible)) = frame {
                nodes[idx].data.bounds = bounds;
                nodes[idx].data.states.visible = visible;
            }
        }
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

    /// The text of a node, which the mock takes to be its `value`. A node
    /// with no value has no text interface.
    fn text_of(&self, el: &ElementData) -> Result<String> {
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        nodes
            .get(el.handle as usize)
            .and_then(|n| n.data.value.clone())
            .ok_or_else(|| Error::ActionNotSupported {
                action: "text".into(),
                role: el.role,
            })
    }

    fn selection_of(&self, el: &ElementData) -> Option<TextRange> {
        self.selections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&el.handle)
            .copied()
    }

    fn require_table(&self, nodes: &[MockNode], table: &ElementData) -> Result<()> {
        match nodes.get(table.handle as usize) {
            Some(n) if n.data.table.is_some() => Ok(()),
            _ => Err(Error::ActionNotSupported {
                action: "table".into(),
                role: table.role,
            }),
        }
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
        let mut nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
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
                for child in nodes[idx].children.clone() {
                    self.advance_frame(&mut nodes, child);
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
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        Ok(nodes
            .iter()
            .filter(|node| node.data.role == Role::Application && !node.closed)
            .map(|node| node.data.clone())
            .collect())
    }

    fn app_roots(&self, app: &ElementData) -> Result<Vec<ElementData>> {
        if app.pid.is_none() {
            return Ok(vec![app.clone()]);
        }
        Ok(self
            .list_apps()?
            .into_iter()
            .filter(|root| root.pid == app.pid)
            .collect())
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
        // Only nodes with text keep a selection; others just log, as before.
        if let Ok(text) = self.text_of(el) {
            check_in_bounds(TextRange { start, end }, utf16_len(&text))?;
            self.selections
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(el.handle, TextRange { start, end });
        }
        self.record(el, "set_text_selection", Some(format!("{start}..{end}")))
    }

    fn table_cell_at(
        &self,
        table: &ElementData,
        row: u32,
        column: u32,
    ) -> Result<Option<ElementData>> {
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        self.require_table(&nodes, table)?;
        Ok(descendants(&nodes, table.handle as usize)
            .into_iter()
            .map(|i| &nodes[i].data)
            .find(|d| {
                d.cell.is_some_and(|c| {
                    let covers = |at: Option<u32>, span: Option<u32>, want: u32| {
                        at.is_some_and(|a| want >= a && want < a + span.unwrap_or(1))
                    };
                    covers(c.row, c.row_span, row) && covers(c.column, c.column_span, column)
                })
            })
            .cloned())
    }

    fn table_headers(&self, table: &ElementData, kind: HeaderKind) -> Result<Vec<ElementData>> {
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        self.require_table(&nodes, table)?;
        Ok(descendants(&nodes, table.handle as usize)
            .into_iter()
            .map(|i| &nodes[i].data)
            .filter(|d| d.cell.and_then(|c| c.header) == Some(kind))
            .cloned()
            .collect())
    }

    fn cell_headers(&self, cell: &ElementData, kind: HeaderKind) -> Result<Vec<ElementData>> {
        let info = cell.cell.ok_or_else(|| Error::ActionNotSupported {
            action: "cell_headers".into(),
            role: cell.role,
        })?;
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        // The table is the nearest ancestor with table info.
        let mut at = nodes.get(cell.handle as usize).and_then(|n| n.parent);
        while let Some(i) = at {
            if nodes[i].data.table.is_some() {
                break;
            }
            at = nodes[i].parent;
        }
        let Some(table) = at else {
            return Ok(Vec::new());
        };
        Ok(descendants(&nodes, table)
            .into_iter()
            .map(|i| &nodes[i].data)
            .filter(|d| d.handle != cell.handle)
            .filter(|d| {
                let Some(h) = d.cell else { return false };
                // A header heads every slot its span covers.
                let covers =
                    |at: Option<u32>, span: Option<u32>, want: Option<u32>| match (at, want) {
                        (Some(a), Some(w)) => w >= a && w < a + span.unwrap_or(1),
                        _ => false,
                    };
                h.header == Some(kind)
                    && match kind {
                        HeaderKind::Column => covers(h.column, h.column_span, info.column),
                        HeaderKind::Row => covers(h.row, h.row_span, info.row),
                    }
            })
            .cloned()
            .collect())
    }

    fn selected_items(&self, container: &ElementData) -> Result<Vec<ElementData>> {
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        Ok(descendants(&nodes, container.handle as usize)
            .into_iter()
            .map(|i| &nodes[i].data)
            .filter(|d| d.states.selected)
            .cloned()
            .collect())
    }

    fn text_length(&self, el: &ElementData) -> Result<u32> {
        Ok(utf16_len(&self.text_of(el)?))
    }

    fn text_caret(&self, el: &ElementData) -> Result<Option<u32>> {
        self.text_of(el)?;
        Ok(self.selection_of(el).map(|r| r.end))
    }

    fn text_selections(&self, el: &ElementData) -> Result<Vec<TextRange>> {
        self.text_of(el)?;
        Ok(self
            .selection_of(el)
            .filter(|r| !r.is_empty())
            .into_iter()
            .collect())
    }

    fn text_read(&self, el: &ElementData, range: TextRange) -> Result<String> {
        slice_utf16(&self.text_of(el)?, range)
    }

    fn text_range_at(&self, el: &ElementData, offset: u32, unit: TextUnit) -> Result<TextRange> {
        let text = self.text_of(el)?;
        mock_unit_at(&text, offset, unit)
    }

    fn text_range_bounds(&self, el: &ElementData, range: TextRange) -> Result<Option<Rect>> {
        let text = self.text_of(el)?;
        check_in_bounds(range, utf16_len(&text))?;
        // A fixed-pitch layout: 8 px per unit, 16 px per line, from the
        // element's origin.
        let Some(origin) = el.bounds else {
            return Ok(None);
        };
        let line = mock_unit_at(&text, range.start, TextUnit::Line)?;
        let line_no = text
            .encode_utf16()
            .take(line.start as usize)
            .filter(|&u| u == u16::from(b'\n'))
            .count() as i32;
        Ok(Some(Rect {
            x: origin.x + 8 * (range.start - line.start) as i32,
            y: origin.y + 16 * line_no,
            width: 8 * range.len(),
            height: 16,
        }))
    }

    fn text_attributes_at(&self, el: &ElementData, offset: u32) -> Result<TextRun> {
        let text = self.text_of(el)?;
        let length = utf16_len(&text);
        check_in_bounds(
            TextRange {
                start: offset,
                end: offset,
            },
            length,
        )?;
        // One run per word: words alternate bold / regular, so a multi-run
        // walk has something to page through.
        let word = mock_unit_at(&text, offset, TextUnit::Word)?;
        let words_before = text
            .encode_utf16()
            .take(word.start as usize)
            .filter(|&u| u == u16::from(b' '))
            .count();
        let bold = words_before % 2 == 0;
        Ok(TextRun::new(
            word,
            TextAttributeParts {
                font_family: Some("MockSans".into()),
                font_size: Some(12.0),
                font_weight: Some(if bold { 700 } else { 400 }),
                italic: Some(false),
                underline: None,
                strikethrough: None,
                foreground: None,
                background: None,
                script: None,
                spelling_error: None,
            }
            .into(),
        ))
    }
    fn perform_action(&self, el: &ElementData, action: &str) -> Result<()> {
        // Nullary window verbs delegate to the typed methods, which validate
        // the target and mutate the model — the mock must not record success
        // for a call that changes nothing, or a test that fails on every real
        // provider would pass here. The payload verbs take no arguments on
        // the generic escape hatch and fail surfaceably with how to call
        // them, mirroring the real providers' contract (see uia.rs; tenet 1).
        match action {
            "activate" => self.activate(el),
            "minimize" => self.minimize(el),
            "maximize" => self.maximize(el),
            "enter_fullscreen" => self.enter_fullscreen(el),
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

    fn activate(&self, el: &ElementData) -> Result<()> {
        self.live_window(el)?;
        self.record(el, "activate", None)
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
                // Minimizing leaves fullscreen, as the macOS provider does
                // (it must exit the fullscreen state before the iconify can
                // take effect). The mock decided it, so `Some(false)`.
                node.data.states.fullscreen = Some(false);
                // Model an iconified window as off-screen. That is what makes
                // the Locator window verbs' enabled-only gate (no `visible`)
                // testable: a minimized window must still be actionable.
                node.data.states.visible = false;
                node.data.states.hidden_reason = Some(HiddenReason::WindowMinimized);
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
                node.data.states.hidden_reason = None;
            }
        }
        self.record(el, "maximize", None)
    }

    fn enter_fullscreen(&self, el: &ElementData) -> Result<()> {
        self.live_window(el)?;
        {
            let mut nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(node) = nodes.get_mut(el.handle as usize) {
                node.data.states.fullscreen = Some(true);
                // Fullscreen and the other states are exclusive: entering it
                // clears the minimized flag it would otherwise leave the
                // window in the Dock, and the maximized state it supersedes.
                node.data.states.minimized = Some(false);
                node.data.states.maximized = Some(false);
                node.data.states.visible = true;
                node.data.states.hidden_reason = None;
            }
        }
        self.record(el, "enter_fullscreen", None)
    }

    fn restore(&self, el: &ElementData) -> Result<()> {
        self.live_window(el)?;
        {
            let mut nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(node) = nodes.get_mut(el.handle as usize) {
                node.data.states.minimized = Some(false);
                node.data.states.maximized = Some(false);
                node.data.states.fullscreen = Some(false);
                node.data.states.visible = true;
                node.data.states.hidden_reason = None;
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
                node.data.states.hidden_reason = Some(HiddenReason::NotShowing);
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
                "activate",
                "minimize",
                "maximize",
                "enter_fullscreen",
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
                hidden_reason: Some(HiddenReason::NotShowing),
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
            table: None,
            cell: None,
            table_row: None,
            collection: None,
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
        frames: Mutex::new(HashMap::new()),
        selections: Mutex::new(HashMap::new()),
    })
}

/// Build the test tree plus one `web_area` per entry of `filled` under the
/// main window; an entry that is `true` holds a button, one that is `false`
/// is the empty web area a Chromium app shows before its tree is built.
pub fn build_web_area_provider(filled: &[bool]) -> Arc<MockProvider> {
    let provider = build_provider();
    {
        let mut nodes = provider.nodes.lock().unwrap_or_else(|e| e.into_inner());
        let window_index = 1;
        for (i, has_content) in filled.iter().enumerate() {
            let area_index = nodes.len();
            let mut area = nodes[window_index].clone();
            area.data = ElementData::for_role(Role::WebArea);
            area.data.handle = area_index as u64;
            area.data.pid = nodes[window_index].data.pid;
            area.data.name = Some(format!("Page {i}"));
            area.children = Vec::new();
            area.parent = Some(window_index);
            nodes.push(area);
            nodes[window_index].children.push(area_index);
            if *has_content {
                let button_index = nodes.len();
                let mut button = nodes[area_index].clone();
                button.data = ElementData::for_role(Role::Button);
                button.data.handle = button_index as u64;
                button.data.pid = nodes[window_index].data.pid;
                button.data.name = Some(format!("Inner {i}"));
                button.children = Vec::new();
                button.parent = Some(area_index);
                nodes.push(button);
                nodes[area_index].children.push(button_index);
            }
        }
    }
    provider
}

/// Build two native application roots for one process, each with a window.
/// Exercises process scope through the actual Python and JavaScript bindings.
pub fn build_split_provider() -> Arc<MockProvider> {
    let provider = build_provider();
    {
        let mut nodes = provider.nodes.lock().unwrap_or_else(|e| e.into_inner());
        let root_index = nodes.len();
        let window_index = root_index + 1;
        let mut root = nodes[0].clone();
        root.data.handle = root_index as u64;
        root.data.stable_id = Some("second-app-root".into());
        root.children = vec![window_index];
        let mut window = nodes[1].clone();
        window.data.handle = window_index as u64;
        window.data.stable_id = Some("second-window".into());
        window.data.name = Some("Second Window".into());
        window.children.clear();
        window.parent = Some(root_index);
        nodes.extend([root, window]);
    }
    provider
}

/// Every descendant of `root`, in tree order.
fn descendants(nodes: &[MockNode], root: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut stack: Vec<usize> = nodes
        .get(root)
        .map(|n| n.children.iter().rev().copied().collect())
        .unwrap_or_default();
    while let Some(i) = stack.pop() {
        out.push(i);
        stack.extend(nodes[i].children.iter().rev().copied());
    }
    out
}

/// The mock's text units: words split on spaces, lines and paragraphs on
/// `\n` (the mock does not wrap), characters as scalar values.
fn mock_unit_at(text: &str, offset: u32, unit: TextUnit) -> Result<TextRange> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let length = units.len() as u32;
    check_in_bounds(
        TextRange {
            start: offset,
            end: offset,
        },
        length,
    )?;
    if length == 0 {
        return Ok(TextRange { start: 0, end: 0 });
    }
    // At the end, answer for the unit that ends there.
    let at = if offset == length { offset - 1 } else { offset } as usize;
    let is_sep = |u: u16| match unit {
        TextUnit::Word => u == u16::from(b' ') || u == u16::from(b'\n'),
        TextUnit::Line | TextUnit::Paragraph => u == u16::from(b'\n'),
        TextUnit::Character => false,
    };
    if unit == TextUnit::Character {
        let start = if (0xDC00..=0xDFFF).contains(&units[at]) && at > 0 {
            at - 1
        } else {
            at
        };
        let width = if (0xD800..=0xDBFF).contains(&units[start]) {
            2
        } else {
            1
        };
        return Ok(TextRange {
            start: start as u32,
            end: (start + width).min(units.len()) as u32,
        });
    }
    let mut start = at;
    while start > 0 && !is_sep(units[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    // A unit includes its trailing separator, so consecutive units tile.
    while end < units.len() && !is_sep(units[end]) {
        end += 1;
    }
    if end < units.len() {
        end += 1;
    }
    Ok(TextRange {
        start: start as u32,
        end: end as u32,
    })
}

/// Build the test tree plus a table, a virtualized list and a tree under the
/// main window, for the table, collection and text APIs.
///
/// ```text
/// table "Prices" (3 rows x 3 columns)
/// ├── table_row (index 0)
/// │   ├── table_cell "Product" (0,0, column header)
/// │   └── table_cell "Price" (0,1, spans 2 columns, column header)
/// ├── table_row (index 1)
/// │   ├── table_cell "Widget" (1,0, row header)
/// │   ├── table_cell "1.00" (1,1)
/// │   └── table_cell "2.00" (1,2, selected)
/// └── table_row (index 2)
///     ├── table_cell "Gadget" (2,0, row header)
///     ├── table_cell "3.00" (2,1, no spans reported)
///     └── table_cell "4.00" (2,2)
/// list "Results" (virtualized: size unknown)
/// ├── list_item "First" (index 0)
/// └── list_item "Second" (index 1)
/// list "Folders" (a tree: xa11y maps tree containers to list)
/// ├── tree_item "Root" (index 0 of 1, depth 0)
/// └── tree_item "Child" (index 0 of 1, depth 1)
/// text_area "Notes" (value "Hello 😀 world\nsecond line")
/// ```
pub fn build_table_provider() -> Arc<MockProvider> {
    let provider = build_provider();
    {
        let mut nodes = provider.nodes.lock().unwrap_or_else(|e| e.into_inner());
        let window = 1;
        let pid = nodes[window].data.pid;
        let add = |nodes: &mut Vec<MockNode>, parent: usize, data: ElementData| {
            let i = nodes.len();
            let mut data = data;
            data.handle = i as u64;
            data.pid = pid;
            if data.bounds.is_none() {
                data.bounds = Some(Rect {
                    x: 10,
                    y: 10 * i as i32,
                    width: 100,
                    height: 20,
                });
            }
            nodes.push(MockNode {
                data,
                children: Vec::new(),
                parent: Some(parent),
                closed: false,
            });
            nodes[parent].children.push(i);
            i
        };
        let named = |role: Role, name: &str| {
            let mut d = ElementData::for_role(role);
            d.name = Some(name.into());
            d
        };
        let cell = |name: &str,
                    row: u32,
                    column: u32,
                    spans: Option<(u32, u32)>,
                    header: Option<HeaderKind>| {
            let mut d = named(Role::TableCell, name);
            d.cell = Some(
                CellParts {
                    row: Some(row),
                    column: Some(column),
                    row_span: spans.map(|s| s.0),
                    column_span: spans.map(|s| s.1),
                    header,
                }
                .into(),
            );
            d
        };
        let mut table = named(Role::Table, "Prices");
        table.table = Some(
            TableParts {
                row_count: Some(Count::Known(3)),
                column_count: Some(Count::Known(3)),
            }
            .into(),
        );
        let t = add(&mut nodes, window, table);
        let rows: [Vec<ElementData>; 3] = [
            vec![
                cell("Product", 0, 0, Some((1, 1)), Some(HeaderKind::Column)),
                cell("Price", 0, 1, Some((1, 2)), Some(HeaderKind::Column)),
            ],
            vec![
                cell("Widget", 1, 0, Some((1, 1)), Some(HeaderKind::Row)),
                cell("1.00", 1, 1, Some((1, 1)), None),
                {
                    let mut c = cell("2.00", 1, 2, Some((1, 1)), None);
                    c.states.selected = true;
                    c
                },
            ],
            vec![
                cell("Gadget", 2, 0, Some((1, 1)), Some(HeaderKind::Row)),
                cell("3.00", 2, 1, None, None),
                cell("4.00", 2, 2, Some((1, 1)), None),
            ],
        ];
        for (index, cells) in rows.into_iter().enumerate() {
            let mut row = ElementData::for_role(Role::TableRow);
            row.table_row = Some(
                RowParts {
                    index: Some(index as u32),
                }
                .into(),
            );
            let r = add(&mut nodes, t, row);
            for c in cells {
                add(&mut nodes, r, c);
            }
        }
        let item = |role: Role, name: &str, index: u32, size: Option<Count>, depth: Option<u32>| {
            let mut d = named(role, name);
            d.collection = Some(
                CollectionParts {
                    index: Some(index),
                    size,
                    depth,
                }
                .into(),
            );
            d
        };
        let list = add(&mut nodes, window, named(Role::List, "Results"));
        add(
            &mut nodes,
            list,
            item(Role::ListItem, "First", 0, Some(Count::Unknown), None),
        );
        add(
            &mut nodes,
            list,
            item(Role::ListItem, "Second", 1, Some(Count::Unknown), None),
        );
        let tree = add(&mut nodes, window, named(Role::List, "Folders"));
        let root = add(
            &mut nodes,
            tree,
            item(Role::TreeItem, "Root", 0, Some(Count::Known(1)), Some(0)),
        );
        add(
            &mut nodes,
            root,
            item(Role::TreeItem, "Child", 0, Some(Count::Known(1)), Some(1)),
        );
        let mut notes = named(Role::TextArea, "Notes");
        notes.value = Some("Hello 😀 world\nsecond line".into());
        notes.states.editable = true;
        add(&mut nodes, window, notes);
    }
    provider
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

/// Build a [`Subscription`] that yields `events` in order and then reports
/// a disconnect.
///
/// Lets binding tests drive their event conversion with real core events,
/// without a platform event source.
pub fn scripted_subscription(events: Vec<crate::event::Event>) -> Subscription {
    use crate::event_provider::{CancelHandle, EventReceiver};

    let (tx, rx) = std::sync::mpsc::channel::<crate::event::Event>();
    for event in events {
        // The receiver is alive in this scope, so the send cannot fail.
        let _ = tx.send(event);
    }
    drop(tx);
    Subscription::new(EventReceiver::new(rx), CancelHandle::noop())
}

/// The text events the binding tests convert, in order:
///
/// 1. `TextChanged` inserting `"hé😀"` at `3..7` (UTF-16, as Qt reports it).
/// 2. `TextChanged` deleting `"x"` with no range (a code-point toolkit).
/// 3. `TextChanged` with no delta (macOS, Windows).
/// 4. `TextSelectionChanged`.
pub fn text_edit_events() -> Vec<crate::event::Event> {
    use crate::event::{Event, EventKind, TextChangeKind, TextChangeParts};
    use crate::text_api::TextRange;

    let mut inserted = Event::new(EventKind::TextChanged, "Mock", 1);
    inserted.text_change = Some(
        TextChangeParts {
            kind: TextChangeKind::Inserted,
            range: Some(TextRange { start: 3, end: 7 }),
            text: Some("hé😀".to_string()),
        }
        .into(),
    );
    let mut deleted = Event::new(EventKind::TextChanged, "Mock", 1);
    deleted.text_change = Some(
        TextChangeParts {
            kind: TextChangeKind::Deleted,
            range: None,
            text: Some("x".to_string()),
        }
        .into(),
    );
    vec![
        inserted,
        deleted,
        Event::new(EventKind::TextChanged, "Mock", 1),
        Event::new(EventKind::TextSelectionChanged, "Mock", 1),
    ]
}

/// Display-free input backend for binding geometry and host-thread tests.
#[doc(hidden)]
pub struct MockInputProvider {
    delay: std::time::Duration,
    held: std::sync::atomic::AtomicU32,
}

impl MockInputProvider {
    pub fn new(delay: std::time::Duration) -> Self {
        Self {
            delay,
            held: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn bit(button: crate::input::MouseButton) -> u32 {
        match button {
            crate::input::MouseButton::Left => 1,
            crate::input::MouseButton::Right => 2,
            crate::input::MouseButton::Middle => 4,
        }
    }
}

impl crate::input::InputProvider for MockInputProvider {
    fn validate_pointer_target(&self, point: crate::input::Point) -> Result<()> {
        std::thread::sleep(self.delay);
        crate::input::validate_pointer_regions(
            point,
            point,
            "mock desktop pixels",
            &[
                Rect {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100,
                },
                Rect {
                    x: -100,
                    y: -100,
                    width: 100,
                    height: 100,
                },
            ],
        )
    }
    fn pointer_buttons_down(&self) -> bool {
        self.held.load(std::sync::atomic::Ordering::Acquire) != 0
    }
    fn pointer_move(&self, _: crate::input::Point) -> Result<()> {
        Ok(())
    }
    fn pointer_down(&self, button: crate::input::MouseButton) -> Result<()> {
        self.held
            .fetch_or(Self::bit(button), std::sync::atomic::Ordering::AcqRel);
        Ok(())
    }
    fn pointer_up(&self, button: crate::input::MouseButton) -> Result<()> {
        self.held
            .fetch_and(!Self::bit(button), std::sync::atomic::Ordering::AcqRel);
        Ok(())
    }
    fn pointer_click(
        &self,
        _: crate::input::Point,
        _: crate::input::MouseButton,
        _: u32,
    ) -> Result<()> {
        Ok(())
    }
    fn pointer_scroll(&self, _: crate::input::Point, _: crate::input::ScrollDelta) -> Result<()> {
        Ok(())
    }
    fn key_down(&self, _: &crate::input::Key) -> Result<()> {
        Ok(())
    }
    fn key_up(&self, _: &crate::input::Key) -> Result<()> {
        Ok(())
    }
    fn type_text(&self, _: &str) -> Result<()> {
        Ok(())
    }
}
