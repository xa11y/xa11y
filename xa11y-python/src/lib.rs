use std::sync::Arc;
use std::time::Duration;

use pyo3::exceptions::*;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

// ── Singleton provider ─────────────────────────────────────────────────────

fn get_provider() -> PyResult<Arc<dyn xa11y::Provider>> {
    xa11y::provider().map_err(|e| PlatformError::new_err(format!("{e}")))
}

// ── Exceptions ──────────────────────────────────────────────────────────────

pyo3::create_exception!(_native, XA11yError, PyException);
pyo3::create_exception!(_native, PermissionDeniedError, XA11yError);
pyo3::create_exception!(_native, AccessibilityNotEnabledError, XA11yError);
pyo3::create_exception!(_native, SelectorNotMatchedError, XA11yError);
pyo3::create_exception!(_native, ActionNotSupportedError, XA11yError);
pyo3::create_exception!(_native, XA11yTimeoutError, XA11yError);
pyo3::create_exception!(_native, InvalidSelectorError, XA11yError);
pyo3::create_exception!(_native, InvalidActionDataError, XA11yError);
pyo3::create_exception!(_native, PlatformError, XA11yError);

fn to_py_err(e: xa11y::Error) -> PyErr {
    match e {
        xa11y::Error::PermissionDenied { instructions } => {
            PermissionDeniedError::new_err(instructions)
        }
        xa11y::Error::AccessibilityNotEnabled { app, instructions } => {
            AccessibilityNotEnabledError::new_err(format!(
                "Accessibility not enabled for {app}: {instructions}"
            ))
        }
        xa11y::Error::SelectorNotMatched { selector } => {
            SelectorNotMatchedError::new_err(format!("No element matched: {selector}"))
        }
        xa11y::Error::ElementStale { selector } => {
            SelectorNotMatchedError::new_err(format!("Element stale: {selector}"))
        }
        xa11y::Error::ActionNotSupported { action, role } => {
            ActionNotSupportedError::new_err(format!("{action} not supported on {role}"))
        }
        xa11y::Error::TextValueNotSupported => {
            ActionNotSupportedError::new_err("Text value not supported for this element")
        }
        xa11y::Error::Timeout { elapsed } => {
            XA11yTimeoutError::new_err(format!("Timeout after {elapsed:.1?}"))
        }
        xa11y::Error::InvalidSelector { selector, message } => {
            InvalidSelectorError::new_err(format!("Invalid selector '{selector}': {message}"))
        }
        xa11y::Error::InvalidActionData { message } => {
            InvalidActionDataError::new_err(format!("Invalid action data: {message}"))
        }
        xa11y::Error::Platform { code, message } => {
            PlatformError::new_err(format!("Platform error ({code}): {message}"))
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Convert a `serde_json::Value` to a Python object. Used by `Element.raw` to
/// expose platform-specific data that arrives as JSON (the provider traits
/// store it as `HashMap<String, serde_json::Value>`).
fn json_to_py(py: Python<'_>, value: &serde_json::Value) -> PyResult<PyObject> {
    Ok(match value {
        serde_json::Value::Null => py.None(),
        serde_json::Value::Bool(b) => b.into_pyobject(py)?.to_owned().into_any().unbind(),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into_pyobject(py)?.into_any().unbind()
            } else if let Some(u) = n.as_u64() {
                u.into_pyobject(py)?.into_any().unbind()
            } else if let Some(f) = n.as_f64() {
                f.into_pyobject(py)?.into_any().unbind()
            } else {
                // serde_json::Number always holds one of i64/u64/f64; the
                // above branches are exhaustive.
                return Err(PyValueError::new_err(format!(
                    "Unrepresentable JSON number: {n}"
                )));
            }
        }
        serde_json::Value::String(s) => s.into_pyobject(py)?.into_any().unbind(),
        serde_json::Value::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(json_to_py(py, item)?)?;
            }
            list.into_any().unbind()
        }
        serde_json::Value::Object(map) => {
            let dict = PyDict::new(py);
            for (k, v) in map {
                dict.set_item(k, json_to_py(py, v)?)?;
            }
            dict.into_any().unbind()
        }
    })
}

