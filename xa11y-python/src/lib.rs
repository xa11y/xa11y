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

// Exception class names are kept identical to the public `xa11y` package
// names so tracebacks read `xa11y.TimeoutError: ...` rather than leaking the
// `_native` private module path or an internal Rust-only name. The class's
// `__module__` is patched to `"xa11y"` in `_native()` below. See issue #189.
pyo3::create_exception!(_native, XA11yError, PyException);
pyo3::create_exception!(_native, PermissionDeniedError, XA11yError);
pyo3::create_exception!(_native, AccessibilityNotEnabledError, XA11yError);
pyo3::create_exception!(_native, SelectorNotMatchedError, XA11yError);
pyo3::create_exception!(_native, ActionNotSupportedError, XA11yError);
pyo3::create_exception!(_native, TimeoutError, XA11yError);
pyo3::create_exception!(_native, InvalidSelectorError, XA11yError);
pyo3::create_exception!(_native, InvalidActionDataError, XA11yError);
pyo3::create_exception!(_native, PlatformError, XA11yError);

/// Add an exception class to `m` and re-anchor its `__module__` to the
/// public `xa11y` package.
///
/// `pyo3::create_exception!(_native, …)` bakes `__module__ = "_native"` into
/// the heap type, which leaks the private submodule path into tracebacks
/// (e.g. `_native.TimeoutError: …`). The documented surface is `xa11y`, so we
/// patch `__module__` before exposing the type. This is purely cosmetic for
/// `isinstance` checks — they're based on class identity, which is unchanged —
/// but it keeps tracebacks honest about the public path. See issue #189.
fn register_exception<E>(m: &Bound<'_, PyModule>, name: &str) -> PyResult<()>
where
    E: pyo3::type_object::PyTypeInfo,
{
    let ty = m.py().get_type::<E>();
    ty.setattr("__module__", "xa11y")?;
    m.add(name, ty)?;
    Ok(())
}

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
            TimeoutError::new_err(format!("Timeout after {elapsed:.1?}"))
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
        xa11y::Error::NoElementBounds => {
            PyValueError::new_err("Element has no bounds; cannot compute a screen point")
        }
        xa11y::Error::Unsupported { feature } => {
            ActionNotSupportedError::new_err(format!("Unsupported: {feature}"))
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

fn tree_node_to_py(py: Python<'_>, node: &xa11y::TreeNode) -> PyResult<PyObject> {
    let dict = PyDict::new(py);
    dict.set_item("role", &node.role)?;
    match &node.name {
        Some(n) => dict.set_item("name", n)?,
        None => dict.set_item("name", py.None())?,
    }
    match &node.value {
        Some(v) => dict.set_item("value", v)?,
        None => dict.set_item("value", py.None())?,
    }
    let children: Vec<PyObject> = node
        .children
        .iter()
        .map(|child| tree_node_to_py(py, child))
        .collect::<PyResult<_>>()?;
    dict.set_item("children", children)?;
    Ok(dict.into_any().unbind())
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

    /// Capture the subtree rooted at this element as a recursive dict snapshot.
    ///
    /// Each dict has keys ``role``, ``name``, ``value``, and ``children``
    /// (a list of dicts with the same shape). ``max_depth`` limits traversal:
    /// ``0`` = only this node, ``1`` = node + direct children, ``None`` = full subtree.
    #[pyo3(signature = (max_depth=None))]
    fn tree(&self, py: Python<'_>, max_depth: Option<usize>) -> PyResult<PyObject> {
        let element = xa11y::Element::new(self.inner_data.clone(), self.provider.clone());
        let node = py
            .allow_threads(move || element.tree(max_depth))
            .map_err(to_py_err)?;
        tree_node_to_py(py, &node)
    }

    /// Render the subtree rooted at this element as an indented string.
    ///
    /// Returns the string without printing it. Same depth semantics as ``tree()``.
    #[pyo3(signature = (max_depth=None))]
    fn dump(&self, py: Python<'_>, max_depth: Option<usize>) -> PyResult<String> {
        let element = xa11y::Element::new(self.inner_data.clone(), self.provider.clone());
        py.allow_threads(move || element.dump(max_depth))
            .map_err(to_py_err)
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

    // ── Actions ──
    //
    // These act on the captured snapshot rather than re-resolving the selector
    // (contrast with Locator, which re-queries the provider on every call).

    /// Press (default activate) this element.
    fn press(&self) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .press()
            .map_err(to_py_err)
    }
    /// Move keyboard focus to this element.
    fn focus(&self) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .focus()
            .map_err(to_py_err)
    }
    /// Remove keyboard focus from this element.
    fn blur(&self) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .blur()
            .map_err(to_py_err)
    }
    /// Toggle this element's checked state.
    fn toggle(&self) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .toggle()
            .map_err(to_py_err)
    }
    /// Expand this element (e.g. tree node, combo box).
    fn expand(&self) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .expand()
            .map_err(to_py_err)
    }
    /// Collapse this element.
    fn collapse(&self) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .collapse()
            .map_err(to_py_err)
    }
    /// Select this element (e.g. list item, tab).
    fn select(&self) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .select()
            .map_err(to_py_err)
    }
    /// Show this element's context menu.
    fn show_menu(&self) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .show_menu()
            .map_err(to_py_err)
    }
    /// Scroll this element into view.
    fn scroll_into_view(&self) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .scroll_into_view()
            .map_err(to_py_err)
    }
    /// Increment this element's value (e.g. slider, spinner).
    fn increment(&self) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .increment()
            .map_err(to_py_err)
    }
    /// Decrement this element's value.
    fn decrement(&self) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .decrement()
            .map_err(to_py_err)
    }
    /// Replace this element's text value.
    fn set_value(&self, value: &str) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .set_value(value)
            .map_err(to_py_err)
    }
    /// Set this element's numeric value.
    fn set_numeric_value(&self, value: f64) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .set_numeric_value(value)
            .map_err(to_py_err)
    }
    /// Insert text at the current cursor position.
    fn type_text(&self, text: &str) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .type_text(text)
            .map_err(to_py_err)
    }
    /// Select the text range from `start` to `end` (0-based character offsets).
    fn select_text(&self, start: u32, end: u32) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .select_text(start, end)
            .map_err(to_py_err)
    }
    /// Perform an action by its ``snake_case`` name.
    fn perform_action(&self, action: &str) -> PyResult<()> {
        xa11y::Element::new(self.inner_data.clone(), self.provider.clone())
            .perform_action(action)
            .map_err(to_py_err)
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

    /// Capture the subtree rooted at the matched element as a recursive dict.
    ///
    /// Each dict has keys ``role``, ``name``, ``value``, and ``children``
    /// (a list of dicts with the same shape). ``max_depth`` limits traversal:
    /// ``0`` = only this node, ``1`` = node + direct children, ``None`` =
    /// full subtree.
    ///
    /// Resolves the selector once; fails fast with
    /// :class:`SelectorNotMatchedError` if no match — does not auto-wait.
    #[pyo3(signature = (max_depth=None))]
    fn tree(&self, py: Python<'_>, max_depth: Option<usize>) -> PyResult<PyObject> {
        let locator = self.inner.clone();
        let node = py
            .allow_threads(move || locator.tree(max_depth))
            .map_err(to_py_err)?;
        tree_node_to_py(py, &node)
    }

    /// Render the subtree rooted at the matched element as an indented string.
    ///
    /// Returns the string without printing it. Same depth and resolution
    /// semantics as :meth:`tree`.
    #[pyo3(signature = (max_depth=None))]
    fn dump(&self, py: Python<'_>, max_depth: Option<usize>) -> PyResult<String> {
        let locator = self.inner.clone();
        py.allow_threads(move || locator.dump(max_depth))
            .map_err(to_py_err)
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
/// Convert a `timeout` in seconds (as exposed to Python) to a [`Duration`].
/// Zero means "no polling, single attempt". Negative or non-finite values
/// raise `ValueError`.
fn timeout_from(timeout: f64) -> PyResult<Duration> {
    if timeout.is_finite() && timeout >= 0.0 {
        Ok(Duration::from_secs_f64(timeout))
    } else {
        Err(PyValueError::new_err(format!(
            "timeout must be a non-negative number of seconds, got {timeout}"
        )))
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
    /// Polls the accessibility API until the app appears or `timeout`
    /// (in seconds) elapses. Defaults to 5 seconds — pass `timeout=0`
    /// for a single attempt with no waiting. Only "not found" errors
    /// trigger a retry; other errors fail fast.
    #[staticmethod]
    #[pyo3(signature = (name, *, timeout=5.0))]
    fn by_name(py: Python<'_>, name: &str, timeout: f64) -> PyResult<Self> {
        // Validate `timeout` before touching the provider so callers get a
        // crisp `ValueError` regardless of whether accessibility is set up.
        let timeout = timeout_from(timeout)?;
        let provider = get_provider()?;
        let app = py
            .allow_threads(move || xa11y::App::by_name_with(provider, name, timeout))
            .map_err(to_py_err)?;
        Ok(Self::from_core(app))
    }

    /// Find an application by process ID.
    ///
    /// See [`by_name`] for `timeout` semantics.
    #[staticmethod]
    #[pyo3(signature = (pid, *, timeout=5.0))]
    fn by_pid(py: Python<'_>, pid: u32, timeout: f64) -> PyResult<Self> {
        let timeout = timeout_from(timeout)?;
        let provider = get_provider()?;
        let app = py
            .allow_threads(move || xa11y::App::by_pid_with(provider, pid, timeout))
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

    /// Get an :class:`Element` handle for the application root.
    ///
    /// Useful for invoking Element-level methods (``children()``,
    /// ``parent()``, etc.) without going through a locator.
    fn as_element(&self, py: Python<'_>) -> PyResult<Py<Element>> {
        make_py_element(py, &self.inner_data, self.provider.clone())
    }

    /// Capture this application's accessibility tree as a recursive dict snapshot.
    ///
    /// Each dict has keys ``role``, ``name``, ``value``, and ``children``
    /// (a list of dicts with the same shape). ``max_depth`` limits traversal:
    /// ``0`` = only the application node, ``1`` = application + direct
    /// children (typically windows), ``None`` = full subtree.
    ///
    /// Equivalent to ``Element.tree(...)`` on the application's root element.
    #[pyo3(signature = (max_depth=None))]
    fn tree(&self, py: Python<'_>, max_depth: Option<usize>) -> PyResult<PyObject> {
        let element = xa11y::Element::new(self.inner_data.clone(), self.provider.clone());
        let node = py
            .allow_threads(move || element.tree(max_depth))
            .map_err(to_py_err)?;
        tree_node_to_py(py, &node)
    }

    /// Render this application's accessibility tree as an indented string.
    ///
    /// Returns the string without printing it. Same depth semantics as
    /// ``tree()``. This is the primary inspection helper — call
    /// ``print(app.dump())`` to discover the role and name of every element
    /// in the app before writing selectors.
    ///
    /// For the same output from the shell, use ``xa11y tree --app NAME``.
    #[pyo3(signature = (max_depth=None))]
    fn dump(&self, py: Python<'_>, max_depth: Option<usize>) -> PyResult<String> {
        let element = xa11y::Element::new(self.inner_data.clone(), self.provider.clone());
        py.allow_threads(move || element.dump(max_depth))
            .map_err(to_py_err)
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

// ── Input simulation ────────────────────────────────────────────────────────

/// Input-simulation façade. Constructed via [`input_sim()`][input_sim_fn].
///
/// Methods accept targets as either a `(x, y)` tuple or an `Element` (uses
/// centre bounds). `Key` values are Python strings — see [`_parse_key`] for
/// the grammar; short version: printable characters are literal
/// (`"a"`, `"7"`, `";"`), named keys use their Pascal name (`"Enter"`,
/// `"ArrowUp"`, `"F5"`), modifiers are `"Shift"`, `"Ctrl"`, `"Alt"`, `"Meta"`.
#[pyclass]
struct InputSim {
    inner: xa11y::InputSim,
}

#[pymethods]
impl InputSim {
    /// Left-click at `target` once.
    fn click(&self, target: Bound<'_, PyAny>) -> PyResult<()> {
        let pt = parse_target(&target)?;
        self.inner.mouse().click(pt).map_err(to_py_err)
    }

    /// Left double-click at `target`.
    fn double_click(&self, target: Bound<'_, PyAny>) -> PyResult<()> {
        let pt = parse_target(&target)?;
        self.inner.mouse().double_click(pt).map_err(to_py_err)
    }

    /// Right-click at `target`.
    fn right_click(&self, target: Bound<'_, PyAny>) -> PyResult<()> {
        let pt = parse_target(&target)?;
        self.inner.mouse().right_click(pt).map_err(to_py_err)
    }

    /// Move the pointer to `target` without pressing any button.
    fn move_to(&self, target: Bound<'_, PyAny>) -> PyResult<()> {
        let pt = parse_target(&target)?;
        self.inner.mouse().move_to(pt).map_err(to_py_err)
    }

    /// Left-drag from `start` to `end`. Default duration 150 ms.
    fn drag(&self, start: Bound<'_, PyAny>, end: Bound<'_, PyAny>) -> PyResult<()> {
        let from = parse_target(&start)?;
        let to = parse_target(&end)?;
        self.inner.mouse().drag(from, to).map_err(to_py_err)
    }

    /// Scroll at `target`. `dx` positive → scroll right, `dy` positive →
    /// scroll content down.
    #[pyo3(signature = (target, dx=0, dy=0))]
    fn scroll(&self, target: Bound<'_, PyAny>, dx: i32, dy: i32) -> PyResult<()> {
        let pt = parse_target(&target)?;
        self.inner
            .mouse()
            .scroll(pt, xa11y::ScrollDelta::new(dx, dy))
            .map_err(to_py_err)
    }

    /// Tap a key (press + release). See the class docstring for key names.
    fn press(&self, key: &str) -> PyResult<()> {
        let k = parse_key(key)?;
        self.inner.keyboard().press(k).map_err(to_py_err)
    }

    /// Tap `key` while `held` (list of key names) are held.
    #[pyo3(signature = (key, held=Vec::new()))]
    fn chord(&self, key: &str, held: Vec<String>) -> PyResult<()> {
        let k = parse_key(key)?;
        let held: Result<Vec<_>, _> = held.iter().map(|s| parse_key(s)).collect();
        self.inner.keyboard().chord(k, &held?).map_err(to_py_err)
    }

    /// Type literal text into the currently focused control.
    fn type_text(&self, text: &str) -> PyResult<()> {
        self.inner.keyboard().type_text(text).map_err(to_py_err)
    }
}

/// Convert a Python target (`(int, int)` tuple or `Element`) to an
/// [`xa11y::Point`]. Keeps the target-resolution cost explicit at the call
/// site, matching the Rust [`IntoPoint`] contract.
fn parse_target(target: &Bound<'_, PyAny>) -> PyResult<xa11y::Point> {
    if let Ok(el) = target.downcast::<Element>() {
        let element = el.borrow();
        let (x, y, w, h) = element
            .bounds_data
            .ok_or_else(|| PyValueError::new_err("Element has no bounds"))?;
        return Ok(xa11y::Point::new(x + (w as i32) / 2, y + (h as i32) / 2));
    }
    let tup: (i32, i32) = target
        .extract()
        .map_err(|_| PyTypeError::new_err("expected (int, int) tuple or Element for target"))?;
    Ok(xa11y::Point::new(tup.0, tup.1))
}

/// Parse a key-name string into an [`xa11y::Key`]. Accepts:
/// single chars (`"a"`, `"7"`), named modifiers (`"Shift"`, `"Ctrl"`,
/// `"Alt"`, `"Meta"`), named keys (`"Enter"`, `"Tab"`, `"Escape"`,
/// `"Backspace"`, `"Space"`, `"Delete"`, `"Insert"`, `"Home"`, `"End"`,
/// `"PageUp"`, `"PageDown"`, `"ArrowUp/Down/Left/Right"`), and function
/// keys `"F1"` through `"F24"`.
fn parse_key(name: &str) -> PyResult<xa11y::Key> {
    let k = match name {
        "Shift" => xa11y::Key::Shift,
        "Ctrl" | "Control" => xa11y::Key::Ctrl,
        "Alt" | "Option" => xa11y::Key::Alt,
        "Meta" | "Cmd" | "Command" | "Super" | "Win" => xa11y::Key::Meta,
        "Enter" | "Return" => xa11y::Key::Enter,
        "Escape" | "Esc" => xa11y::Key::Escape,
        "Backspace" => xa11y::Key::Backspace,
        "Tab" => xa11y::Key::Tab,
        "Space" => xa11y::Key::Space,
        "Delete" => xa11y::Key::Delete,
        "Insert" => xa11y::Key::Insert,
        "ArrowUp" | "Up" => xa11y::Key::ArrowUp,
        "ArrowDown" | "Down" => xa11y::Key::ArrowDown,
        "ArrowLeft" | "Left" => xa11y::Key::ArrowLeft,
        "ArrowRight" | "Right" => xa11y::Key::ArrowRight,
        "Home" => xa11y::Key::Home,
        "End" => xa11y::Key::End,
        "PageUp" => xa11y::Key::PageUp,
        "PageDown" => xa11y::Key::PageDown,
        s if s.starts_with('F') && s.len() >= 2 && s[1..].chars().all(|c| c.is_ascii_digit()) => {
            let n: u8 = s[1..]
                .parse()
                .map_err(|_| PyValueError::new_err(format!("Invalid function key: {s}")))?;
            xa11y::Key::F(n)
        }
        s if s.chars().count() == 1 => xa11y::Key::Char(s.chars().next().unwrap()),
        _ => return Err(PyValueError::new_err(format!("Unknown key name: {name}"))),
    };
    Ok(k)
}

/// Construct an [`InputSim`] backed by the platform's native input path.
#[pyfunction]
fn input_sim() -> PyResult<InputSim> {
    let sim = xa11y::input_sim().map_err(to_py_err)?;
    Ok(InputSim { inner: sim })
}

// ── Screenshot ──────────────────────────────────────────────────────────────

/// A captured image: raw RGBA8 pixels plus dimensions and scale.
///
/// `width` and `height` are in physical pixels. `scale` is the physical-to-
/// logical ratio (1.0 on standard displays, 2.0 on typical Retina). `pixels`
/// length is `width * height * 4` (RGBA).
#[pyclass(frozen)]
struct Screenshot {
    #[pyo3(get)]
    width: u32,
    #[pyo3(get)]
    height: u32,
    #[pyo3(get)]
    scale: f32,
    inner: xa11y::Screenshot,
}

#[pymethods]
impl Screenshot {
    /// Raw RGBA8 pixel bytes (`width * height * 4`).
    #[getter]
    fn pixels<'py>(&self, py: Python<'py>) -> Bound<'py, pyo3::types::PyBytes> {
        pyo3::types::PyBytes::new(py, &self.inner.pixels)
    }

    /// Encode as PNG and return the bytes.
    fn to_png<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PyBytes>> {
        let bytes = self.inner.to_png().map_err(to_py_err)?;
        Ok(pyo3::types::PyBytes::new(py, &bytes))
    }

    /// Encode as PNG and write to `path`.
    fn save_png(&self, path: std::path::PathBuf) -> PyResult<()> {
        self.inner.save_png(&path).map_err(to_py_err)
    }

    fn __repr__(&self) -> String {
        format!(
            "Screenshot(width={}, height={}, scale={})",
            self.width, self.height, self.scale,
        )
    }
}

/// Capture pixels from the screen.
///
/// With no arguments, captures the full primary display. Pass `element=` to
/// capture the pixels under an element's current bounds, or `region=(x, y,
/// width, height)` to capture an explicit rectangle in logical screen
/// coordinates.
///
/// Raises `ValueError` if both `element` and `region` are given.
#[pyfunction]
#[pyo3(signature = (*, element=None, region=None))]
fn screenshot(
    py: Python<'_>,
    element: Option<&Element>,
    region: Option<(i32, i32, u32, u32)>,
) -> PyResult<Screenshot> {
    if element.is_some() && region.is_some() {
        return Err(PyValueError::new_err(
            "screenshot: pass either `element` or `region`, not both",
        ));
    }

    let shot = if let Some(element) = element {
        let el = xa11y::Element::new(element.inner_data.clone(), element.provider.clone());
        py.allow_threads(move || xa11y::screenshot_element(&el))
    } else if let Some((x, y, w, h)) = region {
        let rect = xa11y::Rect {
            x,
            y,
            width: w,
            height: h,
        };
        py.allow_threads(move || xa11y::screenshot_region(rect))
    } else {
        py.allow_threads(xa11y::screenshot)
    }
    .map_err(to_py_err)?;

    Ok(Screenshot {
        width: shot.width,
        height: shot.height,
        scale: shot.scale,
        inner: shot,
    })
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
    m.add_class::<InputSim>()?;
    m.add_class::<Locator>()?;
    m.add_class::<Rect>()?;
    m.add_class::<Screenshot>()?;
    m.add_class::<Subscription>()?;

    register_exception::<XA11yError>(m, "XA11yError")?;
    register_exception::<PermissionDeniedError>(m, "PermissionDeniedError")?;
    register_exception::<AccessibilityNotEnabledError>(m, "AccessibilityNotEnabledError")?;
    register_exception::<SelectorNotMatchedError>(m, "SelectorNotMatchedError")?;
    register_exception::<ActionNotSupportedError>(m, "ActionNotSupportedError")?;
    register_exception::<TimeoutError>(m, "TimeoutError")?;
    register_exception::<InvalidSelectorError>(m, "InvalidSelectorError")?;
    register_exception::<InvalidActionDataError>(m, "InvalidActionDataError")?;
    register_exception::<PlatformError>(m, "PlatformError")?;

    // Module-level locator function (renamed from "locator" to avoid Rust naming conflict)
    m.add_function(wrap_pyfunction!(locator_fn, m)?)?;
    // Re-export as "locator" in Python
    let locator_fn_obj = m.getattr("locator_fn")?;
    m.setattr("locator", &locator_fn_obj)?;

    // Input simulation factory
    m.add_function(wrap_pyfunction!(input_sim, m)?)?;

    // Screenshot entry point
    m.add_function(wrap_pyfunction!(screenshot, m)?)?;

    // CLI entry point
    m.add_function(wrap_pyfunction!(_cli_main, m)?)?;

    // Test helpers
    m.add_function(wrap_pyfunction!(_make_test_locator, m)?)?;
    m.add_function(wrap_pyfunction!(_make_test_app, m)?)?;
    m.add_function(wrap_pyfunction!(_make_disconnected_subscription, m)?)?;
    m.add_function(wrap_pyfunction!(_make_test_action_probe, m)?)?;

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

/// Create a test App backed by the shared mock provider (resolves "TestApp").
#[pyfunction]
fn _make_test_app() -> PyResult<App> {
    let provider = xa11y::mock::build_provider() as Arc<dyn xa11y::Provider>;
    let app = xa11y::App::by_name_with(provider, "TestApp", std::time::Duration::ZERO)
        .map_err(to_py_err)?;
    Ok(App::from_core(app))
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

/// Probe wrapping a single shared mock provider so tests can drive actions
/// from a Python `Element` and then inspect the recorded action log.
#[pyclass]
struct TestActionProbe {
    provider: Arc<xa11y::mock::MockProvider>,
}

#[pymethods]
impl TestActionProbe {
    /// Locator rooted at the mock test app, sharing this probe's provider.
    fn locator(&self, selector: &str) -> Locator {
        Locator {
            inner: xa11y::Locator::new(
                self.provider.clone() as Arc<dyn xa11y::Provider>,
                None,
                selector,
            ),
        }
    }

    /// Recorded action log: list of `(handle, action_name, optional_data)`.
    fn actions(&self, py: Python<'_>) -> PyResult<PyObject> {
        let entries = self.provider.actions();
        let list = PyList::empty(py);
        for (handle, name, data) in entries {
            let tup = PyList::empty(py);
            tup.append(handle)?;
            tup.append(name)?;
            match data {
                Some(s) => tup.append(s)?,
                None => tup.append(py.None())?,
            }
            list.append(tup)?;
        }
        Ok(list.into_any().unbind())
    }

    /// Clear the action log.
    fn clear(&self) {
        self.provider.clear_actions();
    }
}

/// Create a probe over the shared mock provider.
///
/// The returned probe exposes a `locator()` helper plus `actions()` /
/// `clear()` for inspecting and resetting the action log recorded by the
/// mock provider.
#[pyfunction]
fn _make_test_action_probe() -> TestActionProbe {
    TestActionProbe {
        provider: xa11y::mock::build_provider(),
    }
}
