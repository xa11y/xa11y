use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionData, ScrollDirection};
use crate::error::{Error, Result};
use crate::provider::{AppTarget, Provider, QueryOptions};
use crate::role::Role;
use crate::selector::Selector;

/// Internal index for a node within a snapshot (sequential DFS order).
/// This is an array index, not a stable identity — it changes between snapshots.
#[doc(hidden)]
pub type NodeIndex = u32;

// ── TreeData ────────────────────────────────────────────────────────────────

/// Internal shared snapshot data backing all `Node` cursors from the same snapshot.
pub struct TreeData {
    /// Application name
    pub app_name: String,
    /// Process ID. `None` for multi-app snapshots.
    pub pid: Option<u32>,
    /// Screen dimensions at capture time (width, height)
    pub screen_size: (u32, u32),
    /// All raw nodes in DFS order
    pub nodes: Vec<RawNode>,
    /// Provider for re-fetching and actions. `None` for detached/deserialized snapshots.
    pub provider: Option<Arc<dyn Provider>>,
    /// App target for re-fetching. `None` for multi-app or detached snapshots.
    pub target: Option<AppTarget>,
}

impl std::fmt::Debug for TreeData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeData")
            .field("app_name", &self.app_name)
            .field("pid", &self.pid)
            .field("nodes_len", &self.nodes.len())
            .finish()
    }
}

// ── RawNode (the data struct, formerly Node) ────────────────────────────────

/// Raw node data within an accessibility tree snapshot.
///
/// This is the internal data representation. Most consumers should use [`Node`]
/// (a cursor that provides navigation, queries, and actions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawNode {
    /// Element role
    pub role: Role,

    /// Human-readable name (title, label)
    pub name: Option<String>,

    /// Current value (text content, slider position, etc.)
    pub value: Option<String>,

    /// Supplementary description (tooltip, help text)
    pub description: Option<String>,

    /// Bounding rectangle in screen pixels
    pub bounds: Option<Rect>,

    /// Available actions
    pub actions: Vec<Action>,

    /// Current state flags
    pub states: StateSet,

    /// Numeric value for range controls (sliders, progress bars, spinners).
    pub numeric_value: Option<f64>,

    /// Minimum value for range controls.
    pub min_value: Option<f64>,

    /// Maximum value for range controls.
    pub max_value: Option<f64>,

    /// Platform-assigned stable identifier for cross-snapshot correlation.
    /// - macOS: `AXIdentifier`
    /// - Windows: `AutomationId`
    /// - Linux: D-Bus `object_path`
    ///
    /// Not all elements have one.
    pub stable_id: Option<String>,

    /// Platform-specific raw data
    pub raw: RawPlatformData,

    /// Process ID. Populated for Application-role nodes.
    pub pid: Option<u32>,

    /// macOS bundle identifier. Populated for Application-role nodes.
    pub bundle_id: Option<String>,

    // ── Internal fields ──────────────────────────────────────────────────────
    /// Sequential DFS index within the snapshot.
    #[doc(hidden)]
    pub index: NodeIndex,

    /// Child node indices (direct children only).
    #[doc(hidden)]
    pub children_indices: Vec<NodeIndex>,

    /// Parent node index (None for root).
    #[doc(hidden)]
    pub parent_index: Option<NodeIndex>,
}

impl RawNode {
    /// Create a synthetic empty node, used as a placeholder when a wait
    /// condition is satisfied by the *absence* of a node (e.g. Detached/Hidden).
    pub fn synthetic_empty() -> Self {
        Self {
            role: Role::Unknown,
            name: None,
            value: None,
            description: None,
            bounds: None,
            actions: vec![],
            states: StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            raw: RawPlatformData::Synthetic,
            pid: None,
            bundle_id: None,
            index: 0,
            children_indices: vec![],
            parent_index: None,
        }
    }
}

// ── Node (public cursor) ────────────────────────────────────────────────────

