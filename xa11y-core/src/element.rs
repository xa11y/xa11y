use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::Error;
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

    /// Human-readable name (title, label).
    ///
    /// Stripped of Unicode bidi format controls (LRM, RLM, embeddings,
    /// overrides, isolates) so equality assertions match the logical text.
    /// The unstripped platform string is preserved in [`Self::raw`] under the
    /// platform-native key (e.g. `AXTitle` on macOS, `atspi_name` on Linux,
    /// `uia_name` on Windows). See [`crate::text::strip_bidi`].
    pub name: Option<String>,

    /// Current value (text content, slider position, etc.).
    ///
    /// Stripped of Unicode bidi format controls. The unstripped platform
    /// string is preserved in [`Self::raw`] (`AXValue` on macOS, `atspi_value`
    /// on Linux, `uia_value` on Windows). See [`crate::text::strip_bidi`].
    pub value: Option<String>,

    /// Supplementary description (tooltip, help text).
    ///
    /// Stripped of Unicode bidi format controls. The unstripped platform
    /// string is preserved in [`Self::raw`] (`AXDescription`/`AXHelp` on
    /// macOS, `atspi_description` on Linux, `uia_help_text` on Windows).
    /// See [`crate::text::strip_bidi`].
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

    // ── Actions ─────────────────────────────────────────────────────
    //
    // Element actions invoke the platform via the captured provider handle —
    // they do **not** re-resolve the selector. If the underlying element has
    // been destroyed since this snapshot was taken, the provider returns a
    // platform-specific "gone" error. For resilient retry-on-change semantics,
    // use the equivalent method on [`crate::Locator`] instead.

    /// Click / invoke this element via the accessibility action layer.
    pub fn press(&self) -> crate::error::Result<()> {
        self.provider.press(&self.data)
    }

    /// Set keyboard focus to this element.
    pub fn focus(&self) -> crate::error::Result<()> {
        self.provider.focus(&self.data)
    }

    /// Remove keyboard focus from this element.
    pub fn blur(&self) -> crate::error::Result<()> {
        self.provider.blur(&self.data)
    }

    /// Toggle a two- or three-state control (checkbox, switch).
    pub fn toggle(&self) -> crate::error::Result<()> {
        self.provider.toggle(&self.data)
    }

    /// Select this element (list item, tab, row).
    pub fn select(&self) -> crate::error::Result<()> {
        self.provider.select(&self.data)
    }

    /// Expand a disclosure, menu, combo box, or tree item.
    pub fn expand(&self) -> crate::error::Result<()> {
        self.provider.expand(&self.data)
    }

    /// Collapse an expanded element.
    pub fn collapse(&self) -> crate::error::Result<()> {
        self.provider.collapse(&self.data)
    }

    /// Open this element's context menu or dropdown.
    pub fn show_menu(&self) -> crate::error::Result<()> {
        self.provider.show_menu(&self.data)
    }

    /// Increment a numeric control (slider, spinner) by its platform step.
    pub fn increment(&self) -> crate::error::Result<()> {
        self.provider.increment(&self.data)
    }

    /// Decrement a numeric control (slider, spinner) by its platform step.
    pub fn decrement(&self) -> crate::error::Result<()> {
        self.provider.decrement(&self.data)
    }

    /// Scroll this element into the visible area.
    ///
    /// No-op on macOS — the macOS accessibility API has no equivalent.
    pub fn scroll_into_view(&self) -> crate::error::Result<()> {
        self.provider.scroll_into_view(&self.data)
    }

    /// Set the text value of this element. Replaces the entire value rather
    /// than inserting at the caret — use [`Element::type_text`] for insertion.
    pub fn set_value(&self, value: &str) -> crate::error::Result<()> {
        self.provider.set_value(&self.data, value)
    }

    /// Set the numeric value of this element (slider, spinner).
    ///
    /// Returns [`Error::InvalidActionData`] if `value` is NaN or infinite.
    pub fn set_numeric_value(&self, value: f64) -> crate::error::Result<()> {
        if !value.is_finite() {
            return Err(Error::InvalidActionData {
                message: format!("set_numeric_value requires a finite value, got {}", value),
            });
        }
        self.provider.set_numeric_value(&self.data, value)
    }

    /// Insert text at the current cursor position.
    ///
    /// Uses the platform accessibility API — never simulates keyboard events.
    pub fn type_text(&self, text: &str) -> crate::error::Result<()> {
        self.provider.type_text(&self.data, text)
    }

    /// Select the text range from `start` to `end` (0-based character offsets).
    ///
    /// Returns [`Error::InvalidActionData`] if `start > end`.
    pub fn select_text(&self, start: u32, end: u32) -> crate::error::Result<()> {
        if start > end {
            return Err(Error::InvalidActionData {
                message: format!("select_text start ({}) must be <= end ({})", start, end),
            });
        }
        self.provider.set_text_selection(&self.data, start, end)
    }

    /// Perform an action by its `snake_case` name.
    ///
    /// Use this for actions the element advertises in its [`actions`](ElementData::actions)
    /// list that don't have a dedicated method. Well-known names (`"press"`,
    /// `"focus"`, etc.) also work — providers delegate to the named methods.
    pub fn perform_action(&self, action: &str) -> crate::error::Result<()> {
        self.provider.perform_action(&self.data, action)
    }
}

