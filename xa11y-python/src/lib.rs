use std::sync::Arc;
use std::time::Duration;

use pyo3::exceptions::*;
use pyo3::prelude::*;

// ── Singleton provider ─────────────────────────────────────────────────────

fn get_provider() -> PyResult<Arc<dyn xa11y::Provider>> {
    xa11y::provider().map_err(|e| PlatformError::new_err(format!("{e}")))
}

// ── Exceptions ──────────────────────────────────────────────────────────────

pyo3::create_exception!(_native, XA11yError, PyException);
pyo3::create_exception!(_native, PermissionDeniedError, XA11yError);
pyo3::create_exception!(_native, SelectorNotMatchedError, XA11yError);
pyo3::create_exception!(_native, ActionNotSupportedError, XA11yError);
pyo3::create_exception!(_native, XA11yTimeoutError, XA11yError);
pyo3::create_exception!(_native, InvalidSelectorError, XA11yError);
pyo3::create_exception!(_native, PlatformError, XA11yError);

fn to_py_err(e: xa11y::Error) -> PyErr {
    match e {
        xa11y::Error::PermissionDenied { instructions } => {
            PermissionDeniedError::new_err(instructions)
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
            PyValueError::new_err(format!("Invalid action data: {message}"))
        }
        xa11y::Error::Platform { code, message } => {
            PlatformError::new_err(format!("Platform error ({code}): {message}"))
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Create a Python Element from Rust ElementData.
fn make_py_element(
    py: Python<'_>,
    data: &xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
) -> PyResult<Py<Element>> {
    let checked = data.states.checked.map(|t| t.to_str().to_string());
    let actions: Vec<String> = data
        .actions
        .iter()
        .map(|a| a.to_str().to_string())
        .collect();
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

// Rect is now defined in xa11y-core with #[derive(PyBindable)] and
// #[pyclass(frozen)], providing __new__, getters, __repr__, and __eq__.
use xa11y_core::Rect;

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

    /// Create a Locator scoped to this element's subtree.
    fn locator(&self, selector: &str) -> Locator {
        Locator {
            inner: xa11y::Locator::new(
                self.provider.clone(),
                Some(self.inner_data.clone()),
                selector,
            ),
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

    fn nth(&self, n: usize) -> Self {
        Self {
            inner: self.inner.clone().nth(n),
        }
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
    #[pyo3(signature = (amount=1.0))]
    fn scroll_up(&self, amount: f64) -> PyResult<()> {
        self.inner.scroll_up(amount).map_err(to_py_err)
    }
    #[pyo3(signature = (amount=1.0))]
    fn scroll_down(&self, amount: f64) -> PyResult<()> {
        self.inner.scroll_down(amount).map_err(to_py_err)
    }
    #[pyo3(signature = (amount=1.0))]
    fn scroll_left(&self, amount: f64) -> PyResult<()> {
        self.inner.scroll_left(amount).map_err(to_py_err)
    }
    #[pyo3(signature = (amount=1.0))]
    fn scroll_right(&self, amount: f64) -> PyResult<()> {
        self.inner.scroll_right(amount).map_err(to_py_err)
    }

    // ── Wait operations ──

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

// ── EventType ──────────────────────────────────────────────────────────────

// EventType is now generated by #[derive(PyBindable)] with #[py_bind(class_attrs)]
// in xa11y-core. It produces a `PyEventType` wrapper class with @classattr constants
// and a `to_py_str()` method on the Rust EventType enum.
use xa11y_core::PyEventType;

// ── Event ──────────────────────────────────────────────────────────────────

#[pyclass(frozen)]
#[derive(Clone)]
struct Event {
    #[pyo3(get)]
    event_type: String,
    #[pyo3(get)]
    app_name: String,
    #[pyo3(get)]
    app_pid: u32,
    target_data: Option<xa11y::ElementData>,
    provider: Arc<dyn xa11y::Provider>,
}

impl Event {
    fn from_core(event: xa11y::Event, provider: Arc<dyn xa11y::Provider>) -> Self {
        Self {
            event_type: event.event_type.to_py_str().to_string(),
            app_name: event.app_name,
            app_pid: event.app_pid,
            target_data: event.target,
            provider,
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
    fn wait_for(&self, predicate: PyObject, timeout: f64) -> PyResult<Event> {
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
            let maybe_event =
                self.with_sub(|sub| sub.try_recv().map(|e| Event::from_core(e, provider)))?;
            if let Some(py_event) = maybe_event {
                let matched = Python::with_gil(|py| -> PyResult<bool> {
                    let py_ref = Py::new(py, py_event.clone())?;
                    let result = predicate.call1(py, (py_ref,))?;
                    result.extract::<bool>(py)
                })?;
                if matched {
                    return Ok(py_event);
                }
            } else {
                std::thread::sleep(poll);
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

    fn __next__(&self) -> PyResult<Option<Event>> {
        let provider = self.provider.clone();
        let maybe_event = self.with_sub(|sub| {
            sub.recv(Duration::from_millis(100))
                .ok()
                .map(|e| Event::from_core(e, provider))
        })?;
        if maybe_event.is_some() {
            return Ok(maybe_event);
        }
        Python::with_gil(|py| py.check_signals())?;
        Ok(None)
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
    #[staticmethod]
    fn by_name(py: Python<'_>, name: &str) -> PyResult<Self> {
        let provider = get_provider()?;
        let app = py
            .allow_threads(move || xa11y::App::by_name_with(provider, name))
            .map_err(to_py_err)?;
        Ok(Self::from_core(app))
    }

    /// Find an application by process ID.
    #[staticmethod]
    fn by_pid(py: Python<'_>, pid: u32) -> PyResult<Self> {
        let provider = get_provider()?;
        let app = py
            .allow_threads(move || xa11y::App::by_pid_with(provider, pid))
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
    m.add_class::<PyEventType>()?;
    m.add_class::<Locator>()?;
    m.add_class::<Rect>()?;
    m.add_class::<Subscription>()?;

    m.add("XA11yError", m.py().get_type::<XA11yError>())?;
    m.add(
        "PermissionDeniedError",
        m.py().get_type::<PermissionDeniedError>(),
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

    Ok(())
}

/// CLI entry point called from the Python `xa11y` console script.
///
/// Runs the Rust CLI implementation with the given args (excluding program name).
#[pyfunction]
fn _cli_main(args: Vec<String>) -> PyResult<()> {
    xa11y::cli::run(&args).map_err(|e| PlatformError::new_err(format!("{e}")))
}

// ── Test helpers ────────────────────────────────────────────────────────────

/// Mock provider for Python unit tests.
struct MockProvider {
    nodes: Vec<MockNode>,
    actions: std::sync::Mutex<Vec<(u64, String, Option<String>)>>,
}

struct MockNode {
    data: xa11y::ElementData,
    children: Vec<usize>,
    parent: Option<usize>,
}

impl xa11y::Provider for MockProvider {
    fn get_children(
        &self,
        element: Option<&xa11y::ElementData>,
    ) -> xa11y::Result<Vec<xa11y::ElementData>> {
        match element {
            None => {
                if self.nodes.is_empty() {
                    return Ok(vec![]);
                }
                Ok(vec![self.nodes[0].data.clone()])
            }
            Some(el) => {
                let idx = el.handle as usize;
                if idx >= self.nodes.len() {
                    return Ok(vec![]);
                }
                Ok(self.nodes[idx]
                    .children
                    .iter()
                    .map(|&i| self.nodes[i].data.clone())
                    .collect())
            }
        }
    }

    fn get_parent(
        &self,
        element: &xa11y::ElementData,
    ) -> xa11y::Result<Option<xa11y::ElementData>> {
        let idx = element.handle as usize;
        if idx >= self.nodes.len() {
            return Ok(None);
        }
        Ok(self.nodes[idx].parent.map(|i| self.nodes[i].data.clone()))
    }

    fn perform_action(
        &self,
        element: &xa11y::ElementData,
        action: xa11y::Action,
        data: Option<xa11y::ActionData>,
    ) -> xa11y::Result<()> {
        let data_debug = data.map(|d| format!("{d:?}"));
        self.actions
            .lock()
            .unwrap()
            .push((element.handle, format!("{action}"), data_debug));
        Ok(())
    }

    fn subscribe(&self, _element: &xa11y::ElementData) -> xa11y::Result<xa11y::Subscription> {
        Err(xa11y::Error::Platform {
            code: -1,
            message: "MockProvider does not support subscribe".to_string(),
        })
    }
}

fn build_test_tree() -> Arc<MockProvider> {
    use xa11y::*;

    let element_defs: Vec<(
        Role,
        Option<&str>,
        Option<&str>,
        Option<&str>,
        Option<Rect>,
        Vec<Action>,
        StateSet,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<&str>,
    )> = vec![
        (
            Role::Application,
            Some("TestApp"),
            None,
            Some("Test application"),
            Some(Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }),
            vec![],
            StateSet::default(),
            None,
            None,
            None,
            Some("app-root"),
        ),
        (
            Role::Window,
            Some("Main Window"),
            None,
            None,
            Some(Rect {
                x: 100,
                y: 50,
                width: 800,
                height: 600,
            }),
            vec![],
            StateSet {
                focused: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
        ),
        (
            Role::Toolbar,
            Some("Navigation"),
            None,
            None,
            None,
            vec![],
            StateSet::default(),
            None,
            None,
            None,
            None,
        ),
        (
            Role::Button,
            Some("Back"),
            None,
            Some("Go back"),
            Some(Rect {
                x: 110,
                y: 60,
                width: 50,
                height: 30,
            }),
            vec![Action::Press, Action::Focus],
            StateSet {
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            Some("btn-back"),
        ),
        (
            Role::Button,
            Some("Forward"),
            None,
            None,
            Some(Rect {
                x: 170,
                y: 60,
                width: 50,
                height: 30,
            }),
            vec![Action::Press, Action::Focus],
            StateSet {
                enabled: false,
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
        ),
        (
            Role::Group,
            Some("Content"),
            None,
            None,
            None,
            vec![],
            StateSet::default(),
            None,
            None,
            None,
            None,
        ),
        (
            Role::TextField,
            Some("Search"),
            Some("hello"),
            Some("Search field"),
            Some(Rect {
                x: 200,
                y: 120,
                width: 300,
                height: 25,
            }),
            vec![Action::Focus, Action::SetValue, Action::TypeText],
            StateSet {
                editable: true,
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
        ),
        (
            Role::CheckBox,
            Some("Agree"),
            None,
            None,
            None,
            vec![Action::Toggle, Action::Focus],
            StateSet {
                checked: Some(Toggled::On),
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
        ),
        (
            Role::Slider,
            Some("Volume"),
            Some("75"),
            None,
            None,
            vec![
                Action::Increment,
                Action::Decrement,
                Action::SetValue,
                Action::Focus,
            ],
            StateSet {
                focusable: true,
                ..StateSet::default()
            },
            Some(75.0),
            Some(0.0),
            Some(100.0),
            None,
        ),
        (
            Role::StaticText,
            Some("Status"),
            Some("Loading..."),
            None,
            None,
            vec![],
            StateSet {
                visible: false,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
        ),
        (
            Role::List,
            Some("Items"),
            None,
            None,
            None,
            vec![],
            StateSet {
                expanded: Some(true),
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
        ),
        (
            Role::ListItem,
            Some("Item 1"),
            None,
            None,
            None,
            vec![Action::Select, Action::Focus],
            StateSet {
                selected: true,
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
        ),
        (
            Role::ListItem,
            Some("Item 2"),
            None,
            None,
            None,
            vec![Action::Select, Action::Focus],
            StateSet {
                focusable: true,
                ..StateSet::default()
            },
            None,
            None,
            None,
            None,
        ),
    ];

    let children_map: Vec<Vec<usize>> = vec![
        vec![1],    // 0: application
        vec![2, 5], // 1: window
        vec![3, 4], // 2: toolbar
        vec![],
        vec![],               // 3, 4: buttons
        vec![6, 7, 8, 9, 10], // 5: group
        vec![],
        vec![],
        vec![],
        vec![],       // 6-9: leaf nodes
        vec![11, 12], // 10: list
        vec![],
        vec![], // 11, 12: list items
    ];

    let parent_map: Vec<Option<usize>> = vec![
        None,
        Some(0),
        Some(1),
        Some(2),
        Some(2),
        Some(1),
        Some(5),
        Some(5),
        Some(5),
        Some(5),
        Some(5),
        Some(10),
        Some(10),
    ];

    let mut nodes = Vec::new();
    for (i, (role, name, value, desc, bounds, actions, states, nv, minv, maxv, sid)) in
        element_defs.into_iter().enumerate()
    {
        nodes.push(MockNode {
            data: ElementData {
                role,
                name: name.map(String::from),
                value: value.map(String::from),
                description: desc.map(String::from),
                bounds,
                actions,
                states,
                numeric_value: nv,
                min_value: minv,
                max_value: maxv,
                stable_id: sid.map(String::from),
                pid: Some(1234),
                raw: RawPlatformData::Synthetic,
                handle: i as u64,
            },
            children: children_map[i].clone(),
            parent: parent_map[i],
        });
    }

    Arc::new(MockProvider {
        nodes,
        actions: std::sync::Mutex::new(Vec::new()),
    })
}

/// Create a test Locator backed by a mock provider.
#[pyfunction]
fn _make_test_locator() -> PyResult<Locator> {
    let provider = build_test_tree();
    Ok(Locator {
        inner: xa11y::Locator::new(provider as Arc<dyn xa11y::Provider>, None, "application"),
    })
}