/// Create a Python Element from Rust ElementData.
fn make_py_element(
    py: Python<'_>,
    data: &xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
) -> PyResult<Py<Element>> {
    let checked = data.states.checked.map(|t| match t {
        xa11y::Toggled::Off => "off".to_string(),
        xa11y::Toggled::On => "on".to_string(),
        xa11y::Toggled::Mixed => "mixed".to_string(),
    });
    let actions: Vec<String> = data.actions.clone();
    Py::new(
        py,
        Element {
            role: data.role.to_snake_case().to_string(),
            name: data.name.clone(),
            value: data.value.clone(),
            description: data.description.clone(),
            numeric_value: data.numeric_value,
            min_value: data.min_value,
            max_value: data.max_value,
            stable_id: data.stable_id.clone(),
            pid: data.pid,
            actions,
            bounds_data: data.bounds.as_ref().map(|r| (r.x, r.y, r.width, r.height)),
            enabled: data.states.enabled,
            visible: data.states.visible,
            focused: data.states.focused,
            checked,
            selected: data.states.selected,
            expanded: data.states.expanded,
            editable: data.states.editable,
            focusable: data.states.focusable,
            modal: data.states.modal,
            required: data.states.required,
            busy: data.states.busy,
            inner_data: data.clone(),
            provider,
        },
    )
}

// ── Data Classes ────────────────────────────────────────────────────────────

#[pyclass(frozen)]
#[derive(Clone)]
struct Rect {
    #[pyo3(get)]
    x: i32,
    #[pyo3(get)]
    y: i32,
    #[pyo3(get)]
    width: u32,
    #[pyo3(get)]
    height: u32,
}

#[pymethods]
impl Rect {
    fn __repr__(&self) -> String {
        format!(
            "Rect(x={}, y={}, width={}, height={})",
            self.x, self.y, self.width, self.height
        )
    }

    fn __eq__(&self, other: &Rect) -> bool {
        self.x == other.x
            && self.y == other.y
            && self.width == other.width
            && self.height == other.height
    }
}

// ── Element ──────────────────────────────────────────────────────────────────

/// A live element with lazy navigation.
#[pyclass]
struct Element {
    #[pyo3(get)]
    role: String,
    #[pyo3(get)]
    name: Option<String>,
    #[pyo3(get)]
    value: Option<String>,
    #[pyo3(get)]
    description: Option<String>,
    #[pyo3(get)]
    numeric_value: Option<f64>,
    #[pyo3(get)]
    min_value: Option<f64>,
    #[pyo3(get)]
    max_value: Option<f64>,
    #[pyo3(get)]
    stable_id: Option<String>,
    #[pyo3(get)]
    pid: Option<u32>,
    #[pyo3(get)]
    actions: Vec<String>,

    bounds_data: Option<(i32, i32, u32, u32)>,
    #[pyo3(get)]
    enabled: bool,
    #[pyo3(get)]
    visible: bool,
    #[pyo3(get)]
    focused: bool,
    #[pyo3(get)]
    checked: Option<String>,
    #[pyo3(get)]
    selected: bool,
    #[pyo3(get)]
    expanded: Option<bool>,
    #[pyo3(get)]
    editable: bool,
    #[pyo3(get)]
    focusable: bool,
    #[pyo3(get)]
    modal: bool,
    #[pyo3(get)]
    required: bool,
    #[pyo3(get)]
    busy: bool,

    /// The underlying Rust ElementData (for provider calls).
    inner_data: xa11y::ElementData,
    /// Provider reference for lazy navigation.
    provider: Arc<dyn xa11y::Provider>,
}

#[pymethods]
impl Element {
    /// Get direct children (lazy — each call queries the provider).
    fn children(&self, py: Python<'_>) -> PyResult<Vec<Py<Element>>> {
        let provider = self.provider.clone();
        let data = self.inner_data.clone();
        let children = py
            .allow_threads(move || provider.get_children(Some(&data)))
            .map_err(to_py_err)?;
        children
            .iter()
            .map(|c| make_py_element(py, c, self.provider.clone()))
            .collect()
    }

    /// Get parent element (lazy — each call queries the provider).
    fn parent(&self, py: Python<'_>) -> PyResult<Option<Py<Element>>> {
        let provider = self.provider.clone();
        let data = self.inner_data.clone();
        let parent = py
            .allow_threads(move || provider.get_parent(&data))
            .map_err(to_py_err)?;
        match parent {
            Some(p) => Ok(Some(make_py_element(py, &p, self.provider.clone())?)),
            None => Ok(None),
        }
    }