/// An element in the accessibility tree.
///
/// `Node` is a cursor into a shared snapshot. Property access and navigation
/// (`children`, `parent`) read from the snapshot — fast, no IPC.
/// [`query()`](Node::query) and action methods (`press()`, `focus()`, etc.)
/// take a fresh snapshot from the OS.
///
/// # Example
/// ```no_run
/// # fn example(node: xa11y_core::Node) -> xa11y_core::Result<()> {
/// // Navigate snapshot (fast, no IPC)
/// for child in node.children() {
///     println!("{}: {:?}", child.role().to_snake_case(), child.name());
/// }
/// // Query re-fetches from OS
/// let buttons = node.query("button")?;
/// // Actions auto-refresh
/// buttons[0].press()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Node {
    data: Arc<TreeData>,
    index: u32,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let raw = self.raw_node();
        f.debug_struct("Node")
            .field("role", &raw.role)
            .field("name", &raw.name)
            .field("index", &self.index)
            .finish()
    }
}

impl Node {
    /// Create a Node cursor from shared tree data and an index.
    pub fn new(data: Arc<TreeData>, index: u32) -> Self {
        Self { data, index }
    }

    /// Access the underlying raw node data.
    pub fn raw_node(&self) -> &RawNode {
        &self.data.nodes[self.index as usize]
    }

    /// Access the underlying shared tree data.
    pub fn tree_data(&self) -> &Arc<TreeData> {
        &self.data
    }

    /// Get the node's DFS index within its snapshot.
    #[doc(hidden)]
    pub fn node_index(&self) -> u32 {
        self.index
    }

    // ── Properties (read from snapshot — fast, no IPC) ──────────────────

    /// Element role.
    pub fn role(&self) -> Role {
        self.raw_node().role
    }

    /// Human-readable name (title, label).
    pub fn name(&self) -> Option<&str> {
        self.raw_node().name.as_deref()
    }

    /// Current value (text content, slider position, etc.).
    pub fn value(&self) -> Option<&str> {
        self.raw_node().value.as_deref()
    }

    /// Supplementary description (tooltip, help text).
    pub fn description(&self) -> Option<&str> {
        self.raw_node().description.as_deref()
    }

    /// Bounding rectangle in screen pixels.
    pub fn bounds(&self) -> Option<Rect> {
        self.raw_node().bounds
    }

    /// Available actions.
    pub fn actions_list(&self) -> &[Action] {
        &self.raw_node().actions
    }

    /// Current state flags.
    pub fn states(&self) -> &StateSet {
        &self.raw_node().states
    }

    /// Numeric value for range controls.
    pub fn numeric_value(&self) -> Option<f64> {
        self.raw_node().numeric_value
    }

    /// Minimum value for range controls.
    pub fn min_value(&self) -> Option<f64> {
        self.raw_node().min_value
    }

    /// Maximum value for range controls.
    pub fn max_value(&self) -> Option<f64> {
        self.raw_node().max_value
    }

    /// Platform-assigned stable identifier for cross-snapshot correlation.
    pub fn stable_id(&self) -> Option<&str> {
        self.raw_node().stable_id.as_deref()
    }

    /// Platform-specific raw data.
    pub fn raw_platform_data(&self) -> &RawPlatformData {
        &self.raw_node().raw
    }

    /// Process ID. Populated for Application-role nodes.
    pub fn pid(&self) -> Option<u32> {
        self.raw_node().pid.or(self.data.pid)
    }

    /// macOS bundle identifier. Populated for Application-role nodes.
    pub fn bundle_id(&self) -> Option<&str> {
        self.raw_node().bundle_id.as_deref()
    }

    /// Application name (from the snapshot metadata).
    pub fn app_name(&self) -> &str {
        &self.data.app_name
    }

    /// Screen dimensions at capture time.
    pub fn screen_size(&self) -> (u32, u32) {
        self.data.screen_size
    }

    // ── Navigation (read from snapshot — fast, no IPC) ──────────────────

