//! Small plain-data types exposed to JS: `Rect`, `TreeNode`, `EventKind`, etc.

/// A node in a recursive snapshot of the accessibility subtree.
///
/// Returned by `Element.tree()` and `Locator.tree()`. Each node carries the
/// role, display name, and value of one element, plus its children recursively.
/// `children` is empty when `maxDepth` was reached or the element is a leaf.
#[napi(object)]
#[derive(Clone)]
pub struct TreeNode {
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub children: Vec<TreeNode>,
}

impl From<xa11y::TreeNode> for TreeNode {
    fn from(n: xa11y::TreeNode) -> Self {
        Self {
            role: n.role,
            name: n.name,
            value: n.value,
            children: n.children.into_iter().map(Into::into).collect(),
        }
    }
}

/// A bounding rectangle in screen coordinates.
///
/// Coordinates use the platform's native coordinate space: points on macOS,
/// physical pixels on Windows and Linux. Origin is the top-left of the
/// primary display; negative `x` / `y` are valid on multi-monitor setups.
#[napi(object)]
#[derive(Clone)]
pub struct Rect {
    /// Left edge, in screen coordinates.
    pub x: i32,
    /// Top edge, in screen coordinates.
    pub y: i32,
    /// Width in screen-coordinate units.
    pub width: i32,
    /// Height in screen-coordinate units.
    pub height: i32,
}

impl From<xa11y::Rect> for Rect {
    fn from(r: xa11y::Rect) -> Self {
        Self {
            x: r.x,
            y: r.y,
            width: r.width as i32,
            height: r.height as i32,
        }
    }
}

/// Convert an `xa11y::EventKind` into a camelCase string used across the JS API.
pub fn event_kind_to_str(kind: &xa11y::EventKind) -> &'static str {
    match kind {
        xa11y::EventKind::FocusChanged => "focusChanged",
        xa11y::EventKind::ValueChanged => "valueChanged",
        xa11y::EventKind::NameChanged => "nameChanged",
        xa11y::EventKind::StateChanged { .. } => "stateChanged",
        xa11y::EventKind::StructureChanged => "structureChanged",
        xa11y::EventKind::WindowOpened => "windowOpened",
        xa11y::EventKind::WindowClosed => "windowClosed",
        xa11y::EventKind::WindowActivated => "windowActivated",
        xa11y::EventKind::WindowDeactivated => "windowDeactivated",
        xa11y::EventKind::SelectionChanged => "selectionChanged",
        xa11y::EventKind::MenuOpened => "menuOpened",
        xa11y::EventKind::MenuClosed => "menuClosed",
        xa11y::EventKind::TextChanged => "textChanged",
        xa11y::EventKind::Announcement => "announcement",
    }
}

/// Convert an `xa11y::StateFlag` to a camelCase string.
pub fn state_flag_to_str(flag: xa11y::StateFlag) -> &'static str {
    match flag {
        xa11y::StateFlag::Enabled => "enabled",
        xa11y::StateFlag::Visible => "visible",
        xa11y::StateFlag::Focused => "focused",
        xa11y::StateFlag::Checked => "checked",
        xa11y::StateFlag::Selected => "selected",
        xa11y::StateFlag::Expanded => "expanded",
        xa11y::StateFlag::Editable => "editable",
        xa11y::StateFlag::Focusable => "focusable",
        xa11y::StateFlag::Modal => "modal",
        xa11y::StateFlag::Required => "required",
        xa11y::StateFlag::Busy => "busy",
    }
}

/// Convert an `xa11y::Toggled` into a lower-case string (`"on"`/`"off"`/`"mixed"`).
pub fn toggled_to_str(t: xa11y::Toggled) -> &'static str {
    match t {
        xa11y::Toggled::Off => "off",
        xa11y::Toggled::On => "on",
        xa11y::Toggled::Mixed => "mixed",
    }
}
