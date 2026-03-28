use std::sync::Arc;
use std::time::Duration;

use pyo3::exceptions::*;
use pyo3::prelude::*;
use pyo3::types::PyList;

// ── Singleton provider ─────────────────────────────────────────────────────

fn get_provider() -> PyResult<Arc<dyn xa11y::Provider>> {
    xa11y::provider().map_err(|e| PlatformError::new_err(format!("{e}")))
}

// ── Exceptions ──────────────────────────────────────────────────────────────

pyo3::create_exception!(_native, XA11yError, PyException);
pyo3::create_exception!(_native, PermissionDeniedError, XA11yError);
pyo3::create_exception!(_native, AppNotFoundError, XA11yError);
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
        xa11y::Error::AppNotFound { target } => {
            AppNotFoundError::new_err(format!("Application not found: {target}"))
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

fn action_to_str(a: &xa11y::Action) -> &'static str {
    match a {
        xa11y::Action::Press => "press",
        xa11y::Action::Focus => "focus",
        xa11y::Action::SetValue => "set_value",
        xa11y::Action::Toggle => "toggle",
        xa11y::Action::Expand => "expand",
        xa11y::Action::Collapse => "collapse",
        xa11y::Action::Select => "select",
        xa11y::Action::ShowMenu => "show_menu",
        xa11y::Action::ScrollIntoView => "scroll_into_view",
        xa11y::Action::Scroll => "scroll",
        xa11y::Action::Increment => "increment",
        xa11y::Action::Decrement => "decrement",
        xa11y::Action::Blur => "blur",
        xa11y::Action::SetTextSelection => "set_text_selection",
        xa11y::Action::TypeText => "type_text",
    }
}

fn parse_scroll_direction(s: &str) -> PyResult<xa11y::ScrollDirection> {
    match s {
        "up" => Ok(xa11y::ScrollDirection::Up),
        "down" => Ok(xa11y::ScrollDirection::Down),
        "left" => Ok(xa11y::ScrollDirection::Left),
        "right" => Ok(xa11y::ScrollDirection::Right),
        _ => Err(PyValueError::new_err(format!(
            "Unknown scroll direction: {s} (expected up/down/left/right)"
        ))),
    }
}

fn build_query_options(
    max_depth: Option<u32>,
    max_elements: Option<u32>,
    visible_only: bool,
    roles: Option<Vec<String>>,
) -> xa11y::QueryOptions {
    xa11y::QueryOptions {
        max_depth,
        max_elements,
        visible_only,
        roles: roles
            .unwrap_or_default()
            .iter()
            .filter_map(|s| xa11y::Role::from_snake_case(s))
            .collect(),
    }
}

fn resolve_app_target(name: Option<&str>, pid: Option<u32>) -> PyResult<xa11y::AppTarget> {
    match (name, pid) {
        (Some(n), _) => Ok(xa11y::AppTarget::ByName(n.to_string())),
        (None, Some(p)) => Ok(xa11y::AppTarget::ByPid(p)),
        (None, None) => Err(PyValueError::new_err("Either name or pid must be provided")),
    }
}