    /// Direct children of this node.
    pub fn children(&self) -> Vec<Node> {
        self.raw_node()
            .children_indices
            .iter()
            .map(|&idx| Node::new(Arc::clone(&self.data), idx))
            .collect()
    }

    /// Parent of this node, if any (root has no parent).
    pub fn parent(&self) -> Option<Node> {
        self.raw_node()
            .parent_index
            .map(|idx| Node::new(Arc::clone(&self.data), idx))
    }

    /// All descendants of this node (including itself), in DFS order.
    pub fn subtree(&self) -> Vec<Node> {
        let mut result = Vec::new();
        self.collect_subtree(self.index, &mut result);
        result
    }

    fn collect_subtree(&self, index: u32, result: &mut Vec<Node>) {
        if let Some(raw) = self.data.nodes.get(index as usize) {
            result.push(Node::new(Arc::clone(&self.data), index));
            for &child_idx in &raw.children_indices {
                self.collect_subtree(child_idx, result);
            }
        }
    }

    /// Number of nodes in this node's subtree (including itself).
    pub fn subtree_size(&self) -> usize {
        fn count(nodes: &[RawNode], index: u32) -> usize {
            let Some(raw) = nodes.get(index as usize) else {
                return 0;
            };
            1 + raw
                .children_indices
                .iter()
                .map(|&idx| count(nodes, idx))
                .sum::<usize>()
        }
        count(&self.data.nodes, self.index)
    }

    // ── Query (re-fetches fresh tree from OS) ───────────────────────────

    /// Query for elements matching a CSS-like selector within this node's app.
    ///
    /// **Takes a fresh snapshot** from the OS, then runs the selector.
    /// Returns nodes from the fresh snapshot.
    pub fn query(&self, selector_str: &str) -> Result<Vec<Node>> {
        let provider = self.data.provider.as_ref().ok_or(Error::Detached)?;
        let target = self.data.target.as_ref().ok_or(Error::Detached)?;

        let tree = provider.get_app_tree(target, &QueryOptions::default())?;
        let data = Arc::new(TreeData {
            app_name: tree.app_name.clone(),
            pid: tree.pid,
            screen_size: tree.screen_size,
            nodes: tree.nodes,
            provider: Some(Arc::clone(provider)),
            target: Some(target.clone()),
        });

        let selector = Selector::parse(selector_str)?;
        let indices = selector.match_raw_nodes(&data.nodes);
        Ok(indices
            .into_iter()
            .map(|idx| Node::new(Arc::clone(&data), idx))
            .collect())
    }

    /// Create a [`Locator`](crate::locator::Locator) scoped to this node's app.
    pub fn locator(&self, selector: &str) -> Result<crate::locator::Locator> {
        let provider = self.data.provider.as_ref().ok_or(Error::Detached)?;
        let target = self.data.target.as_ref().ok_or(Error::Detached)?;

        Ok(crate::locator::Locator::new(
            Arc::clone(provider),
            target.clone(),
            selector,
        ))
    }

    // ── Actions (re-fetch + re-locate + act) ────────────────────────────

    /// Re-fetch the tree, re-locate this node, and perform an action.
    fn perform_action_impl(&self, action: Action, data: Option<ActionData>) -> Result<()> {
        if let Some(ref d) = data {
            d.validate(action)?;
        }
        let provider = self.data.provider.as_ref().ok_or(Error::Detached)?;
        let target = self.data.target.as_ref().ok_or(Error::Detached)?;

        let tree = provider.get_app_tree(target, &QueryOptions::default())?;
        let fresh_index = relocate_node(self.raw_node(), &tree.nodes)?;
        provider.perform_action_raw(&tree, fresh_index, action, data)
    }

    /// Click / invoke this element.
    pub fn press(&self) -> Result<()> {
        self.perform_action_impl(Action::Press, None)
    }