    /// Subscribe to accessibility events for this element (typically an app).
    fn subscribe(&self, py: Python<'_>) -> PyResult<Subscription> {
        let provider = self.provider.clone();
        let data = self.inner_data.clone();
        let sub = py
            .allow_threads(move || provider.subscribe(&data))
            .map_err(to_py_err)?;
        Ok(Subscription {
            inner: std::sync::Mutex::new(Some(sub)),
            provider: self.provider.clone(),
        })
    }

    #[getter]
    fn bounds(&self) -> Option<Rect> {
        self.bounds_data.map(|(x, y, w, h)| Rect {
            x,
            y,
            width: w,
            height: h,
        })
    }

    /// Platform-specific raw data attached to this element, as a Python dict.
    ///
    /// Keys are provider-defined (e.g. `"ax_role"`, `"ax_subrole"` on macOS;
    /// `"uia_control_type"` on Windows). Values are the JSON representation
    /// the provider produced — strings, numbers, booleans, nested objects.
    ///
    /// Intended for debugging and platform-specific advanced queries. Prefer
    /// the cross-platform fields (`role`, `name`, `states`, etc.) for
    /// portable logic.
    #[getter]
    fn raw(&self, py: Python<'_>) -> PyResult<PyObject> {
        let dict = PyDict::new(py);
        for (k, v) in &self.inner_data.raw {
            dict.set_item(k, json_to_py(py, v)?)?;
        }
        Ok(dict.into_any().unbind())
    }

    fn __repr__(&self) -> String {
        let mut parts = vec![format!("role='{}'", self.role)];
        if let Some(ref n) = self.name {
            parts.push(format!("name='{n}'"));
        }
        if let Some(ref v) = self.value {
            parts.push(format!("value='{v}'"));
        }
        if !self.enabled {
            parts.push("enabled=False".to_string());
        }
        if !self.visible {
            parts.push("visible=False".to_string());
        }
        if self.focused {
            parts.push("focused=True".to_string());
        }
        format!("Element({})", parts.join(", "))
    }

    fn __str__(&self) -> String {
        self.__repr__()
    }
}

// ── Locator ─────────────────────────────────────────────────────────────────

#[pyclass]
struct Locator {
    inner: xa11y::Locator,
}

#[pymethods]
impl Locator {
    #[getter]
    fn selector(&self) -> &str {
        self.inner.selector()
    }

    fn nth(&self, n: usize) -> PyResult<Self> {
        // Reject n == 0 at the binding boundary instead of forwarding to
        // `Locator::nth`, which asserts and panics (crashes Python).
        if n == 0 {
            return Err(to_py_err(xa11y::Error::InvalidActionData {
                message: "Locator.nth is 1-based; got 0".to_string(),
            }));
        }
        Ok(Self {
            inner: self.inner.clone().nth(n),
        })
    }

    fn first(&self) -> Self {
        Self {
            inner: self.inner.clone().first(),
        }
    }

    fn child(&self, selector: &str) -> Self {
        Self {
            inner: self.inner.clone().child(selector),
        }
    }

    fn descendant(&self, selector: &str) -> Self {
        Self {
            inner: self.inner.clone().descendant(selector),
        }
    }

    // ── Queries ──

    fn exists(&self) -> PyResult<bool> {
        self.inner.exists().map_err(to_py_err)
    }

    fn count(&self) -> PyResult<usize> {
        self.inner.count().map_err(to_py_err)
    }

    fn element(&self, py: Python<'_>) -> PyResult<Py<Element>> {
        let el = self.inner.element().map_err(to_py_err)?;
        make_py_element(py, el.data(), el.provider().clone())
    }

    fn elements(&self, py: Python<'_>) -> PyResult<Vec<Py<Element>>> {
        let els = self.inner.elements().map_err(to_py_err)?;
        els.iter()
            .map(|el| make_py_element(py, el.data(), el.provider().clone()))
            .collect()
    }

    // ── Actions ──

