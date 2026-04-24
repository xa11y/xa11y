//! JS `Element` class: a live element handle with lazy navigation.

use std::sync::Arc;

use napi::bindgen_prelude::{AsyncTask, Env, Task};

use crate::map_err;
use crate::subscription::NativeSubscription;
use crate::types::{toggled_to_str, Rect};

/// A snapshot of a node in the accessibility tree.
///
/// Property getters (`role`, `name`, `value`, state flags, etc.) are
/// synchronous — they read the snapshot data captured when the element
/// was fetched. Navigation methods (`children()`, `parent()`) are async
/// and re-query the provider on every call, so you always see the latest
/// tree state.
///
/// Elements are cheap to pass around; they share the provider handle
/// internally.
#[napi]
pub struct Element {
    pub(crate) data: xa11y::ElementData,
    pub(crate) provider: Arc<dyn xa11y::Provider>,
}

impl Element {
    pub(crate) fn new(data: xa11y::ElementData, provider: Arc<dyn xa11y::Provider>) -> Self {
        Self { data, provider }
    }
}

#[napi]
impl Element {
    // ── Synchronous property getters ────────────────────────────────────

    /// The element's role, as a snake_case string (e.g. `"button"`, `"check_box"`).
    #[napi(getter)]
    pub fn role(&self) -> String {
        self.data.role.to_snake_case().to_string()
    }

    /// Human-readable name (title, label, or ARIA name).
    #[napi(getter)]
    pub fn name(&self) -> Option<String> {
        self.data.name.clone()
    }

    /// Current value — text content for editable fields, stringified slider
    /// position, etc. For numeric controls, prefer `numericValue`.
    #[napi(getter)]
    pub fn value(&self) -> Option<String> {
        self.data.value.clone()
    }

    /// Supplementary description (tooltip text, ARIA description).
    #[napi(getter)]
    pub fn description(&self) -> Option<String> {
        self.data.description.clone()
    }

    /// Numeric value for sliders, spin buttons, and progress indicators.
    #[napi(getter)]
    pub fn numeric_value(&self) -> Option<f64> {
        self.data.numeric_value
    }

    /// Minimum numeric value for bounded controls (slider, spin button).
    #[napi(getter)]
    pub fn min_value(&self) -> Option<f64> {
        self.data.min_value
    }

    /// Maximum numeric value for bounded controls (slider, spin button).
    #[napi(getter)]
    pub fn max_value(&self) -> Option<f64> {
        self.data.max_value
    }

    /// Platform-assigned identifier that is stable across queries for the
    /// same element. Not available on every platform / every widget.
    #[napi(getter)]
    pub fn stable_id(&self) -> Option<String> {
        self.data.stable_id.clone()
    }

    /// Process ID of the owning application.
    #[napi(getter)]
    pub fn pid(&self) -> Option<u32> {
        self.data.pid
    }

    /// Names of actions the element advertises (e.g. `["press", "focus"]`).
    /// Use `Locator.performAction(name)` to invoke a custom action, or the
    /// named convenience methods (`press`, `toggle`, etc.) for the common
    /// ones.
    #[napi(getter)]
    pub fn actions(&self) -> Vec<String> {
        self.data.actions.clone()
    }

    /// Screen-coordinate bounding rectangle, or `null` for virtual /
    /// off-screen elements that do not have a physical position.
    #[napi(getter)]
    pub fn bounds(&self) -> Option<Rect> {
        self.data.bounds.map(Into::into)
    }

    /// Platform-specific raw data attached to this element, as a plain JS
    /// object. Keys are provider-defined (e.g. `ax_role`/`ax_subrole` on macOS,
    /// `uia_control_type` on Windows). Values are JSON-compatible — strings,
    /// numbers, booleans, arrays, nested objects. Intended for debugging and
    /// platform-specific queries.
    #[napi(getter, ts_return_type = "Record<string, unknown>")]
    pub fn raw(&self) -> serde_json::Value {
        // Build a JSON Object from the raw HashMap. napi's serde-json
        // integration converts this to a plain JS object when returned.
        let map: serde_json::Map<String, serde_json::Value> = self
            .data
            .raw
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        serde_json::Value::Object(map)
    }