    /// Set keyboard focus on this element.
    pub fn focus(&self) -> Result<()> {
        self.perform_action_impl(Action::Focus, None)
    }

    /// Toggle this element (checkbox, switch).
    pub fn toggle(&self) -> Result<()> {
        self.perform_action_impl(Action::Toggle, None)
    }

    /// Select this element (list item, etc.).
    pub fn select(&self) -> Result<()> {
        self.perform_action_impl(Action::Select, None)
    }

    /// Expand this element.
    pub fn expand(&self) -> Result<()> {
        self.perform_action_impl(Action::Expand, None)
    }

    /// Collapse this element.
    pub fn collapse(&self) -> Result<()> {
        self.perform_action_impl(Action::Collapse, None)
    }

    /// Set the text value of this element.
    pub fn set_value(&self, value: &str) -> Result<()> {
        self.perform_action_impl(Action::SetValue, Some(ActionData::Value(value.to_string())))
    }

    /// Set the numeric value of this element (slider, spinner).
    pub fn set_numeric_value(&self, value: f64) -> Result<()> {
        self.perform_action_impl(Action::SetValue, Some(ActionData::NumericValue(value)))
    }

    /// Increment this element (slider, spinner).
    pub fn increment(&self) -> Result<()> {
        self.perform_action_impl(Action::Increment, None)
    }

    /// Decrement this element (slider, spinner).
    pub fn decrement(&self) -> Result<()> {
        self.perform_action_impl(Action::Decrement, None)
    }

    /// Show the context menu for this element.
    pub fn show_menu(&self) -> Result<()> {
        self.perform_action_impl(Action::ShowMenu, None)
    }

    /// Scroll this element into view.
    pub fn scroll_into_view(&self) -> Result<()> {
        self.perform_action_impl(Action::ScrollIntoView, None)
    }

    /// Type text character-by-character on this element.
    pub fn type_text(&self, text: &str) -> Result<()> {
        self.perform_action_impl(Action::TypeText, Some(ActionData::Value(text.to_string())))
    }

    /// Select a text range within this element.
    pub fn select_text(&self, start: u32, end: u32) -> Result<()> {
        self.perform_action_impl(
            Action::SetTextSelection,
            Some(ActionData::TextSelection { start, end }),
        )
    }

    /// Remove keyboard focus from this element.
    pub fn blur(&self) -> Result<()> {
        self.perform_action_impl(Action::Blur, None)
    }

    /// Scroll upward.
    pub fn scroll_up(&self, amount: f64) -> Result<()> {
        self.perform_action_impl(
            Action::Scroll,
            Some(ActionData::ScrollAmount {
                direction: ScrollDirection::Up,
                amount,
            }),
        )
    }

    /// Scroll downward.
    pub fn scroll_down(&self, amount: f64) -> Result<()> {
        self.perform_action_impl(
            Action::Scroll,
            Some(ActionData::ScrollAmount {
                direction: ScrollDirection::Down,
                amount,
            }),
        )
    }

    /// Scroll leftward.
    pub fn scroll_left(&self, amount: f64) -> Result<()> {
        self.perform_action_impl(
            Action::Scroll,
            Some(ActionData::ScrollAmount {
                direction: ScrollDirection::Left,
                amount,
            }),
        )
    }

    /// Scroll rightward.
    pub fn scroll_right(&self, amount: f64) -> Result<()> {
        self.perform_action_impl(
            Action::Scroll,
            Some(ActionData::ScrollAmount {
                direction: ScrollDirection::Right,
                amount,
            }),
        )
    }

    // ── Debug ───────────────────────────────────────────────────────────

