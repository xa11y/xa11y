use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::action::Action;
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

    /// Available well-known actions.
    pub actions: Vec<Action>,

    /// Platform-specific actions not covered by [`Action`] variants.
    ///
    /// Names are in `snake_case` — providers convert to/from platform naming
    /// conventions (e.g. macOS `AXCustomThing` ↔ `"custom_thing"`).
    #[serde(default)]
    pub custom_actions: Vec<String>,

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

    /// Full set of element attributes — both normalized properties and
    /// platform-specific ones — keyed by `snake_case` names. Named properties
    /// (name, value, enabled, etc.) also appear here.
    #[serde(default)]
    pub attributes: HashMap<String, serde_json::Value>,

    /// Platform-specific raw data
    pub raw: RawPlatformData,

    /// Opaque handle for the provider to look up the platform object.
    /// Not serialized — only valid within the provider that created it.
    #[serde(skip, default)]
    pub handle: u64,
}

impl ElementData {
    /// Populate the `attributes` map from the struct's named properties.
    /// Providers should call this after constructing `ElementData` to ensure
    /// normalized attributes are present in the map.
    pub fn populate_attributes(&mut self) {
        use serde_json::Value;
        let a = &mut self.attributes;

        a.insert(
            "role".into(),
            Value::String(self.role.to_snake_case().to_string()),
        );
        if let Some(ref n) = self.name {
            a.insert("name".into(), Value::String(n.clone()));
        }
        if let Some(ref v) = self.value {
            a.insert("value".into(), Value::String(v.clone()));
        }
        if let Some(ref d) = self.description {
            a.insert("description".into(), Value::String(d.clone()));
        }
        if let Some(ref b) = self.bounds {
            a.insert(
                "bounds".into(),
                serde_json::json!({
                    "x": b.x, "y": b.y, "width": b.width, "height": b.height
                }),
            );
        }
        if let Some(nv) = self.numeric_value {
            if let Some(n) = serde_json::Number::from_f64(nv) {
                a.insert("numeric_value".into(), Value::Number(n));
            }
        }
        if let Some(nv) = self.min_value {
            if let Some(n) = serde_json::Number::from_f64(nv) {
                a.insert("min_value".into(), Value::Number(n));
            }
        }
        if let Some(nv) = self.max_value {
            if let Some(n) = serde_json::Number::from_f64(nv) {
                a.insert("max_value".into(), Value::Number(n));
            }
        }
        if let Some(ref sid) = self.stable_id {
            a.insert("stable_id".into(), Value::String(sid.clone()));
        }
        a.insert("enabled".into(), Value::Bool(self.states.enabled));
        a.insert("visible".into(), Value::Bool(self.states.visible));
        a.insert("focused".into(), Value::Bool(self.states.focused));
        a.insert("focusable".into(), Value::Bool(self.states.focusable));
        a.insert("selected".into(), Value::Bool(self.states.selected));
        a.insert("editable".into(), Value::Bool(self.states.editable));
        a.insert("modal".into(), Value::Bool(self.states.modal));
        a.insert("required".into(), Value::Bool(self.states.required));
        a.insert("busy".into(), Value::Bool(self.states.busy));
        if let Some(exp) = self.states.expanded {
            a.insert("expanded".into(), Value::Bool(exp));
        }
        if let Some(ref chk) = self.states.checked {
            let s = match chk {
                Toggled::On => "on",
                Toggled::Off => "off",
                Toggled::Mixed => "mixed",
            };
            a.insert("checked".into(), Value::String(s.into()));
        }
    }
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

    /// Perform a well-known action on this element.
    ///
    /// This is the direct, no-auto-wait version. For auto-waiting behavior,
    /// use the corresponding [`Locator`](crate::Locator) methods instead.
    pub fn perform_action(
        &self,
        action: crate::Action,
        data: Option<crate::ActionData>,
    ) -> crate::error::Result<()> {
        self.provider.perform_action(&self.data, action, data)
    }

    /// Perform a custom (platform-specific) action by `snake_case` name.
    ///
    /// Custom actions are platform-specific operations not covered by the
    /// [`Action`](crate::Action) enum. Available custom actions are listed
    /// in [`ElementData::custom_actions`].
    ///
    /// The provider converts the name to the platform's convention (e.g.
    /// `"custom_thing"` → `"AXCustomThing"` on macOS) and invokes it.
    pub fn perform_custom_action(&self, name: &str) -> crate::error::Result<()> {
        self.provider.perform_custom_action(&self.data, name)
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
