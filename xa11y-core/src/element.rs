use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::provider::Provider;
use crate::role::Role;

/// The raw data for a single element in an accessibility tree.
///
/// This is the underlying data struct. Most consumers should use [`Element`],
/// which wraps `ElementData` with a provider reference for lazy navigation.
/// `ElementData` is used directly by provider implementors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementData {
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

    /// Available actions reported by the platform.
    ///
    /// Names are `snake_case` strings — well-known actions use their standard
    /// names (`"press"`, `"toggle"`, `"expand"`, etc.) and platform-specific
    /// actions use their converted names (e.g. macOS `AXCustomThing` →
    /// `"custom_thing"`).
    pub actions: Vec<String>,

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

    /// Process ID of the application that owns this element.
    pub pid: Option<u32>,

    /// Platform-specific raw data
    pub raw: RawPlatformData,

    /// Opaque handle for the provider to look up the platform object.
    /// Not serialized — only valid within the provider that created it.
    #[serde(skip, default)]
    pub handle: u64,
}

/// A live element with lazy navigation via a provider reference.
///
/// `Element` dereferences to [`ElementData`], so all properties (`role`, `name`,
/// `value`, `states`, etc.) are accessible via field access. Navigation
/// methods (`parent()`, `children()`) call the provider on demand.
///
/// Elements are cheap to clone (they share the provider via `Arc`).
#[derive(Clone)]
pub struct Element {
    data: ElementData,
    provider: Arc<dyn Provider>,
}

impl Deref for Element {
    type Target = ElementData;

    fn deref(&self) -> &ElementData {
        &self.data
    }
}

impl fmt::Debug for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.data, f)
    }
}

impl fmt::Display for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name_part = self
            .data
            .name
            .as_ref()
            .map(|n| format!(" \"{}\"", n))
            .unwrap_or_default();
        let value_part = self
            .data
            .value
            .as_ref()
            .map(|v| format!(" value=\"{}\"", v))
            .unwrap_or_default();
        write!(
            f,
            "{}{}{}",
            self.data.role.to_snake_case(),
            name_part,
            value_part,
        )
    }
}

impl Serialize for Element {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.data.serialize(serializer)
    }
}

impl Element {
    /// Create an Element from raw data and a provider reference.
    pub fn new(data: ElementData, provider: Arc<dyn Provider>) -> Self {
        Self { data, provider }
    }

    /// Get the underlying ElementData.
    pub fn data(&self) -> &ElementData {
        &self.data
    }

    /// Get the provider reference.
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    /// Get direct children of this element.
    ///
    /// Each call queries the provider — results are not cached.
    pub fn children(&self) -> crate::error::Result<Vec<Element>> {
        let children = self.provider.get_children(Some(&self.data))?;
        Ok(children
            .into_iter()
            .map(|d| Element::new(d, Arc::clone(&self.provider)))
            .collect())
    }

    /// Get the parent element, if any (root-level elements have no parent).
    ///
    /// Each call queries the provider — results are not cached.
    pub fn parent(&self) -> crate::error::Result<Option<Element>> {
        let parent = self.provider.get_parent(&self.data)?;
        Ok(parent.map(|d| Element::new(d, Arc::clone(&self.provider))))
    }

    /// Get the process ID from the element data.
    pub fn pid(&self) -> Option<u32> {
        self.data.pid
    }

    /// Capture the subtree rooted at this element as a recursive snapshot.
    ///
    /// `max_depth` limits traversal depth: `0` = only this node (no children),
    /// `1` = node + direct children, and so on. `None` traverses the full subtree.
    pub fn tree(&self, max_depth: Option<usize>) -> crate::error::Result<TreeNode> {
        build_tree_node(self, max_depth, 0)
    }

    /// Render the subtree rooted at this element as an indented string.
    ///
    /// Each line is `{indent}{role} "{name}" [value="{value}"]`. Returns the
    /// string without printing it. Same depth semantics as [`Element::tree`].
    pub fn dump(&self, max_depth: Option<usize>) -> crate::error::Result<String> {
        let node = self.tree(max_depth)?;
        let mut out = String::new();
        write_tree_node(&node, 0, &mut out);
        Ok(out)
    }
}

fn build_tree_node(
    element: &Element,
    max_depth: Option<usize>,
    depth: usize,
) -> crate::error::Result<TreeNode> {
    let children = if max_depth.map_or(true, |d| depth < d) {
        element
            .children()?
            .into_iter()
            .map(|child| build_tree_node(&child, max_depth, depth + 1))
            .collect::<crate::error::Result<Vec<_>>>()?
    } else {
        vec![]
    };
    Ok(TreeNode {
        role: element.data.role.to_snake_case().to_string(),
        name: element.data.name.clone(),
        value: element.data.value.clone(),
        children,
    })
}

fn write_tree_node(node: &TreeNode, depth: usize, out: &mut String) {
    use fmt::Write as _;
    let indent = "  ".repeat(depth);
    write!(out, "{}{}", indent, node.role).unwrap();
    if let Some(ref n) = node.name {
        write!(out, " \"{}\"", n).unwrap();
    }
    if let Some(ref v) = node.value {
        write!(out, " value=\"{}\"", v).unwrap();
    }
    out.push('\n');
    for child in &node.children {
        write_tree_node(child, depth + 1, out);
    }
}

/// Boolean state flags for an element.
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

/// Platform-specific raw data attached to every element.
///
/// An untyped key-value map containing the original platform-specific data
/// exactly as the platform reported it. Keys use `snake_case` naming. This is
/// the escape hatch for consumers who need full platform fidelity.
pub type RawPlatformData = HashMap<String, serde_json::Value>;

/// A node in a recursive snapshot of the accessibility subtree.
///
/// Returned by [`Element::tree`] and [`Locator::tree`]. Each node carries the
/// role, display name, and value of one element, plus its children recursively.
/// `children` is empty when `max_depth` was reached or the element is a leaf.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNode {
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub children: Vec<TreeNode>,
}