    /// Render this node's subtree as indented text for debugging.
    pub fn dump(&self) -> String {
        let subtree = self.subtree();
        // Compute depths relative to this node
        let base_depth = self.depth();
        let mut output = String::new();
        for node in &subtree {
            let depth = node.depth().saturating_sub(base_depth);
            let indent = "  ".repeat(depth as usize);
            let name_part = node
                .name()
                .map(|n| format!(" \"{}\"", n))
                .unwrap_or_default();
            let value_part = node
                .value()
                .map(|v| format!(" value=\"{}\"", v))
                .unwrap_or_default();
            output.push_str(&format!(
                "{}[{}] {}{}{}\n",
                indent,
                node.index,
                node.role().to_snake_case(),
                name_part,
                value_part,
            ));
        }
        output
    }

    /// Compute depth of this node by walking parent chain.
    fn depth(&self) -> u32 {
        let mut depth = 0u32;
        let mut idx = self.index;
        while let Some(parent_idx) = self
            .data
            .nodes
            .get(idx as usize)
            .and_then(|n| n.parent_index)
        {
            depth += 1;
            idx = parent_idx;
        }
        depth
    }
}

/// Re-locate a node in a fresh tree by stable_id or index+role verification.
fn relocate_node(old: &RawNode, fresh_nodes: &[RawNode]) -> Result<u32> {
    // 1. Try stable_id match
    if let Some(sid) = &old.stable_id {
        if let Some(node) = fresh_nodes
            .iter()
            .find(|n| n.stable_id.as_ref() == Some(sid))
        {
            return Ok(node.index);
        }
    }
    // 2. Fallback: same index + verify role match
    if let Some(node) = fresh_nodes.get(old.index as usize) {
        if node.role == old.role {
            return Ok(node.index);
        }
    }
    Err(Error::ElementStale {
        selector: format!(
            "{}{}",
            old.role.to_snake_case(),
            old.name
                .as_ref()
                .map(|n| format!("[name=\"{}\"]", n))
                .unwrap_or_default()
        ),
    })
}

// ── Supporting types ────────────────────────────────────────────────────────

/// Boolean state flags for a node.
///
/// **Semantics for non-applicable states:** When a state doesn't apply to an
/// element's role, the backend uses the platform's reported value or defaults:
/// - `enabled`: `true` (elements are enabled unless explicitly disabled)
/// - `visible`: `true` (elements are visible unless explicitly hidden/offscreen)
/// - `focused`, `focusable`, `modal`, `selected`, `editable`, `required`, `busy`: `false`
///
/// States that are inherently inapplicable use `Option`: `checked` is `None`
/// for non-checkable elements, `expanded` is `None` for non-expandable elements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateSet {
    pub enabled: bool,
    pub visible: bool,
    pub focused: bool,
    /// None = not checkable
    pub checked: Option<Toggled>,
    pub selected: bool,
    /// None = not expandable
    pub expanded: Option<bool>,
    pub editable: bool,
    /// Whether the element can receive keyboard focus
    pub focusable: bool,
    /// Whether the element is a modal dialog
    pub modal: bool,
    /// Form field required
    pub required: bool,
    /// Async operation in progress
    pub busy: bool,
}

impl Default for StateSet {
    fn default() -> Self {
        Self {
            enabled: true,
            visible: true,
            focused: false,
            checked: None,
            selected: false,
            expanded: None,
            editable: false,
            focusable: false,
            modal: false,
            required: false,
            busy: false,
        }
    }
}

/// Tri-state toggle value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Toggled {
    Off,
    On,
    /// Indeterminate / tri-state
    Mixed,
}

/// Screen-pixel bounding rectangle (origin + size).
/// `x`/`y` are signed to support negative multi-monitor coordinates.
/// `width`/`height` are unsigned (always non-negative).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Platform-specific raw data attached to every node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RawPlatformData {
    MacOS {
        ax_role: String,
        ax_subrole: Option<String>,
        ax_identifier: Option<String>,
    },
    Windows {
        control_type_id: i32,
        automation_id: Option<String>,
        class_name: Option<String>,
    },
    Linux {
        atspi_role: String,
        bus_name: String,
        object_path: String,
    },
    /// Placeholder for synthetic nodes with no real platform backing.
    Synthetic,
}
