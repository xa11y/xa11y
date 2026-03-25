use serde::{Deserialize, Serialize};

use crate::action::Action;
use crate::role::Role;

/// Internal index for a node within a snapshot (sequential DFS order).
/// This is an array index, not a stable identity — it changes between snapshots.
/// Internal index type for node positions within a snapshot.
#[doc(hidden)]
pub type NodeIndex = u32;

/// A single element in the accessibility tree snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
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

    /// Platform-specific raw data (opt-in, for debugging)
    pub raw: Option<RawPlatformData>,

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

/// Bounding rectangle normalized to [0.0, 1.0] range relative to screen dimensions.
/// Uses corner-pair representation (distinct from `Rect`'s origin+size).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormalizedRect {
    /// 0.0 = left edge of screen
    pub left: f64,
    /// 0.0 = top edge of screen
    pub top: f64,
    /// 1.0 = right edge of screen
    pub right: f64,
    /// 1.0 = bottom edge of screen
    pub bottom: f64,
}

/// Platform-specific raw data, available when `include_raw: true` in QueryOptions.
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
