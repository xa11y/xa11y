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
    /// Use `Element.performAction(name)` to invoke a custom action, or the
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

    // ── Actions ────────────────────────────────────────────────────────

    /// Click / invoke this element via the accessibility action layer.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn press(&self) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::nullary(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::Press,
        ))
    }

    /// Move keyboard focus to this element.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn focus(&self) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::nullary(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::Focus,
        ))
    }

    /// Toggle a two- or three-state control.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn toggle(&self) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::nullary(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::Toggle,
        ))
    }

    /// Expand a disclosure, menu, or tree item.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn expand(&self) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::nullary(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::Expand,
        ))
    }

    /// Collapse a disclosure, menu, or tree item.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn collapse(&self) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::nullary(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::Collapse,
        ))
    }

    /// Select this element (list item, tab, row).
    #[napi(js_name = "select", ts_return_type = "Promise<void>")]
    pub fn select_(&self) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::nullary(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::Select,
        ))
    }

    /// Open this element's context menu.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn show_menu(&self) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::nullary(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::ShowMenu,
        ))
    }

    /// Scroll this element into the visible area.
    ///
    /// No-op on macOS — the macOS accessibility API has no equivalent. Uses
    /// `Component.ScrollTo` on Linux and `ScrollItemPattern` on Windows.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn scroll_into_view(&self) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::nullary(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::ScrollIntoView,
        ))
    }

    /// Increment a numeric value (slider, spin button) by its platform step.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn increment(&self) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::nullary(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::Increment,
        ))
    }

    /// Decrement a numeric value (slider, spin button) by its platform step.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn decrement(&self) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::nullary(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::Decrement,
        ))
    }

    /// Set the text value of this element. Replaces the entire value rather
    /// than inserting at the caret — use `typeText` for insertion.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn set_value(&self, value: String) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::with_text(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::SetValue,
            value,
        ))
    }

    /// Set the numeric value of this element (slider, spin button).
    #[napi(ts_return_type = "Promise<void>")]
    pub fn set_numeric_value(&self, value: f64) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::with_num(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::SetNumericValue,
            value,
        ))
    }

    /// Type `text` at the current caret position.
    ///
    /// Uses the platform accessibility API — never simulates keyboard events.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn type_text(&self, text: String) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::with_text(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::TypeText,
            text,
        ))
    }

    /// Select the text range from `start` to `end` (0-based character offsets).
    /// Rejects with `InvalidActionDataError` if `start > end`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn select_text(&self, start: u32, end: u32) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::with_range(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::SelectText,
            start,
            end,
        ))
    }

    /// Perform a custom action by its snake_case name.
    ///
    /// Use this for actions the element advertises in its `actions` list
    /// that don't have a dedicated method. Rejects with
    /// `ActionNotSupportedError` if the element does not advertise `action`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn perform_action(&self, action: String) -> AsyncTask<ElementActionTask> {
        AsyncTask::new(ElementActionTask::with_text(
            self.provider.clone(),
            self.data.clone(),
            ElementActionKind::PerformAction,
            action,
        ))
    }
}

// ── Task implementations ────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub enum ElementActionKind {
    Press,
    Focus,
    Toggle,
    Expand,
    Collapse,
    Select,
    ShowMenu,
    ScrollIntoView,
    Increment,
    Decrement,
    SetValue,
    SetNumericValue,
    TypeText,
    SelectText,
    PerformAction,
}

pub struct ElementActionTask {
    provider: Arc<dyn xa11y::Provider>,
    data: xa11y::ElementData,
    kind: ElementActionKind,
    text: Option<String>,
    num: Option<f64>,
    range: Option<(u32, u32)>,
}

impl ElementActionTask {
    fn nullary(
        provider: Arc<dyn xa11y::Provider>,
        data: xa11y::ElementData,
        kind: ElementActionKind,
    ) -> Self {
        Self {
            provider,
            data,
            kind,
            text: None,
            num: None,
            range: None,
        }
    }

    fn with_text(
        provider: Arc<dyn xa11y::Provider>,
        data: xa11y::ElementData,
        kind: ElementActionKind,
        text: String,
    ) -> Self {
        Self {
            provider,
            data,
            kind,
            text: Some(text),
            num: None,
            range: None,
        }
    }

    fn with_num(
        provider: Arc<dyn xa11y::Provider>,
        data: xa11y::ElementData,
        kind: ElementActionKind,
        num: f64,
    ) -> Self {
        Self {
            provider,
            data,
            kind,
            text: None,
            num: Some(num),
            range: None,
        }
    }

    fn with_range(
        provider: Arc<dyn xa11y::Provider>,
        data: xa11y::ElementData,
        kind: ElementActionKind,
        start: u32,
        end: u32,
    ) -> Self {
        Self {
            provider,
            data,
            kind,
            text: None,
            num: None,
            range: Some((start, end)),
        }
    }
}

impl Task for ElementActionTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let r = match self.kind {
            ElementActionKind::Press => self.provider.press(&self.data),
            ElementActionKind::Focus => self.provider.focus(&self.data),
            ElementActionKind::Toggle => self.provider.toggle(&self.data),
            ElementActionKind::Expand => self.provider.expand(&self.data),
            ElementActionKind::Collapse => self.provider.collapse(&self.data),
            ElementActionKind::Select => self.provider.select(&self.data),
            ElementActionKind::ShowMenu => self.provider.show_menu(&self.data),
            ElementActionKind::ScrollIntoView => self.provider.scroll_into_view(&self.data),
            ElementActionKind::Increment => self.provider.increment(&self.data),
            ElementActionKind::Decrement => self.provider.decrement(&self.data),
            ElementActionKind::SetValue => self
                .provider
                .set_value(&self.data, self.text.as_deref().unwrap_or("")),
            ElementActionKind::SetNumericValue => {
                let value = self.num.unwrap_or(0.0);
                if !value.is_finite() {
                    return Err(map_err(xa11y::Error::InvalidActionData {
                        message: format!("set_numeric_value requires a finite value, got {value}"),
                    }));
                }
                self.provider.set_numeric_value(&self.data, value)
            }
            ElementActionKind::TypeText => self
                .provider
                .type_text(&self.data, self.text.as_deref().unwrap_or("")),
            ElementActionKind::SelectText => {
                let (start, end) = self.range.unwrap_or((0, 0));
                if start > end {
                    return Err(map_err(xa11y::Error::InvalidActionData {
                        message: format!(
                            "select_text start ({start}) must be <= end ({end})"
                        ),
                    }));
                }
                self.provider.set_text_selection(&self.data, start, end)
            }
            ElementActionKind::PerformAction => self
                .provider
                .perform_action(&self.data, self.text.as_deref().unwrap_or("")),
        };
        r.map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(())
    }
}

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