    fn press(&self) -> PyResult<()> {
        self.inner.press().map_err(to_py_err)
    }
    fn focus(&self) -> PyResult<()> {
        self.inner.focus().map_err(to_py_err)
    }
    fn blur(&self) -> PyResult<()> {
        self.inner.blur().map_err(to_py_err)
    }
    fn toggle(&self) -> PyResult<()> {
        self.inner.toggle().map_err(to_py_err)
    }
    fn expand(&self) -> PyResult<()> {
        self.inner.expand().map_err(to_py_err)
    }
    fn collapse(&self) -> PyResult<()> {
        self.inner.collapse().map_err(to_py_err)
    }
    fn select(&self) -> PyResult<()> {
        self.inner.select().map_err(to_py_err)
    }
    fn show_menu(&self) -> PyResult<()> {
        self.inner.show_menu().map_err(to_py_err)
    }
    fn scroll_into_view(&self) -> PyResult<()> {
        self.inner.scroll_into_view().map_err(to_py_err)
    }
    fn increment(&self) -> PyResult<()> {
        self.inner.increment().map_err(to_py_err)
    }
    fn decrement(&self) -> PyResult<()> {
        self.inner.decrement().map_err(to_py_err)
    }
    fn set_value(&self, value: &str) -> PyResult<()> {
        self.inner.set_value(value).map_err(to_py_err)
    }
    fn set_numeric_value(&self, value: f64) -> PyResult<()> {
        self.inner.set_numeric_value(value).map_err(to_py_err)
    }
    fn type_text(&self, text: &str) -> PyResult<()> {
        self.inner.type_text(text).map_err(to_py_err)
    }
    fn select_text(&self, start: u32, end: u32) -> PyResult<()> {
        self.inner.select_text(start, end).map_err(to_py_err)
    }
    fn perform_action(&self, action: &str) -> PyResult<()> {
        self.inner.perform_action(action).map_err(to_py_err)
    }

    // ── Wait operations ──