    /// `true` if the element is interactive (not greyed out or disabled).
    #[napi(getter)]
    pub fn enabled(&self) -> bool {
        self.data.states.enabled
    }

    /// `true` if the element is currently rendered on screen (not hidden,
    /// not clipped off the viewport).
    #[napi(getter)]
    pub fn visible(&self) -> bool {
        self.data.states.visible
    }

    /// `true` if the element currently has keyboard focus.
    #[napi(getter)]
    pub fn focused(&self) -> bool {
        self.data.states.focused
    }

    /// Tri-state checked value for checkboxes, toggle buttons, and menu items:
    /// `"on"`, `"off"`, `"mixed"`, or `null` if the element is not toggleable.
    #[napi(getter)]
    pub fn checked(&self) -> Option<String> {
        self.data
            .states
            .checked
            .map(|t| toggled_to_str(t).to_string())
    }

    /// `true` if the element is selected (list item, tab, row).
    #[napi(getter)]
    pub fn selected(&self) -> bool {
        self.data.states.selected
    }

    /// `true` / `false` for expandable elements (disclosures, menus, tree
    /// items); `null` if the element is not expandable.
    #[napi(getter)]
    pub fn expanded(&self) -> Option<bool> {
        self.data.states.expanded
    }

    /// `true` if the element accepts text editing (text field, text area,
    /// rich-text region).
    #[napi(getter)]
    pub fn editable(&self) -> bool {
        self.data.states.editable
    }

    /// `true` if the element can receive keyboard focus (distinct from
    /// `focused`, which reports the current state).
    #[napi(getter)]
    pub fn focusable(&self) -> bool {
        self.data.states.focusable
    }

    /// `true` if the element is a modal dialog that blocks interaction with
    /// the rest of the app.
    #[napi(getter)]
    pub fn modal(&self) -> bool {
        self.data.states.modal
    }

    /// `true` for form fields that are marked required.
    #[napi(getter)]
    pub fn required(&self) -> bool {
        self.data.states.required
    }

    /// `true` if the element is loading or otherwise indicating a busy
    /// state (progress indicator, spinner region).
    #[napi(getter)]
    pub fn busy(&self) -> bool {
        self.data.states.busy
    }

    // ── Async navigation ────────────────────────────────────────────────

    /// Get direct children (lazy — each call re-queries the provider).
    #[napi(ts_return_type = "Promise<Element[]>")]
    pub fn children(&self) -> AsyncTask<ChildrenTask> {
        AsyncTask::new(ChildrenTask {
            data: self.data.clone(),
            provider: self.provider.clone(),
        })
    }

    /// Get the parent element, or `null` if this is the root.
    #[napi(ts_return_type = "Promise<Element | null>")]
    pub fn parent(&self) -> AsyncTask<ParentTask> {
        AsyncTask::new(ParentTask {
            data: self.data.clone(),
            provider: self.provider.clone(),
        })
    }

    /// Subscribe to accessibility events for this element (typically an app).
    #[napi(ts_return_type = "Promise<_NativeSubscription>")]
    pub fn subscribe(&self) -> AsyncTask<SubscribeTask> {
        AsyncTask::new(SubscribeTask {
            data: self.data.clone(),
            provider: self.provider.clone(),
        })
    }
}

// ── Task implementations ────────────────────────────────────────────────

pub struct ChildrenTask {
    data: xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
}

impl Task for ChildrenTask {
    type Output = Vec<xa11y::ElementData>;
    type JsValue = Vec<Element>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.provider
            .get_children(Some(&self.data))
            .map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output
            .into_iter()
            .map(|d| Element::new(d, self.provider.clone()))
            .collect())
    }
}

pub struct ParentTask {
    data: xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
}

impl Task for ParentTask {
    type Output = Option<xa11y::ElementData>;
    type JsValue = Option<Element>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.provider.get_parent(&self.data).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output.map(|d| Element::new(d, self.provider.clone())))
    }
}

pub struct SubscribeTask {
    data: xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
}

impl Task for SubscribeTask {
    type Output = xa11y::Subscription;
    type JsValue = NativeSubscription;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.provider.subscribe(&self.data).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(NativeSubscription::new(output, self.provider.clone()))
    }
}
