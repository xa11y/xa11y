use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::action::Action;
use crate::role::Role;
use crate::tree::Tree;

/// Internal index for a node within a snapshot (sequential DFS order).
/// This is an array index, not a stable identity — it changes between snapshots.
/// Internal index type for node positions within a snapshot.
#[doc(hidden)]
pub type NodeIndex = u32;

/// The raw data for a single element in an accessibility tree snapshot.
///
/// This is the underlying data struct. Most consumers should use [`Node`],
/// which wraps `NodeData` with snapshot navigation (parent/children).
/// `NodeData` is used directly by provider implementors building trees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeData {
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

    /// Process ID of the application that owns this node.
    pub pid: Option<u32>,

    /// Platform-specific raw data
    pub raw: RawPlatformData,

    // ── Internal fields ──────────────────────────────────────────────────────
    // Present in serialized output for FFI consumers (Python, JS, LLMs),
    // but not part of the Rust public API.
    /// Sequential DFS index within the snapshot.
    /// Internal — present in serialized output for FFI consumers,
    /// but not intended as part of the primary Rust API.
    #[doc(hidden)]
    pub index: NodeIndex,

    /// Child node indices (direct children only).
    #[doc(hidden)]
    pub children_indices: Vec<NodeIndex>,

    /// Parent node index (None for root).
    #[doc(hidden)]
    pub parent_index: Option<NodeIndex>,
}

/// A read-only node in an accessibility tree snapshot, with navigation.
///
/// `Node` dereferences to [`NodeData`], so all properties (`role`, `name`,
/// `value`, `states`, etc.) are accessible via field access. Navigation
/// methods (`parent()`, `children()`) use the shared snapshot — no
/// platform refetch occurs.
///
/// Nodes are cheap to clone (they share the underlying snapshot via `Arc`).
/// To perform actions, use a [`Locator`](crate::Locator) instead.
#[derive(Clone)]
pub struct Node {
    snapshot: Arc<Tree>,
    index: u32,
}

impl Deref for Node {
    type Target = NodeData;

    fn deref(&self) -> &NodeData {
        self.snapshot
            .get_data(self.index)
            .expect("Node index must be valid within its snapshot")
    }
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Delegate to the underlying NodeData's Debug
        fmt::Debug::fmt(&**self, f)
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.snapshot, f)
    }
}

impl Serialize for Node {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        // Serialize as the underlying NodeData
        (**self).serialize(serializer)
    }
}

impl Node {
    /// Create a Node handle from a snapshot and an index into the snapshot.
    pub fn new(snapshot: Arc<Tree>, index: u32) -> Self {
        Self { snapshot, index }
    }

    /// Get the underlying snapshot (Tree) this node belongs to.
    ///
    /// Used by provider crates for action dispatch.
    pub fn tree(&self) -> &Arc<Tree> {
        &self.snapshot
    }

    /// Get the node's index within its snapshot.
    ///
    /// Used by provider crates for action dispatch.
    pub fn node_index(&self) -> u32 {
        self.index
    }

    /// Get the parent node, if any (root has no parent).
    ///
    /// Uses the snapshot — no platform refetch.
    pub fn parent(&self) -> Option<Node> {
        self.parent_index
            .map(|idx| Node::new(Arc::clone(&self.snapshot), idx))
    }

    /// Get direct children of this node.
    ///
    /// Uses the snapshot — no platform refetch.
    pub fn children(&self) -> Vec<Node> {
        self.children_indices
            .iter()
            .map(|&idx| Node::new(Arc::clone(&self.snapshot), idx))
            .collect()
    }

    /// Get the subtree rooted at this node (including this node).
    ///
    /// Uses the snapshot — no platform refetch.
    pub fn subtree(&self) -> Vec<Node> {
        self.snapshot
            .subtree_indices(self.index)
            .into_iter()
            .map(|idx| Node::new(Arc::clone(&self.snapshot), idx))
            .collect()
    }

}

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