    #[pyo3(signature = (timeout=5.0))]
    fn wait_visible(&self, py: Python<'_>, timeout: f64) -> PyResult<Py<Element>> {
        let el = self
            .inner
            .wait_visible(Duration::from_secs_f64(timeout))
            .map_err(to_py_err)?;
        make_py_element(py, el.data(), el.provider().clone())
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_attached(&self, py: Python<'_>, timeout: f64) -> PyResult<Py<Element>> {
        let el = self
            .inner
            .wait_attached(Duration::from_secs_f64(timeout))
            .map_err(to_py_err)?;
        make_py_element(py, el.data(), el.provider().clone())
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_detached(&self, timeout: f64) -> PyResult<()> {
        self.inner
            .wait_detached(Duration::from_secs_f64(timeout))
            .map_err(to_py_err)
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_enabled(&self, py: Python<'_>, timeout: f64) -> PyResult<Py<Element>> {
        let el = self
            .inner
            .wait_enabled(Duration::from_secs_f64(timeout))
            .map_err(to_py_err)?;
        make_py_element(py, el.data(), el.provider().clone())
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_hidden(&self, timeout: f64) -> PyResult<()> {
        self.inner
            .wait_hidden(Duration::from_secs_f64(timeout))
            .map_err(to_py_err)
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_disabled(&self, py: Python<'_>, timeout: f64) -> PyResult<Py<Element>> {
        let el = self
            .inner
            .wait_disabled(Duration::from_secs_f64(timeout))
            .map_err(to_py_err)?;
        make_py_element(py, el.data(), el.provider().clone())
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_focused(&self, py: Python<'_>, timeout: f64) -> PyResult<Py<Element>> {
        let el = self
            .inner
            .wait_focused(Duration::from_secs_f64(timeout))
            .map_err(to_py_err)?;
        make_py_element(py, el.data(), el.provider().clone())
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_unfocused(&self, py: Python<'_>, timeout: f64) -> PyResult<Py<Element>> {
        let el = self
            .inner
            .wait_unfocused(Duration::from_secs_f64(timeout))
            .map_err(to_py_err)?;
        make_py_element(py, el.data(), el.provider().clone())
    }

    /// Wait until an arbitrary Python predicate is satisfied.
    #[pyo3(signature = (predicate, timeout=5.0))]
    fn wait_until(&self, predicate: PyObject, timeout: f64) -> PyResult<()> {
        let provider = self.inner.provider().clone();
        self.inner
            .wait_until(
                |element_data: Option<&xa11y::ElementData>| {
                    Python::with_gil(|py| -> bool {
                        let arg: PyObject = match element_data {
                            Some(data) => match make_py_element(py, data, provider.clone()) {
                                Ok(el) => el.into_any(),
                                Err(_) => py.None(),
                            },
                            None => py.None(),
                        };
                        predicate
                            .call1(py, (arg,))
                            .and_then(|r| r.extract::<bool>(py))
                            .unwrap_or(false)
                    })
                },
                Duration::from_secs_f64(timeout),
            )
            .map_err(to_py_err)?;
        Ok(())
    }

    fn __repr__(&self) -> String {
        format!("Locator(selector='{}')", self.inner.selector())
    }
}

// ── EventKind ─────────────────────────────────────────────────────────────

fn event_kind_to_str(kind: &xa11y::EventKind) -> &'static str {
    match kind {
        xa11y::EventKind::FocusChanged => "focus_changed",
        xa11y::EventKind::ValueChanged => "value_changed",
        xa11y::EventKind::NameChanged => "name_changed",
        xa11y::EventKind::StateChanged { .. } => "state_changed",
        xa11y::EventKind::StructureChanged => "structure_changed",
        xa11y::EventKind::WindowOpened => "window_opened",
        xa11y::EventKind::WindowClosed => "window_closed",
        xa11y::EventKind::WindowActivated => "window_activated",
        xa11y::EventKind::WindowDeactivated => "window_deactivated",
        xa11y::EventKind::SelectionChanged => "selection_changed",
        xa11y::EventKind::MenuOpened => "menu_opened",
        xa11y::EventKind::MenuClosed => "menu_closed",
        xa11y::EventKind::TextChanged => "text_changed",
        xa11y::EventKind::Announcement => "announcement",
    }
}

fn state_flag_to_str(flag: xa11y::StateFlag) -> &'static str {
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

#[pyclass(frozen)]
struct EventType;

#[pymethods]
impl EventType {
    #[classattr]
    const FOCUS_CHANGED: &'static str = "focus_changed";
    #[classattr]
    const VALUE_CHANGED: &'static str = "value_changed";
    #[classattr]
    const NAME_CHANGED: &'static str = "name_changed";
    #[classattr]
    const STATE_CHANGED: &'static str = "state_changed";
    #[classattr]
    const STRUCTURE_CHANGED: &'static str = "structure_changed";
    #[classattr]
    const WINDOW_OPENED: &'static str = "window_opened";
    #[classattr]
    const WINDOW_CLOSED: &'static str = "window_closed";
    #[classattr]
    const WINDOW_ACTIVATED: &'static str = "window_activated";
    #[classattr]
    const WINDOW_DEACTIVATED: &'static str = "window_deactivated";
    #[classattr]
    const SELECTION_CHANGED: &'static str = "selection_changed";
    #[classattr]
    const MENU_OPENED: &'static str = "menu_opened";
    #[classattr]
    const MENU_CLOSED: &'static str = "menu_closed";
    #[classattr]
    const TEXT_CHANGED: &'static str = "text_changed";
    #[classattr]
    const ANNOUNCEMENT: &'static str = "announcement";
}

// ── Event ──────────────────────────────────────────────────────────────────

#[pyclass(frozen)]
#[derive(Clone)]
struct Event {
    /// String representation of the event kind (e.g. "focus_changed").
    #[pyo3(get)]
    event_type: String,
    #[pyo3(get)]
    app_name: String,
    #[pyo3(get)]
    app_pid: u32,
    target_data: Option<xa11y::ElementData>,
    provider: Arc<dyn xa11y::Provider>,
    /// For state_changed events: which flag changed (e.g. "checked").
    #[pyo3(get)]
    state_flag: Option<String>,
    /// For state_changed events: the new boolean value.
    #[pyo3(get)]
    state_value: Option<bool>,
}

impl Event {
    fn from_core(event: xa11y::Event, provider: Arc<dyn xa11y::Provider>) -> Self {
        let (state_flag, state_value) = match &event.kind {
            xa11y::EventKind::StateChanged { flag, value } => {
                (Some(state_flag_to_str(*flag).to_string()), Some(*value))
            }
            _ => (None, None),
        };
        Self {
            event_type: event_kind_to_str(&event.kind).to_string(),
            app_name: event.app_name,
            app_pid: event.app_pid,
            target_data: event.target,
            provider,
            state_flag,
            state_value,
        }
    }
}

#[pymethods]
impl Event {
    #[getter]
    fn target(&self, py: Python<'_>) -> PyResult<Option<Py<Element>>> {
        match self.target_data.as_ref() {
            Some(data) => Ok(Some(make_py_element(py, data, self.provider.clone())?)),
            None => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "Event(event_type='{}', app_name='{}', app_pid={})",
            self.event_type, self.app_name, self.app_pid
        )
    }
}

// ── Subscription ───────────────────────────────────────────────────────────

#[pyclass]
struct Subscription {
    inner: std::sync::Mutex<Option<xa11y::Subscription>>,
    provider: Arc<dyn xa11y::Provider>,
}

impl Subscription {
    fn with_sub<T>(&self, f: impl FnOnce(&xa11y::Subscription) -> T) -> PyResult<T> {
        let guard = self.inner.lock().unwrap();
        let sub = guard
            .as_ref()
            .ok_or_else(|| PlatformError::new_err("Subscription is closed"))?;
        Ok(f(sub))
    }
}

#[pymethods]
impl Subscription {
    fn try_recv(&self) -> PyResult<Option<Event>> {
        let provider = self.provider.clone();
        self.with_sub(|sub| sub.try_recv().map(|e| Event::from_core(e, provider)))
    }

    #[pyo3(signature = (timeout=5.0))]
    fn recv(&self, py: Python<'_>, timeout: f64) -> PyResult<Event> {
        let dur = Duration::from_secs_f64(timeout);
        let provider = self.provider.clone();
        py.allow_threads(|| {
            self.with_sub(|sub| sub.recv(dur).map(|e| Event::from_core(e, provider)))
        })
        .and_then(|r| r.map_err(to_py_err))
    }

    #[pyo3(signature = (predicate, timeout=5.0))]
    fn wait_for(&self, py: Python<'_>, predicate: PyObject, timeout: f64) -> PyResult<Event> {
        let dur = Duration::from_secs_f64(timeout);
        let start = std::time::Instant::now();

        loop {
            let remaining = dur.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(to_py_err(xa11y::Error::Timeout {
                    elapsed: start.elapsed(),
                }));
            }
            let poll = remaining.min(Duration::from_millis(50));
            let provider = self.provider.clone();
            // Use recv_status so a sender-disconnect is surfaced explicitly
            // rather than silently spinning forever (tenet 1).
            let status = py.allow_threads(|| self.with_sub(|sub| sub.recv_status(poll)))?;
            let py_event = match status {
                xa11y::RecvStatus::Event(evt) => Event::from_core(*evt, provider),
                xa11y::RecvStatus::Timeout => {
                    py.check_signals()?;
                    continue;
                }
                xa11y::RecvStatus::Disconnected => {
                    // Event source is gone — no further events can match the
                    // predicate. Surface this as a platform error rather than
                    // hanging until the overall timeout elapses.
                    return Err(PlatformError::new_err(
                        "Subscription event source disconnected before predicate matched",
                    ));
                }
            };
            let matched = {
                let py_ref = Py::new(py, py_event.clone())?;
                let result = predicate.call1(py, (py_ref,))?;
                result.extract::<bool>(py)?
            };
            if matched {
                return Ok(py_event);
            }
        }
    }

    fn close(&self) {
        self.inner.lock().unwrap().take();
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        _exc_type: Option<&Bound<'_, pyo3::types::PyAny>>,
        _exc_val: Option<&Bound<'_, pyo3::types::PyAny>>,
        _exc_tb: Option<&Bound<'_, pyo3::types::PyAny>>,
    ) -> bool {
        self.close();
        false
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Event> {
        // Loop, polling in short bursts so KeyboardInterrupt is responsive.
        // Distinguish:
        //   - Event     → yield it
        //   - Timeout   → continue polling (stream still live)
        //   - Disconnect → raise StopIteration (sender gone, stream finished)
        // Returning `Ok(None)` from __next__ would terminate iteration on the
        // first timeout, which is wrong — the stream is still open. Using
        // recv_status lets us surface only the actual end-of-stream condition
        // as StopIteration (tenet 1: no silent fallbacks).
        loop {
            let status = py.allow_threads(|| {
                self.with_sub(|sub| sub.recv_status(Duration::from_millis(100)))
            })?;
            match status {
                xa11y::RecvStatus::Event(evt) => {
                    return Ok(Event::from_core(*evt, self.provider.clone()));
                }
                xa11y::RecvStatus::Timeout => {
                    py.check_signals()?;
                    continue;
                }
                xa11y::RecvStatus::Disconnected => {
                    return Err(PyStopIteration::new_err(()));
                }
            }
        }
    }

    fn __repr__(&self) -> String {
        if self.inner.lock().unwrap().is_some() {
            "Subscription(active)".to_string()
        } else {
            "Subscription(closed)".to_string()
        }
    }
}

// ── App ─────────────────────────────────────────────────────────────────────

/// A running application — the entry point for accessibility queries.
///
/// Convert an optional `timeout` in seconds (as exposed to Python) to a
/// [`Duration`]. `None` and zero both mean "no polling, single attempt".
/// Negative or non-finite values raise `ValueError`.
fn timeout_from(timeout: Option<f64>) -> PyResult<Duration> {
    match timeout {
        None => Ok(Duration::ZERO),
        Some(t) if t.is_finite() && t >= 0.0 => Ok(Duration::from_secs_f64(t)),
        Some(t) => Err(PyValueError::new_err(format!(
            "timeout must be a non-negative number of seconds, got {t}"
        ))),
    }
}

/// `App` is **not** an `Element`. It represents the application as a whole
/// and provides a `locator()` to search its accessibility tree.
#[pyclass(frozen)]
struct App {
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    pid: Option<u32>,
    inner_data: xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
}

#[pymethods]
impl App {
    /// Find an application by exact name.
    ///
    /// If `timeout` is set (in seconds), poll the accessibility API until the
    /// app appears or the timeout elapses. Useful when the app may not yet
    /// be registered (e.g. just-launched). Only "not found" errors trigger a
    /// retry; other errors fail fast.
    #[staticmethod]
    #[pyo3(signature = (name, *, timeout=None))]
    fn by_name(py: Python<'_>, name: &str, timeout: Option<f64>) -> PyResult<Self> {
        let provider = get_provider()?;
        let timeout = timeout_from(timeout)?;
        let app = py
            .allow_threads(move || xa11y::App::by_name_with_timeout(provider, name, timeout))
            .map_err(to_py_err)?;
        Ok(Self::from_core(app))
    }

    /// Find an application by process ID.
    ///
    /// See [`by_name`] for `timeout` semantics.
    #[staticmethod]
    #[pyo3(signature = (pid, *, timeout=None))]
    fn by_pid(py: Python<'_>, pid: u32, timeout: Option<f64>) -> PyResult<Self> {
        let provider = get_provider()?;
        let timeout = timeout_from(timeout)?;
        let app = py
            .allow_threads(move || xa11y::App::by_pid_with_timeout(provider, pid, timeout))
            .map_err(to_py_err)?;
        Ok(Self::from_core(app))
    }

    /// List all running applications.
    #[staticmethod]
    fn list(py: Python<'_>) -> PyResult<Vec<Self>> {
        let provider = get_provider()?;
        let apps = py
            .allow_threads(move || xa11y::App::list_with(provider))
            .map_err(to_py_err)?;
        Ok(apps.into_iter().map(Self::from_core).collect())
    }

    /// Create a Locator scoped to this application's accessibility tree.
    fn locator(&self, selector: &str) -> Locator {
        Locator {
            inner: xa11y::Locator::new(
                self.provider.clone(),
                Some(self.inner_data.clone()),
                selector,
            ),
        }
    }

    /// Subscribe to accessibility events from this application.
    fn subscribe(&self, py: Python<'_>) -> PyResult<Subscription> {
        let provider = self.provider.clone();
        let data = self.inner_data.clone();
        let sub = py
            .allow_threads(move || provider.subscribe(&data))
            .map_err(to_py_err)?;
        Ok(Subscription {
            inner: std::sync::Mutex::new(Some(sub)),
            provider: self.provider.clone(),
        })
    }

    /// Get direct children (typically windows) of this application.
    fn children(&self, py: Python<'_>) -> PyResult<Vec<Py<Element>>> {
        let provider = self.provider.clone();
        let data = self.inner_data.clone();
        let children = py
            .allow_threads(move || provider.get_children(Some(&data)))
            .map_err(to_py_err)?;
        children
            .iter()
            .map(|c| make_py_element(py, c, self.provider.clone()))
            .collect()
    }

    fn __repr__(&self) -> String {
        match self.pid {
            Some(pid) => format!("App(name='{}', pid={})", self.name, pid),
            None => format!("App(name='{}')", self.name),
        }
    }

    fn __str__(&self) -> String {
        self.__repr__()
    }
}

impl App {
    fn from_core(app: xa11y::App) -> Self {
        Self {
            name: app.name.clone(),
            pid: app.pid,
            provider: app.provider().clone(),
            inner_data: app.data.clone(),
        }
    }
}

// ── Module-level functions ──────────────────────────────────────────────────

/// Create a top-level Locator.
#[pyfunction]
#[pyo3(signature = (selector))]
fn locator_fn(selector: &str) -> PyResult<Locator> {
    let provider = get_provider()?;
    Ok(Locator {
        inner: xa11y::Locator::new(provider, None, selector),
    })
}

// ── Module definition ───────────────────────────────────────────────────────

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<App>()?;
    m.add_class::<Element>()?;
    m.add_class::<Event>()?;
    m.add_class::<EventType>()?;
    m.add_class::<Locator>()?;
    m.add_class::<Rect>()?;
    m.add_class::<Subscription>()?;

    m.add("XA11yError", m.py().get_type::<XA11yError>())?;
    m.add(
        "PermissionDeniedError",
        m.py().get_type::<PermissionDeniedError>(),
    )?;
    m.add(
        "AccessibilityNotEnabledError",
        m.py().get_type::<AccessibilityNotEnabledError>(),
    )?;
    m.add(
        "SelectorNotMatchedError",
        m.py().get_type::<SelectorNotMatchedError>(),
    )?;
    m.add(
        "ActionNotSupportedError",
        m.py().get_type::<ActionNotSupportedError>(),
    )?;
    m.add("TimeoutError", m.py().get_type::<XA11yTimeoutError>())?;
    m.add(
        "InvalidSelectorError",
        m.py().get_type::<InvalidSelectorError>(),
    )?;
    m.add(
        "InvalidActionDataError",
        m.py().get_type::<InvalidActionDataError>(),
    )?;
    m.add("PlatformError", m.py().get_type::<PlatformError>())?;

    // Module-level locator function (renamed from "locator" to avoid Rust naming conflict)
    m.add_function(wrap_pyfunction!(locator_fn, m)?)?;
    // Re-export as "locator" in Python
    let locator_fn_obj = m.getattr("locator_fn")?;
    m.setattr("locator", &locator_fn_obj)?;

    // CLI entry point
    m.add_function(wrap_pyfunction!(_cli_main, m)?)?;

    // Test helpers
    m.add_function(wrap_pyfunction!(_make_test_locator, m)?)?;
    m.add_function(wrap_pyfunction!(_make_disconnected_subscription, m)?)?;

    Ok(())
}

/// CLI entry point called from the Python `xa11y` console script.
///
/// Runs the Rust CLI implementation with the given args (excluding program name).
#[pyfunction]
fn _cli_main(args: Vec<String>) -> PyResult<()> {
    xa11y::cli::run(&args).map_err(to_py_err)
}

// ── Test helpers ────────────────────────────────────────────────────────────
//
// The mock Provider, tree, and action log used by these helpers live in
// `xa11y-core::mock` (gated by the `test-support` feature) so the JS
// bindings can share the exact same fixture without a parallel copy.

/// Create a test Locator backed by the shared mock provider.
#[pyfunction]
fn _make_test_locator() -> PyResult<Locator> {
    let provider = xa11y::mock::build_provider();
    Ok(Locator {
        inner: xa11y::Locator::new(provider as Arc<dyn xa11y::Provider>, None, "application"),
    })
}

/// Create a Subscription whose backing channel has already been disconnected.
///
/// Lets Python tests exercise the iterator/recv disconnect paths without
/// needing a live platform event source.
#[pyfunction]
fn _make_disconnected_subscription() -> Subscription {
    Subscription {
        inner: std::sync::Mutex::new(Some(xa11y::mock::disconnected_subscription())),
        provider: xa11y::mock::build_provider(),
    }
}
