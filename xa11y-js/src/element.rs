//! JS `Element` class: a live element handle with lazy navigation.

use std::sync::Arc;

use napi::bindgen_prelude::{AsyncTask, Env, Task};

use crate::map_err;
use crate::subscription::Subscription;
use crate::types::{toggled_to_str, Rect};

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

    #[napi(getter)]
    pub fn name(&self) -> Option<String> {
        self.data.name.clone()
    }

    #[napi(getter)]
    pub fn value(&self) -> Option<String> {
        self.data.value.clone()
    }

    #[napi(getter)]
    pub fn description(&self) -> Option<String> {
        self.data.description.clone()
    }

    #[napi(getter)]
    pub fn numeric_value(&self) -> Option<f64> {
        self.data.numeric_value
    }

    #[napi(getter)]
    pub fn min_value(&self) -> Option<f64> {
        self.data.min_value
    }

    #[napi(getter)]
    pub fn max_value(&self) -> Option<f64> {
        self.data.max_value
    }

    #[napi(getter)]
    pub fn stable_id(&self) -> Option<String> {
        self.data.stable_id.clone()
    }

    #[napi(getter)]
    pub fn pid(&self) -> Option<u32> {
        self.data.pid
    }

    #[napi(getter)]
    pub fn actions(&self) -> Vec<String> {
        self.data.actions.clone()
    }

    #[napi(getter)]
    pub fn bounds(&self) -> Option<Rect> {
        self.data.bounds.map(Into::into)
    }

    #[napi(getter)]
    pub fn enabled(&self) -> bool {
        self.data.states.enabled
    }

    #[napi(getter)]
    pub fn visible(&self) -> bool {
        self.data.states.visible
    }

    #[napi(getter)]
    pub fn focused(&self) -> bool {
        self.data.states.focused
    }

    /// `"on"`, `"off"`, `"mixed"`, or `null` if the element is not toggleable.
    #[napi(getter)]
    pub fn checked(&self) -> Option<String> {
        self.data
            .states
            .checked
            .map(|t| toggled_to_str(t).to_string())
    }

    #[napi(getter)]
    pub fn selected(&self) -> bool {
        self.data.states.selected
    }

    #[napi(getter)]
    pub fn expanded(&self) -> Option<bool> {
        self.data.states.expanded
    }

    #[napi(getter)]
    pub fn editable(&self) -> bool {
        self.data.states.editable
    }

    #[napi(getter)]
    pub fn focusable(&self) -> bool {
        self.data.states.focusable
    }

    #[napi(getter)]
    pub fn modal(&self) -> bool {
        self.data.states.modal
    }

    #[napi(getter)]
    pub fn required(&self) -> bool {
        self.data.states.required
    }

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
    #[napi(ts_return_type = "Promise<Subscription>")]
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
    type JsValue = Subscription;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.provider.subscribe(&self.data).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(Subscription::new(output, self.provider.clone()))
    }
}