/// Create a standalone Python Node from a Rust NodeData (no tree context).
fn make_py_node(py: Python<'_>, n: &xa11y::NodeData) -> PyResult<Py<Node>> {
    let checked = n.states.checked.map(|t| match t {
        xa11y::Toggled::Off => "off".to_string(),
        xa11y::Toggled::On => "on".to_string(),
        xa11y::Toggled::Mixed => "mixed".to_string(),
    });
    let actions: Vec<String> = n
        .actions
        .iter()
        .map(|a| action_to_str(a).to_string())
        .collect();
    Py::new(
        py,
        Node {
            role: n.role.to_snake_case().to_string(),
            name: n.name.clone(),
            value: n.value.clone(),
            description: n.description.clone(),
            numeric_value: n.numeric_value,
            min_value: n.min_value,
            max_value: n.max_value,
            stable_id: n.stable_id.clone(),
            pid: n.pid,
            actions,
            bounds_data: n.bounds.as_ref().map(|r| (r.x, r.y, r.width, r.height)),
            enabled: n.states.enabled,
            visible: n.states.visible,
            focused: n.states.focused,
            checked,
            selected: n.states.selected,
            expanded: n.states.expanded,
            editable: n.states.editable,
            focusable: n.states.focusable,
            modal: n.states.modal,
            required: n.states.required,
            busy: n.states.busy,
            children_indices: n.children_indices.clone(),
            parent_idx: n.parent_index,
            _index: n.index,
            _all_nodes: None,
            _ctx: None,
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

// ── Node ────────────────────────────────────────────────────────────────────

/// A node in the accessibility tree snapshot.
///
/// Nodes form a navigable graph — use `node.children` and `node.parent`
/// to traverse. Use `node.query()` to search.
#[pyclass]
struct Node {
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

    children_indices: Vec<u32>,
    parent_idx: Option<u32>,
    _index: u32,

    /// Shared reference to all nodes in the tree (for graph navigation).
    _all_nodes: Option<Py<PyList>>,
    /// Shared snapshot context for query/dump/locator (None for standalone nodes).
    _ctx: Option<Arc<SnapshotContext>>,
}

/// Shared context for all nodes in a snapshot — avoids cloning Tree per node.
struct SnapshotContext {
    rust_tree: xa11y::Tree,
    provider: Arc<dyn xa11y::Provider>,
    target: xa11y::AppTarget,
}

#[pymethods]
impl Node {
    #[getter]
    fn children(&self, py: Python<'_>) -> PyResult<Vec<PyObject>> {
        let Some(ref all) = self._all_nodes else {
            return Ok(vec![]);
        };
        let list = all.bind(py);
        self.children_indices
            .iter()
            .map(|&idx| list.get_item(idx as usize).map(|item| item.unbind()))
            .collect()
    }

    #[getter]
    fn parent(&self, py: Python<'_>) -> PyResult<Option<PyObject>> {
        let Some(ref all) = self._all_nodes else {
            return Ok(None);
        };
        match self.parent_idx {
            Some(idx) => Ok(Some(all.bind(py).get_item(idx as usize)?.unbind())),
            None => Ok(None),
        }
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

    /// Query nodes matching a CSS-like selector string within this snapshot.
    fn query(&self, py: Python<'_>, selector: &str) -> PyResult<Vec<PyObject>> {
        let ctx = self._ctx.as_ref().ok_or_else(|| {
            PyValueError::new_err(
                "query() requires a snapshot context (node from app(), not locator.get())",
            )
        })?;
        let Some(ref all) = self._all_nodes else {
            return Err(PyValueError::new_err("No node list available"));
        };
        let matches = ctx.rust_tree.query(selector).map_err(to_py_err)?;
        let list = all.bind(py);
        matches
            .iter()
            .map(|n| list.get_item(n.index as usize).map(|item| item.unbind()))
            .collect()
    }

    /// Create a Locator for lazy element resolution from this snapshot's app.
    #[pyo3(signature = (selector, *, max_depth=None, max_elements=None, visible_only=false, roles=None))]
    fn locator(
        &self,
        selector: &str,
        max_depth: Option<u32>,
        max_elements: Option<u32>,
        visible_only: bool,
        roles: Option<Vec<String>>,
    ) -> PyResult<Locator> {
        let ctx = self._ctx.as_ref().ok_or_else(|| {
            PyValueError::new_err(
                "locator() requires a snapshot context (node from app(), not locator.get())",
            )
        })?;
        let opts = build_query_options(max_depth, max_elements, visible_only, roles);
        Ok(Locator {
            provider: ctx.provider.clone(),
            target: ctx.target.clone(),
            selector: selector.to_string(),
            opts,
            nth: None,
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
        format!("Node({})", parts.join(", "))
    }

    fn __str__(&self) -> String {
        self.__repr__()
    }

    fn __len__(&self) -> usize {
        self.children_indices.len()
    }
}

// ── Node construction ───────────────────────────────────────────────────────

/// Convert a Rust Tree into a fully navigable Python root Node.
///
/// All nodes get shared references so parent/children/query/dump work.
/// Returns the root node (index 0).
fn convert_to_root_node(
    py: Python<'_>,
    rust_tree: xa11y::Tree,
    provider: Arc<dyn xa11y::Provider>,
    target: xa11y::AppTarget,
) -> PyResult<Py<Node>> {
    convert_to_node_at(py, rust_tree, provider, target, 0)
}

/// Convert a Rust Tree into a fully navigable Python Node at the given index.
fn convert_to_node_at(
    py: Python<'_>,
    rust_tree: xa11y::Tree,
    provider: Arc<dyn xa11y::Provider>,
    target: xa11y::AppTarget,
    node_index: usize,
) -> PyResult<Py<Node>> {
    let num_nodes = rust_tree.len();
    let mut py_nodes: Vec<Py<Node>> = Vec::with_capacity(num_nodes);

    for i in 0..num_nodes {
        let n = rust_tree
            .get_data(i as u32)
            .expect("index valid in range 0..len");
        py_nodes.push(make_py_node(py, n)?);
    }

    // Build shared context and node list so every Node can navigate and query.
    let ctx = Arc::new(SnapshotContext {
        rust_tree,
        provider,
        target,
    });
    let all_nodes_list: Py<PyList> = PyList::new(py, &py_nodes)?.unbind();
    for py_node in &py_nodes {
        let mut node = py_node.borrow_mut(py);
        node._all_nodes = Some(all_nodes_list.clone_ref(py));
        node._ctx = Some(ctx.clone());
    }

    py_nodes
        .get(node_index)
        .map(|n| n.clone_ref(py))
        .ok_or_else(|| PyValueError::new_err("Node index out of range"))
}

// ── Locator ─────────────────────────────────────────────────────────────────

#[pyclass]
#[derive(Clone)]
struct Locator {
    provider: Arc<dyn xa11y::Provider>,
    target: xa11y::AppTarget,
    #[pyo3(get)]
    selector: String,
    opts: xa11y::QueryOptions,
    nth: Option<usize>,
}

#[pymethods]
impl Locator {
    fn nth(&self, n: usize) -> Self {
        let mut loc = self.clone();
        loc.nth = Some(n);
        loc
    }

    fn first(&self) -> Self {
        self.nth(0)
    }

    fn child(&self, selector: &str) -> Self {
        let mut loc = self.clone();
        loc.selector = format!("{} > {}", self.selector, selector);
        loc.nth = None;
        loc
    }

    fn descendant(&self, selector: &str) -> Self {
        let mut loc = self.clone();
        loc.selector = format!("{} {}", self.selector, selector);
        loc.nth = None;
        loc
    }

    // ── Queries ──

    fn role(&self) -> PyResult<String> {
        Ok(self.resolve_node_data()?.role.to_snake_case().to_string())
    }

    fn name(&self) -> PyResult<Option<String>> {
        Ok(self.resolve_node_data()?.name.clone())
    }

    fn value(&self) -> PyResult<Option<String>> {
        Ok(self.resolve_node_data()?.value.clone())
    }

    fn description(&self) -> PyResult<Option<String>> {
        Ok(self.resolve_node_data()?.description.clone())
    }

    fn bounds(&self) -> PyResult<Option<Rect>> {
        Ok(self.resolve_node_data()?.bounds.as_ref().map(|r| Rect {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        }))
    }

    fn numeric_value(&self) -> PyResult<Option<f64>> {
        Ok(self.resolve_node_data()?.numeric_value)
    }

    fn is_visible(&self) -> PyResult<bool> {
        Ok(self.resolve_node_data()?.states.visible)
    }

    fn is_enabled(&self) -> PyResult<bool> {
        Ok(self.resolve_node_data()?.states.enabled)
    }

    fn is_focused(&self) -> PyResult<bool> {
        Ok(self.resolve_node_data()?.states.focused)
    }

    fn is_selected(&self) -> PyResult<bool> {
        Ok(self.resolve_node_data()?.states.selected)
    }

    fn checked(&self) -> PyResult<Option<String>> {
        Ok(self.resolve_node_data()?.states.checked.map(|t| match t {
            xa11y::Toggled::Off => "off".to_string(),
            xa11y::Toggled::On => "on".to_string(),
            xa11y::Toggled::Mixed => "mixed".to_string(),
        }))
    }

    fn is_expanded(&self) -> PyResult<Option<bool>> {
        Ok(self.resolve_node_data()?.states.expanded)
    }

    fn is_editable(&self) -> PyResult<bool> {
        Ok(self.resolve_node_data()?.states.editable)
    }

    fn is_focusable(&self) -> PyResult<bool> {
        Ok(self.resolve_node_data()?.states.focusable)
    }

    fn is_modal(&self) -> PyResult<bool> {
        Ok(self.resolve_node_data()?.states.modal)
    }

    fn is_required(&self) -> PyResult<bool> {
        Ok(self.resolve_node_data()?.states.required)
    }

    fn is_busy(&self) -> PyResult<bool> {
        Ok(self.resolve_node_data()?.states.busy)
    }

    fn exists(&self) -> PyResult<bool> {
        match self.resolve() {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    fn count(&self) -> PyResult<usize> {
        let tree = self
            .provider
            .get_app_tree(&self.target, &self.opts)
            .map_err(to_py_err)?;
        let matches = tree.query(&self.selector).map_err(to_py_err)?;
        Ok(matches.len())
    }

    /// Get a snapshot of the matched node (with full tree context for navigation).
    fn get(&self, py: Python<'_>) -> PyResult<Py<Node>> {
        let (tree, node_index) = self.resolve()?;
        convert_to_node_at(
            py,
            tree,
            self.provider.clone(),
            self.target.clone(),
            node_index as usize,
        )
    }

    // ── Actions ──

    fn press(&self) -> PyResult<()> {
        self.perform_action(xa11y::Action::Press, None)
    }

    fn focus(&self) -> PyResult<()> {
        self.perform_action(xa11y::Action::Focus, None)
    }

    fn blur(&self) -> PyResult<()> {
        self.perform_action(xa11y::Action::Blur, None)
    }

    fn toggle(&self) -> PyResult<()> {
        self.perform_action(xa11y::Action::Toggle, None)
    }

    fn expand(&self) -> PyResult<()> {
        self.perform_action(xa11y::Action::Expand, None)
    }

    fn collapse(&self) -> PyResult<()> {
        self.perform_action(xa11y::Action::Collapse, None)
    }

    fn select_item(&self) -> PyResult<()> {
        self.perform_action(xa11y::Action::Select, None)
    }

    fn show_menu(&self) -> PyResult<()> {
        self.perform_action(xa11y::Action::ShowMenu, None)
    }

    fn scroll_into_view(&self) -> PyResult<()> {
        self.perform_action(xa11y::Action::ScrollIntoView, None)
    }

    fn increment(&self) -> PyResult<()> {
        self.perform_action(xa11y::Action::Increment, None)
    }

    fn decrement(&self) -> PyResult<()> {
        self.perform_action(xa11y::Action::Decrement, None)
    }

    fn set_value(&self, value: &str) -> PyResult<()> {
        self.perform_action(
            xa11y::Action::SetValue,
            Some(xa11y::ActionData::Value(value.to_string())),
        )
    }

    fn set_numeric_value(&self, value: f64) -> PyResult<()> {
        self.perform_action(
            xa11y::Action::SetValue,
            Some(xa11y::ActionData::NumericValue(value)),
        )
    }

    fn type_text(&self, text: &str) -> PyResult<()> {
        self.perform_action(
            xa11y::Action::TypeText,
            Some(xa11y::ActionData::Value(text.to_string())),
        )
    }

    fn select_text(&self, start: u32, end: u32) -> PyResult<()> {
        self.perform_action(
            xa11y::Action::SetTextSelection,
            Some(xa11y::ActionData::TextSelection { start, end }),
        )
    }

    #[pyo3(signature = (direction, amount=1.0))]
    fn scroll(&self, direction: &str, amount: f64) -> PyResult<()> {
        let dir = parse_scroll_direction(direction)?;
        self.perform_action(
            xa11y::Action::Scroll,
            Some(xa11y::ActionData::ScrollAmount {
                direction: dir,
                amount,
            }),
        )
    }

    // ── Wait operations ──

    #[pyo3(signature = (timeout=5.0))]
    fn wait_visible(&self, timeout: f64) -> PyResult<()> {
        self.poll_state(WaitState::Visible, Duration::from_secs_f64(timeout))
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_attached(&self, timeout: f64) -> PyResult<()> {
        self.poll_state(WaitState::Attached, Duration::from_secs_f64(timeout))
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_detached(&self, timeout: f64) -> PyResult<()> {
        self.poll_state(WaitState::Detached, Duration::from_secs_f64(timeout))
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_enabled(&self, timeout: f64) -> PyResult<()> {
        self.poll_state(WaitState::Enabled, Duration::from_secs_f64(timeout))
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_hidden(&self, timeout: f64) -> PyResult<()> {
        self.poll_state(WaitState::Hidden, Duration::from_secs_f64(timeout))
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_disabled(&self, timeout: f64) -> PyResult<()> {
        self.poll_state(WaitState::Disabled, Duration::from_secs_f64(timeout))
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_focused(&self, timeout: f64) -> PyResult<()> {
        self.poll_state(WaitState::Focused, Duration::from_secs_f64(timeout))
    }

    #[pyo3(signature = (timeout=5.0))]
    fn wait_unfocused(&self, timeout: f64) -> PyResult<()> {
        self.poll_state(WaitState::Unfocused, Duration::from_secs_f64(timeout))
    }

    /// Wait until an arbitrary predicate is satisfied.
    ///
    /// The callback receives a dict with the node's properties when the element
    /// exists, or ``None`` when no element matches the selector. Return ``True``
    /// to stop waiting.
    ///
    /// Example::
    ///
    ///     locator.wait_until(lambda n: n is not None and n["value"] == "Done")
    #[pyo3(signature = (predicate, timeout=5.0))]
    fn wait_until(&self, predicate: PyObject, timeout: f64) -> PyResult<()> {
        self.poll_predicate(predicate, Duration::from_secs_f64(timeout))
    }

    fn __repr__(&self) -> String {
        format!("Locator(selector='{}')", self.selector)
    }
}

enum WaitState {
    Attached,
    Detached,
    Visible,
    Hidden,
    Enabled,
    Disabled,
    Focused,
    Unfocused,
}

impl Locator {
    fn resolve(&self) -> PyResult<(xa11y::Tree, u32)> {
        let tree = self
            .provider
            .get_app_tree(&self.target, &self.opts)
            .map_err(to_py_err)?;
        let matches = tree.query(&self.selector).map_err(to_py_err)?;
        let idx = self.nth.unwrap_or(0);
        let node = matches.get(idx).ok_or_else(|| {
            to_py_err(xa11y::Error::SelectorNotMatched {
                selector: self.selector.clone(),
            })
        })?;
        let node_index = node.index;
        Ok((tree, node_index))
    }

    fn resolve_node_data(&self) -> PyResult<xa11y::NodeData> {
        let (tree, idx) = self.resolve()?;
        Ok(tree.get_data(idx).expect("valid after resolve").clone())
    }

    fn perform_action(
        &self,
        action: xa11y::Action,
        data: Option<xa11y::ActionData>,
    ) -> PyResult<()> {
        if let Some(ref d) = data {
            d.validate(action).map_err(to_py_err)?;
        }
        let (tree, node_index) = self.resolve()?;
        let node = tree.get_data(node_index).expect("valid after resolve");
        self.provider
            .perform_action(&tree, node, action, data)
            .map_err(to_py_err)
    }

    fn poll_predicate(&self, predicate: PyObject, timeout: Duration) -> PyResult<()> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(100);

        loop {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(to_py_err(xa11y::Error::Timeout { elapsed }));
            }

            let node: Option<xa11y::NodeData> = (|| {
                let tree = self.provider.get_app_tree(&self.target, &self.opts).ok()?;
                let matches = tree.query(&self.selector).ok()?;
                let idx = self.nth.unwrap_or(0);
                matches.get(idx).copied().cloned()
            })();

            let met = Python::with_gil(|py| -> PyResult<bool> {
                let arg: PyObject = match node.as_ref() {
                    Some(n) => make_py_node(py, n)?.into_any(),
                    None => py.None(),
                };
                let result = predicate.call1(py, (arg,))?;
                result.extract::<bool>(py)
            })?;

            if met {
                return Ok(());
            }

            std::thread::sleep(poll_interval);
        }
    }

    fn poll_state(&self, state: WaitState, timeout: Duration) -> PyResult<()> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(100);

        loop {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(to_py_err(xa11y::Error::Timeout { elapsed }));
            }

            let tree_result = self.provider.get_app_tree(&self.target, &self.opts);
            let states = tree_result.ok().and_then(|tree| {
                tree.query(&self.selector).ok().and_then(|matches| {
                    let idx = self.nth.unwrap_or(0);
                    matches.get(idx).map(|n| n.states.clone())
                })
            });

            let met = match state {
                WaitState::Attached => states.is_some(),
                WaitState::Detached => states.is_none(),
                WaitState::Visible => states.as_ref().is_some_and(|s| s.visible),
                WaitState::Hidden => {
                    states.is_none() || states.as_ref().is_some_and(|s| !s.visible)
                }
                WaitState::Enabled => states.as_ref().is_some_and(|s| s.enabled),
                WaitState::Disabled => states.as_ref().is_some_and(|s| !s.enabled),
                WaitState::Focused => states.as_ref().is_some_and(|s| s.focused),
                WaitState::Unfocused => states.as_ref().is_some_and(|s| !s.focused),
            };

            if met {
                return Ok(());
            }

            std::thread::sleep(poll_interval);
        }
    }
}

// ── Module-level functions ──────────────────────────────────────────────────

/// Snapshot an app's accessibility tree and return the root Node.
#[pyfunction]
#[pyo3(signature = (name=None, *, pid=None, max_depth=None, max_elements=None, visible_only=false, roles=None))]
fn app(
    py: Python<'_>,
    name: Option<&str>,
    pid: Option<u32>,
    max_depth: Option<u32>,
    max_elements: Option<u32>,
    visible_only: bool,
    roles: Option<Vec<String>>,
) -> PyResult<Py<Node>> {
    let provider = get_provider()?;
    let target = resolve_app_target(name, pid)?;
    let opts = build_query_options(max_depth, max_elements, visible_only, roles);
    let p = provider.clone();
    let rust_tree = py
        .allow_threads(|| p.get_app_tree(&target, &opts))
        .map_err(to_py_err)?;
    convert_to_root_node(py, rust_tree, provider, target)
}

/// Snapshot all running apps and return the root Node.
#[pyfunction]
#[pyo3(signature = (*, max_depth=None, max_elements=None, visible_only=false, roles=None))]
fn apps(
    py: Python<'_>,
    max_depth: Option<u32>,
    max_elements: Option<u32>,
    visible_only: bool,
    roles: Option<Vec<String>>,
) -> PyResult<Py<Node>> {
    let provider = get_provider()?;
    let opts = build_query_options(max_depth, max_elements, visible_only, roles);
    let p = provider.clone();
    let rust_tree = py.allow_threads(|| p.get_apps(&opts)).map_err(to_py_err)?;
    let target = xa11y::AppTarget::ByName(String::new());
    convert_to_root_node(py, rust_tree, provider, target)
}

/// Create a Locator for lazy element resolution.
#[pyfunction]
#[pyo3(signature = (name=None, *, pid=None, selector, max_depth=None, max_elements=None, visible_only=false, roles=None))]
fn locator(
    name: Option<&str>,
    pid: Option<u32>,
    selector: &str,
    max_depth: Option<u32>,
    max_elements: Option<u32>,
    visible_only: bool,
    roles: Option<Vec<String>>,
) -> PyResult<Locator> {
    let provider = get_provider()?;
    let target = resolve_app_target(name, pid)?;
    let opts = build_query_options(max_depth, max_elements, visible_only, roles);
    Ok(Locator {
        provider,
        target,
        selector: selector.to_string(),
        opts,
        nth: None,
    })
}

/// Check accessibility permissions. Returns "granted" or raises PermissionDeniedError.
#[pyfunction]
fn check_permissions(py: Python<'_>) -> PyResult<String> {
    let provider = get_provider()?;
    let status = py
        .allow_threads(|| provider.check_permissions())
        .map_err(to_py_err)?;
    match status {
        xa11y::PermissionStatus::Granted => Ok("granted".to_string()),
        xa11y::PermissionStatus::Denied { instructions } => {
            Err(PermissionDeniedError::new_err(instructions))
        }
    }
}

// ── Module definition ───────────────────────────────────────────────────────

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Node>()?;
    m.add_class::<Locator>()?;
    m.add_class::<Rect>()?;

    m.add("XA11yError", m.py().get_type::<XA11yError>())?;
    m.add(
        "PermissionDeniedError",
        m.py().get_type::<PermissionDeniedError>(),
    )?;
    m.add("AppNotFoundError", m.py().get_type::<AppNotFoundError>())?;
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

    m.add_function(wrap_pyfunction!(app, m)?)?;
    m.add_function(wrap_pyfunction!(apps, m)?)?;
    m.add_function(wrap_pyfunction!(locator, m)?)?;
    m.add_function(wrap_pyfunction!(check_permissions, m)?)?;

    // Test helpers
    m.add_function(wrap_pyfunction!(_make_test_tree, m)?)?;

    Ok(())
}

// ── Test helpers (exposed to Python for unit testing) ────────────────────────

/// A mock provider that returns canned trees and records performed actions.
struct MockProvider {
    tree: xa11y::Tree,
    /// Records (node_index, action, data_debug) for each perform_action call
    actions: std::sync::Mutex<Vec<(u32, String, Option<String>)>>,
}

impl xa11y::Provider for MockProvider {
    fn get_app_tree(
        &self,
        _target: &xa11y::AppTarget,
        _opts: &xa11y::QueryOptions,
    ) -> xa11y::Result<xa11y::Tree> {
        Ok(self.tree.clone())
    }

    fn get_apps(&self, _opts: &xa11y::QueryOptions) -> xa11y::Result<xa11y::Tree> {
        Ok(self.tree.clone())
    }

    fn perform_action(
        &self,
        _tree: &xa11y::Tree,
        node: &xa11y::NodeData,
        action: xa11y::Action,
        data: Option<xa11y::ActionData>,
    ) -> xa11y::Result<()> {
        let data_debug = data.map(|d| format!("{d:?}"));
        self.actions
            .lock()
            .unwrap()
            .push((node.index, format!("{action}"), data_debug));
        Ok(())
    }

    fn check_permissions(&self) -> xa11y::Result<xa11y::PermissionStatus> {
        Ok(xa11y::PermissionStatus::Granted)
    }
}

/// Build the canonical test tree used by all Python unit tests.
///
/// Structure:
/// ```text
/// [0] application "TestApp"
///   [1] window "Main Window"
///     [2] toolbar "Navigation"
///       [3] button "Back"           (enabled, visible, actions=[press,focus])
///       [4] button "Forward"        (enabled=false, visible, actions=[press,focus])
///     [5] group "Content"
///       [6] text_field "Search"     (editable, focusable, value="hello", actions=[focus,set_value,type_text])
///       [7] check_box "Agree"       (checked=on, actions=[toggle,focus])
///       [8] slider "Volume"         (numeric_value=75, min=0, max=100, actions=[increment,decrement,set_value,focus])
///       [9] static_text "Status"    (visible=false)
///       [10] list "Items"
///         [11] list_item "Item 1"   (selected)
///         [12] list_item "Item 2"
/// ```
fn build_test_tree() -> xa11y::Tree {
    use xa11y::*;

    let nodes = vec![
        // [0] application "TestApp"
        NodeData {
            role: Role::Application,
            name: Some("TestApp".to_string()),
            value: None,
            description: Some("Test application".to_string()),
            bounds: Some(Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }),

            actions: vec![],
            states: StateSet::default(),

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: Some("app-root".to_string()),
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 0,
            children_indices: vec![1],
            parent_index: None,
        },
        // [1] window "Main Window"
        NodeData {
            role: Role::Window,
            name: Some("Main Window".to_string()),
            value: None,
            description: None,
            bounds: Some(Rect {
                x: 100,
                y: 50,
                width: 800,
                height: 600,
            }),

            actions: vec![],
            states: StateSet {
                focused: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 1,
            children_indices: vec![2, 5],
            parent_index: Some(0),
        },
        // [2] toolbar "Navigation"
        NodeData {
            role: Role::Toolbar,
            name: Some("Navigation".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![],
            states: StateSet::default(),

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 2,
            children_indices: vec![3, 4],
            parent_index: Some(1),
        },
        // [3] button "Back"
        NodeData {
            role: Role::Button,
            name: Some("Back".to_string()),
            value: None,
            description: Some("Go back".to_string()),
            bounds: Some(Rect {
                x: 110,
                y: 60,
                width: 50,
                height: 30,
            }),

            actions: vec![xa11y::Action::Press, xa11y::Action::Focus],
            states: StateSet {
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: Some("btn-back".to_string()),
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 3,
            children_indices: vec![],
            parent_index: Some(2),
        },
        // [4] button "Forward" (disabled)
        NodeData {
            role: Role::Button,
            name: Some("Forward".to_string()),
            value: None,
            description: None,
            bounds: Some(Rect {
                x: 170,
                y: 60,
                width: 50,
                height: 30,
            }),

            actions: vec![xa11y::Action::Press, xa11y::Action::Focus],
            states: StateSet {
                enabled: false,
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 4,
            children_indices: vec![],
            parent_index: Some(2),
        },
        // [5] group "Content"
        NodeData {
            role: Role::Group,
            name: Some("Content".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![],
            states: StateSet::default(),

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 5,
            children_indices: vec![6, 7, 8, 9, 10],
            parent_index: Some(1),
        },
        // [6] text_field "Search"
        NodeData {
            role: Role::TextField,
            name: Some("Search".to_string()),
            value: Some("hello".to_string()),
            description: Some("Search field".to_string()),
            bounds: Some(Rect {
                x: 200,
                y: 120,
                width: 300,
                height: 25,
            }),

            actions: vec![
                xa11y::Action::Focus,
                xa11y::Action::SetValue,
                xa11y::Action::TypeText,
            ],
            states: StateSet {
                editable: true,
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 6,
            children_indices: vec![],
            parent_index: Some(5),
        },
        // [7] check_box "Agree" (checked=on)
        NodeData {
            role: Role::CheckBox,
            name: Some("Agree".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![xa11y::Action::Toggle, xa11y::Action::Focus],
            states: StateSet {
                checked: Some(Toggled::On),
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 7,
            children_indices: vec![],
            parent_index: Some(5),
        },
        // [8] slider "Volume"
        NodeData {
            role: Role::Slider,
            name: Some("Volume".to_string()),
            value: Some("75".to_string()),
            description: None,
            bounds: None,

            actions: vec![
                xa11y::Action::Increment,
                xa11y::Action::Decrement,
                xa11y::Action::SetValue,
                xa11y::Action::Focus,
            ],
            states: StateSet {
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: Some(75.0),
            min_value: Some(0.0),
            max_value: Some(100.0),
            stable_id: None,
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 8,
            children_indices: vec![],
            parent_index: Some(5),
        },
        // [9] static_text "Status" (hidden)
        NodeData {
            role: Role::StaticText,
            name: Some("Status".to_string()),
            value: Some("Loading...".to_string()),
            description: None,
            bounds: None,

            actions: vec![],
            states: StateSet {
                visible: false,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 9,
            children_indices: vec![],
            parent_index: Some(5),
        },
        // [10] list "Items"
        NodeData {
            role: Role::List,
            name: Some("Items".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![],
            states: StateSet {
                expanded: Some(true),
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 10,
            children_indices: vec![11, 12],
            parent_index: Some(5),
        },
        // [11] list_item "Item 1" (selected)
        NodeData {
            role: Role::ListItem,
            name: Some("Item 1".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![xa11y::Action::Select, xa11y::Action::Focus],
            states: StateSet {
                selected: true,
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 11,
            children_indices: vec![],
            parent_index: Some(10),
        },
        // [12] list_item "Item 2"
        NodeData {
            role: Role::ListItem,
            name: Some("Item 2".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![xa11y::Action::Select, xa11y::Action::Focus],
            states: StateSet {
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 12,
            children_indices: vec![],
            parent_index: Some(10),
        },
    ];

    Tree::new("TestApp".to_string(), Some(1234), (1920, 1080), nodes)
}

/// Create a test tree (for Python unit tests). Returns the root Node backed by a mock provider.
#[pyfunction]
fn _make_test_tree(py: Python<'_>) -> PyResult<Py<Node>> {
    let tree = build_test_tree();
    let provider: Arc<dyn xa11y::Provider> = Arc::new(MockProvider {
        tree: tree.clone(),
        actions: std::sync::Mutex::new(Vec::new()),
    });
    let target = xa11y::AppTarget::ByName("TestApp".to_string());
    convert_to_root_node(py, tree, provider, target)
}
