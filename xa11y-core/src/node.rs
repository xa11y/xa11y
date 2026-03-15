use serde::{Deserialize, Serialize};

use crate::action::Action;
use crate::role::Role;

/// Unique identifier for a node within a snapshot (sequential, deterministic DFS order).
pub type NodeId = u32;

/// Screen-pixel bounding rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Bounding rectangle normalized to [0.0, 1.0] range relative to screen dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormalizedRect {
    /// Left edge
    pub x1: f64,
    /// Top edge
    pub y1: f64,
    /// Right edge
    pub x2: f64,
    /// Bottom edge
    pub y2: f64,
}

/// Tri-state toggle value for checkboxes and similar elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Toggled {
    Off,
    On,
    /// Indeterminate / tri-state
    Mixed,
}

/// Boolean state flags for an accessibility element.
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
            required: false,
            busy: false,
        }
    }
}

/// Platform-specific raw data, for debugging and escape-hatch access.
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
}

/// A single element in the accessibility tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Unique ID within a snapshot (sequential, deterministic DFS order)
    pub id: NodeId,

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

    /// Bounding box normalized to [0.0, 1.0] relative to screen dimensions
    pub bounds_normalized: Option<NormalizedRect>,

    /// Available actions
    pub actions: Vec<Action>,

    /// Current state flags
    pub states: StateSet,

    /// Child node IDs (direct children only)
    pub children: Vec<NodeId>,

    /// Parent node ID (None for root)
    pub parent: Option<NodeId>,

    /// Depth in the tree (0 = root)
    pub depth: u32,

    /// Application name (useful when querying all apps)
    pub app_name: Option<String>,

    /// Platform-specific raw data (opt-in, for debugging)
    pub raw: Option<RawPlatformData>,
}