fn build_tree_node(
    element: &Element,
    max_depth: Option<usize>,
    depth: usize,
) -> crate::error::Result<TreeNode> {
    let children = if max_depth.is_none_or(|d| depth < d) {
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

#[cfg(test)]
mod tests {
    //! Unit tests for `Element` action methods. Verifies each action records the
    //! expected entry in the mock provider's action log and that validation
    //! errors fire before the provider is ever called.

    use super::*;
    use crate::mock::{build_provider, MockProvider};
    use crate::selector::Selector;

    /// Resolve `selector` against the mock tree and return the first match
    /// wrapped in an `Element`. Panics on no match — these are unit tests, not
    /// production paths.
    fn find_element(provider: &Arc<MockProvider>, selector: &str) -> Element {
        let parsed = Selector::parse(selector).expect("selector must parse");
        let provider_dyn: Arc<dyn Provider> = provider.clone();
        let root = provider_dyn
            .list_apps()
            .expect("list_apps must succeed")
            .into_iter()
            .next()
            .expect("mock provider must expose an application root");
        let mut matches = provider_dyn
            .find_elements(&root, &parsed, Some(1), None)
            .expect("find_elements must succeed");
        let data = matches.pop().expect("selector matched no elements");
        Element::new(data, provider_dyn)
    }

    fn last_action(provider: &Arc<MockProvider>) -> (u64, String, Option<String>) {
        provider
            .actions()
            .last()
            .cloned()
            .expect("expected at least one recorded action")
    }

    #[test]
    fn nullary_actions_record_correct_name() {
        let provider = build_provider();
        let cases = [
            (r#"button[name="Back"]"#, "press" as &str),
            (r#"button[name="Back"]"#, "focus"),
            (r#"button[name="Back"]"#, "blur"),
            (r#"check_box[name="Agree"]"#, "toggle"),
            (r#"list_item[name="Item 1"]"#, "select"),
            (r#"list[name="Items"]"#, "expand"),
            (r#"list[name="Items"]"#, "collapse"),
            (r#"button[name="Back"]"#, "show_menu"),
            (r#"slider[name="Volume"]"#, "increment"),
            (r#"slider[name="Volume"]"#, "decrement"),
            (r#"button[name="Back"]"#, "scroll_into_view"),
        ];
        for (selector, action) in cases {
            provider.clear_actions();
            let el = find_element(&provider, selector);
            match action {
                "press" => el.press().unwrap(),
                "focus" => el.focus().unwrap(),
                "blur" => el.blur().unwrap(),
                "toggle" => el.toggle().unwrap(),
                "select" => el.select().unwrap(),
                "expand" => el.expand().unwrap(),
                "collapse" => el.collapse().unwrap(),
                "show_menu" => el.show_menu().unwrap(),
                "increment" => el.increment().unwrap(),
                "decrement" => el.decrement().unwrap(),
                "scroll_into_view" => el.scroll_into_view().unwrap(),
                _ => unreachable!(),
            }
            let (handle, name, data) = last_action(&provider);
            assert_eq!(
                name, action,
                "wrong action recorded for selector {selector}"
            );
            assert_eq!(data, None, "nullary action should not carry data");
            assert_eq!(handle, el.data.handle);
        }
    }

    #[test]
    fn set_value_records_text_payload() {
        let provider = build_provider();
        let el = find_element(&provider, r#"text_field[name="Search"]"#);
        el.set_value("world").unwrap();
        let (handle, name, data) = last_action(&provider);
        assert_eq!(handle, el.data.handle);
        assert_eq!(name, "set_value");
        assert_eq!(data.as_deref(), Some("world"));
    }

    #[test]
    fn set_numeric_value_records_payload() {
        let provider = build_provider();
        let el = find_element(&provider, r#"slider[name="Volume"]"#);
        el.set_numeric_value(42.0).unwrap();
        let (_, name, data) = last_action(&provider);
        assert_eq!(name, "set_numeric_value");
        assert_eq!(data.as_deref(), Some("42"));
    }

    #[test]
    fn set_numeric_value_rejects_non_finite() {
        let provider = build_provider();
        let el = find_element(&provider, r#"slider[name="Volume"]"#);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(
                el.set_numeric_value(bad),
                Err(Error::InvalidActionData { .. })
            ));
        }
        // None of the validation failures should have reached the provider.
        assert!(provider.actions().is_empty());
    }

    #[test]
    fn type_text_records_payload() {
        let provider = build_provider();
        let el = find_element(&provider, r#"text_field[name="Search"]"#);
        el.type_text("abc").unwrap();
        let (_, name, data) = last_action(&provider);
        assert_eq!(name, "type_text");
        assert_eq!(data.as_deref(), Some("abc"));
    }

    #[test]
    fn select_text_records_range() {
        let provider = build_provider();
        let el = find_element(&provider, r#"text_field[name="Search"]"#);
        el.select_text(1, 4).unwrap();
        let (_, name, data) = last_action(&provider);
        assert_eq!(name, "set_text_selection");
        assert_eq!(data.as_deref(), Some("1..4"));
    }

    #[test]
    fn select_text_rejects_inverted_range() {
        let provider = build_provider();
        let el = find_element(&provider, r#"text_field[name="Search"]"#);
        assert!(matches!(
            el.select_text(5, 2),
            Err(Error::InvalidActionData { .. })
        ));
        assert!(provider.actions().is_empty());
    }

    #[test]
    fn perform_action_records_arbitrary_name() {
        let provider = build_provider();
        let el = find_element(&provider, r#"button[name="Back"]"#);
        el.perform_action("raise").unwrap();
        let (_, name, _) = last_action(&provider);
        assert_eq!(name, "raise");
    }

    #[test]
    fn locator_actions_desugar_to_element_actions() {
        // Locator's auto-wait wraps the resolved data in an Element and calls
        // its action — no duplication at the provider call site. This test
        // pins that behavior: pressing via the Locator should record exactly
        // the same entry as pressing via the Element it resolves to.
        let provider = build_provider();
        let provider_dyn: Arc<dyn Provider> = provider.clone();
        let locator = crate::locator::Locator::new(provider_dyn, None, r#"button[name="Back"]"#);
        locator.press().unwrap();
        let (_, name, data) = last_action(&provider);
        assert_eq!(name, "press");
        assert_eq!(data, None);
    }

    #[test]
    fn locator_validation_runs_before_auto_wait() {
        // Locator validates payloads before entering its 5s auto-wait poll.
        // We verify by passing invalid input against a never-matching selector:
        // if validation fired first we get InvalidActionData immediately, not
        // a Timeout 5 seconds later.
        let provider = build_provider();
        let provider_dyn: Arc<dyn Provider> = provider.clone();
        let locator =
            crate::locator::Locator::new(provider_dyn, None, r#"button[name="never-matches"]"#);
        let started = std::time::Instant::now();
        let err = locator.set_numeric_value(f64::NAN).unwrap_err();
        assert!(matches!(err, Error::InvalidActionData { .. }));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "validation must short-circuit auto-wait",
        );
    }
}
