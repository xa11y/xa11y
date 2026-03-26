use std::sync::{Arc, OnceLock};
use std::time::Duration;

use pyo3::exceptions::*;
use pyo3::prelude::*;
use pyo3::types::PyList;

// ── Singleton provider ─────────────────────────────────────────────────────

static PROVIDER: OnceLock<Result<Arc<dyn xa11y::Provider>, String>> = OnceLock::new();

fn get_provider() -> PyResult<Arc<dyn xa11y::Provider>> {
    PROVIDER
        .get_or_init(|| {
            xa11y::create_provider()
                .map(Arc::from)
                .map_err(|e| format!("{e}"))
        })
        .as_ref()
        .map(Arc::clone)
        .map_err(|msg| PlatformError::new_err(msg.clone()))
}

static EVENT_PROVIDER: OnceLock<Result<Arc<dyn xa11y::EventProvider>, String>> = OnceLock::new();

fn get_event_provider() -> PyResult<Arc<dyn xa11y::EventProvider>> {
    EVENT_PROVIDER
        .get_or_init(|| {
            xa11y::create_event_provider()
                .map(Arc::from)
                .map_err(|e| format!("{e}"))
        })
        .as_ref()
        .map(Arc::clone)
        .map_err(|msg| PlatformError::new_err(msg.clone()))
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

fn parse_action(s: &str) -> PyResult<xa11y::Action> {
    match s {
        "press" => Ok(xa11y::Action::Press),
        "focus" => Ok(xa11y::Action::Focus),
        "set_value" => Ok(xa11y::Action::SetValue),
        "toggle" => Ok(xa11y::Action::Toggle),
        "expand" => Ok(xa11y::Action::Expand),
        "collapse" => Ok(xa11y::Action::Collapse),
        "select" => Ok(xa11y::Action::Select),
        "show_menu" => Ok(xa11y::Action::ShowMenu),
        "scroll_into_view" => Ok(xa11y::Action::ScrollIntoView),
        "scroll" => Ok(xa11y::Action::Scroll),
        "increment" => Ok(xa11y::Action::Increment),
        "decrement" => Ok(xa11y::Action::Decrement),
        "blur" => Ok(xa11y::Action::Blur),
        "set_text_selection" => Ok(xa11y::Action::SetTextSelection),
        "type_text" => Ok(xa11y::Action::TypeText),
        _ => Err(PyValueError::new_err(format!("Unknown action: {s}"))),
    }
}

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
        roles: roles.map(|rs| {
            rs.iter()
                .filter_map(|s| xa11y::Role::from_snake_case(s))
                .collect()
        }),
    }
}

fn resolve_app_target(name: Option<&str>, pid: Option<u32>) -> PyResult<xa11y::AppTarget> {
    match (name, pid) {
        (Some(n), _) => Ok(xa11y::AppTarget::ByName(n.to_string())),
        (None, Some(p)) => Ok(xa11y::AppTarget::ByPid(p)),
        (None, None) => Err(PyValueError::new_err("Either name or pid must be provided")),
    }
}

fn build_action_data(
    action: xa11y::Action,
    value: Option<String>,
    numeric_value: Option<f64>,
    direction: Option<String>,
    amount: Option<f64>,
    start: Option<u32>,
    end: Option<u32>,
) -> PyResult<Option<xa11y::ActionData>> {
    let data = match action {
        xa11y::Action::SetValue => match (value, numeric_value) {
            (Some(v), _) => Some(xa11y::ActionData::Value(v)),
            (_, Some(n)) => Some(xa11y::ActionData::NumericValue(n)),
            _ => None,
        },
        xa11y::Action::TypeText => value.map(xa11y::ActionData::Value),
        xa11y::Action::Scroll => {
            let dir = direction
                .as_deref()
                .ok_or_else(|| PyValueError::new_err("scroll requires direction"))?;
            Some(xa11y::ActionData::ScrollAmount {
                direction: parse_scroll_direction(dir)?,
                amount: amount.unwrap_or(1.0),
            })
        }
        xa11y::Action::SetTextSelection => {
            let s =
                start.ok_or_else(|| PyValueError::new_err("set_text_selection requires start"))?;
            let e = end.ok_or_else(|| PyValueError::new_err("set_text_selection requires end"))?;
            Some(xa11y::ActionData::TextSelection { start: s, end: e })
        }
        _ => None,
    };
    if let Some(ref d) = data {
        d.validate(action).map_err(to_py_err)?;
    }
    Ok(data)
}

/// Create a Python Node from a Rust Node. No tree back-reference.
fn make_py_node(py: Python<'_>, n: &xa11y::Node) -> PyResult<Py<Node>> {
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

#[pyclass(frozen)]
#[derive(Clone)]
struct AppInfo {
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    pid: u32,
    #[pyo3(get)]
    bundle_id: Option<String>,
}

#[pymethods]
impl AppInfo {
    fn __repr__(&self) -> String {
        match &self.bundle_id {
            Some(bid) => format!(
                "AppInfo(name='{}', pid={}, bundle_id='{}')",
                self.name, self.pid, bid
            ),
            None => format!("AppInfo(name='{}', pid={})", self.name, self.pid),
        }
    }
}

// ── Node ────────────────────────────────────────────────────────────────────

/// A node in the accessibility tree. Nodes form a navigable graph —
/// use `node.children` and `node.parent` to traverse.
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

// ── Tree ────────────────────────────────────────────────────────────────────

/// A snapshot of an application's accessibility tree.
///
/// Nodes are stored in DFS order. Navigation (children, parent) and
/// queries use the internal Rust selector engine.
#[pyclass]
struct Tree {
    #[pyo3(get)]
    app_name: String,
    #[pyo3(get)]
    pid: Option<u32>,
    #[pyo3(get)]
    screen_size: (u32, u32),

    /// All Python Node objects, indexed by Rust NodeIndex.
    /// We store them to reuse objects across repeated queries.
    py_nodes: Vec<Py<Node>>,

    rust_tree: xa11y::Tree,
    provider: Arc<dyn xa11y::Provider>,
    target: xa11y::AppTarget,
    _opts: xa11y::QueryOptions,
}

#[pymethods]
impl Tree {
    #[getter]
    fn root(&self, py: Python<'_>) -> PyResult<Py<Node>> {
        self.py_nodes
            .first()
            .map(|n| n.clone_ref(py))
            .ok_or_else(|| PyValueError::new_err("Tree has no nodes"))
    }

    /// Query nodes matching a CSS-like selector string.
    fn query(&self, py: Python<'_>, selector: &str) -> PyResult<Vec<Py<Node>>> {
        let matches = self.rust_tree.query(selector).map_err(to_py_err)?;
        Ok(matches
            .iter()
            .filter_map(|n| self.py_nodes.get(n.index as usize).map(|p| p.clone_ref(py)))
            .collect())
    }

    /// Find nodes by role name.
    fn find_by_role(&self, py: Python<'_>, role: &str) -> PyResult<Vec<Py<Node>>> {
        let rust_role = xa11y::Role::from_snake_case(role)
            .ok_or_else(|| PyValueError::new_err(format!("Unknown role: {role}")))?;
        Ok(self
            .rust_tree
            .find_by_role(rust_role)
            .iter()
            .filter_map(|n| self.py_nodes.get(n.index as usize).map(|p| p.clone_ref(py)))
            .collect())
    }

    /// Find nodes by name (substring, case-insensitive).
    fn find_by_name(&self, py: Python<'_>, pattern: &str) -> Vec<Py<Node>> {
        self.rust_tree
            .find_by_name(pattern)
            .iter()
            .filter_map(|n| self.py_nodes.get(n.index as usize).map(|p| p.clone_ref(py)))
            .collect()
    }

    /// Perform an action on a node or selector target.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (target, action, *, value=None, numeric_value=None, direction=None, amount=None, start=None, end=None))]
    fn perform(
        &self,
        target: &Bound<'_, PyAny>,
        action: &str,
        value: Option<String>,
        numeric_value: Option<f64>,
        direction: Option<String>,
        amount: Option<f64>,
        start: Option<u32>,
        end: Option<u32>,
    ) -> PyResult<()> {
        let rust_action = parse_action(action)?;
        let data = build_action_data(
            rust_action,
            value,
            numeric_value,
            direction,
            amount,
            start,
            end,
        )?;
        let node_index = self.resolve_target_index(target)?;
        let rust_node = self
            .rust_tree
            .get(node_index)
            .ok_or_else(|| PyValueError::new_err("Invalid node reference"))?;
        self.provider
            .perform_action(&self.rust_tree, rust_node, rust_action, data)
            .map_err(to_py_err)
    }

    // ── Action convenience methods ──

    fn press(&self, target: &Bound<'_, PyAny>) -> PyResult<()> {
        self.perform_simple(target, xa11y::Action::Press)
    }

    fn focus(&self, target: &Bound<'_, PyAny>) -> PyResult<()> {
        self.perform_simple(target, xa11y::Action::Focus)
    }

    fn blur(&self, target: &Bound<'_, PyAny>) -> PyResult<()> {
        self.perform_simple(target, xa11y::Action::Blur)
    }

    fn toggle(&self, target: &Bound<'_, PyAny>) -> PyResult<()> {
        self.perform_simple(target, xa11y::Action::Toggle)
    }

    fn expand(&self, target: &Bound<'_, PyAny>) -> PyResult<()> {
        self.perform_simple(target, xa11y::Action::Expand)
    }

    fn collapse(&self, target: &Bound<'_, PyAny>) -> PyResult<()> {
        self.perform_simple(target, xa11y::Action::Collapse)
    }

    fn select(&self, target: &Bound<'_, PyAny>) -> PyResult<()> {
        self.perform_simple(target, xa11y::Action::Select)
    }

    fn increment(&self, target: &Bound<'_, PyAny>) -> PyResult<()> {
        self.perform_simple(target, xa11y::Action::Increment)
    }

    fn decrement(&self, target: &Bound<'_, PyAny>) -> PyResult<()> {
        self.perform_simple(target, xa11y::Action::Decrement)
    }

    fn show_menu(&self, target: &Bound<'_, PyAny>) -> PyResult<()> {
        self.perform_simple(target, xa11y::Action::ShowMenu)
    }

    fn scroll_into_view(&self, target: &Bound<'_, PyAny>) -> PyResult<()> {
        self.perform_simple(target, xa11y::Action::ScrollIntoView)
    }

    fn set_value(&self, target: &Bound<'_, PyAny>, value: &str) -> PyResult<()> {
        let node_index = self.resolve_target_index(target)?;
        let rust_node = self
            .rust_tree
            .get(node_index)
            .ok_or_else(|| PyValueError::new_err("Invalid node reference"))?;
        self.provider
            .perform_action(
                &self.rust_tree,
                rust_node,
                xa11y::Action::SetValue,
                Some(xa11y::ActionData::Value(value.to_string())),
            )
            .map_err(to_py_err)
    }

    fn set_numeric_value(&self, target: &Bound<'_, PyAny>, value: f64) -> PyResult<()> {
        let node_index = self.resolve_target_index(target)?;
        let rust_node = self
            .rust_tree
            .get(node_index)
            .ok_or_else(|| PyValueError::new_err("Invalid node reference"))?;
        self.provider
            .perform_action(
                &self.rust_tree,
                rust_node,
                xa11y::Action::SetValue,
                Some(xa11y::ActionData::NumericValue(value)),
            )
            .map_err(to_py_err)
    }

    fn type_text(&self, target: &Bound<'_, PyAny>, text: &str) -> PyResult<()> {
        let node_index = self.resolve_target_index(target)?;
        let rust_node = self
            .rust_tree
            .get(node_index)
            .ok_or_else(|| PyValueError::new_err("Invalid node reference"))?;
        self.provider
            .perform_action(
                &self.rust_tree,
                rust_node,
                xa11y::Action::TypeText,
                Some(xa11y::ActionData::Value(text.to_string())),
            )
            .map_err(to_py_err)
    }

    #[pyo3(signature = (target, direction, amount=1.0))]
    fn scroll(&self, target: &Bound<'_, PyAny>, direction: &str, amount: f64) -> PyResult<()> {
        let dir = parse_scroll_direction(direction)?;
        let node_index = self.resolve_target_index(target)?;
        let rust_node = self
            .rust_tree
            .get(node_index)
            .ok_or_else(|| PyValueError::new_err("Invalid node reference"))?;
        self.provider
            .perform_action(
                &self.rust_tree,
                rust_node,
                xa11y::Action::Scroll,
                Some(xa11y::ActionData::ScrollAmount {
                    direction: dir,
                    amount,
                }),
            )
            .map_err(to_py_err)
    }

    fn select_text(&self, target: &Bound<'_, PyAny>, start: u32, end: u32) -> PyResult<()> {
        let node_index = self.resolve_target_index(target)?;
        let rust_node = self
            .rust_tree
            .get(node_index)
            .ok_or_else(|| PyValueError::new_err("Invalid node reference"))?;
        self.provider
            .perform_action(
                &self.rust_tree,
                rust_node,
                xa11y::Action::SetTextSelection,
                Some(xa11y::ActionData::TextSelection { start, end }),
            )
            .map_err(to_py_err)
    }

    // ── Locator factory ──

    #[pyo3(signature = (selector, *, max_depth=None, max_elements=None, visible_only=false, roles=None))]
    fn locator(
        &self,
        selector: &str,
        max_depth: Option<u32>,
        max_elements: Option<u32>,
        visible_only: bool,
        roles: Option<Vec<String>>,
    ) -> Locator {
        let opts = build_query_options(max_depth, max_elements, visible_only, roles);
        Locator {
            provider: self.provider.clone(),
            target: self.target.clone(),
            selector: selector.to_string(),
            opts,
            nth: None,
        }
    }

    fn __len__(&self) -> usize {
        self.py_nodes.len()
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<PyObject> {
        let list = PyList::new(py, &self.py_nodes)?;
        list.call_method0("__iter__").map(|i| i.unbind())
    }

    fn __repr__(&self) -> String {
        match self.pid {
            Some(pid) => format!(
                "Tree(app='{}', pid={}, nodes={})",
                self.app_name,
                pid,
                self.py_nodes.len()
            ),
            None => format!(
                "Tree(app='{}', nodes={})",
                self.app_name,
                self.py_nodes.len()
            ),
        }
    }

    fn __str__(&self) -> String {
        self.rust_tree.dump()
    }
}

impl Tree {
    fn perform_simple(&self, target: &Bound<'_, PyAny>, action: xa11y::Action) -> PyResult<()> {
        let node_index = self.resolve_target_index(target)?;
        let rust_node = self
            .rust_tree
            .get(node_index)
            .ok_or_else(|| PyValueError::new_err("Invalid node reference"))?;
        self.provider
            .perform_action(&self.rust_tree, rust_node, action, None)
            .map_err(to_py_err)
    }

    fn resolve_target_index(&self, target: &Bound<'_, PyAny>) -> PyResult<u32> {
        if let Ok(node) = target.downcast::<Node>() {
            return Ok(node.borrow()._index);
        }
        if let Ok(selector) = target.extract::<String>() {
            let matches = self.rust_tree.query(&selector).map_err(to_py_err)?;
            let node = matches
                .first()
                .ok_or_else(|| to_py_err(xa11y::Error::SelectorNotMatched { selector }))?;
            return Ok(node.index);
        }
        Err(PyTypeError::new_err(
            "target must be a Node or a selector string",
        ))
    }
}

// ── Tree construction ───────────────────────────────────────────────────────

fn convert_tree(
    py: Python<'_>,
    rust_tree: xa11y::Tree,
    provider: Arc<dyn xa11y::Provider>,
    target: xa11y::AppTarget,
    opts: xa11y::QueryOptions,
) -> PyResult<Py<Tree>> {
    let num_nodes = rust_tree.len();
    let mut py_nodes: Vec<Py<Node>> = Vec::with_capacity(num_nodes);

    for i in 0..num_nodes {
        let n = rust_tree
            .get(i as u32)
            .expect("index valid in range 0..len");
        py_nodes.push(make_py_node(py, n)?);
    }

    // Build a shared PyList so every Node can navigate to children/parent directly.
    let all_nodes_list: Py<PyList> = PyList::new(py, &py_nodes)?.unbind();
    for py_node in &py_nodes {
        py_node.borrow_mut(py)._all_nodes = Some(all_nodes_list.clone_ref(py));
    }

    Py::new(
        py,
        Tree {
            app_name: rust_tree.app_name.clone(),
            pid: rust_tree.pid,
            screen_size: rust_tree.screen_size,
            py_nodes,
            rust_tree,
            provider,
            target,
            _opts: opts,
        },
    )
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
        Ok(self.resolve_node()?.role.to_snake_case().to_string())
    }

    fn name(&self) -> PyResult<Option<String>> {
        Ok(self.resolve_node()?.name.clone())
    }

    fn value(&self) -> PyResult<Option<String>> {
        Ok(self.resolve_node()?.value.clone())
    }

    fn description(&self) -> PyResult<Option<String>> {
        Ok(self.resolve_node()?.description.clone())
    }

    fn is_visible(&self) -> PyResult<bool> {
        Ok(self.resolve_node()?.states.visible)
    }

    fn is_enabled(&self) -> PyResult<bool> {
        Ok(self.resolve_node()?.states.enabled)
    }

    fn is_focused(&self) -> PyResult<bool> {
        Ok(self.resolve_node()?.states.focused)
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

    /// Get a snapshot of the matched node.
    fn get(&self, py: Python<'_>) -> PyResult<Py<Node>> {
        let (tree, node_index) = self.resolve()?;
        let n = tree.get(node_index).expect("valid after resolve");
        make_py_node(py, n)
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

    fn resolve_node(&self) -> PyResult<xa11y::Node> {
        let (tree, idx) = self.resolve()?;
        Ok(tree.get(idx).expect("valid after resolve").clone())
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
        let node = tree.get(node_index).expect("valid after resolve");
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

            let node: Option<xa11y::Node> = (|| {
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

// ── Event helpers ───────────────────────────────────────────────────────────

fn parse_event_kind(s: &str) -> PyResult<xa11y::EventKind> {
    match s {
        "focus_changed" => Ok(xa11y::EventKind::FocusChanged),
        "value_changed" => Ok(xa11y::EventKind::ValueChanged),
        "name_changed" => Ok(xa11y::EventKind::NameChanged),
        "state_changed" => Ok(xa11y::EventKind::StateChanged),
        "structure_changed" => Ok(xa11y::EventKind::StructureChanged),
        "window_opened" => Ok(xa11y::EventKind::WindowOpened),
        "window_closed" => Ok(xa11y::EventKind::WindowClosed),
        "window_activated" => Ok(xa11y::EventKind::WindowActivated),
        "window_deactivated" => Ok(xa11y::EventKind::WindowDeactivated),
        "selection_changed" => Ok(xa11y::EventKind::SelectionChanged),
        "menu_opened" => Ok(xa11y::EventKind::MenuOpened),
        "menu_closed" => Ok(xa11y::EventKind::MenuClosed),
        "alert" => Ok(xa11y::EventKind::Alert),
        "text_changed" => Ok(xa11y::EventKind::TextChanged),
        _ => Err(PyValueError::new_err(format!("Unknown event kind: {s}"))),
    }
}

fn event_kind_to_str(k: xa11y::EventKind) -> &'static str {
    match k {
        xa11y::EventKind::FocusChanged => "focus_changed",
        xa11y::EventKind::ValueChanged => "value_changed",
        xa11y::EventKind::NameChanged => "name_changed",
        xa11y::EventKind::StateChanged => "state_changed",
        xa11y::EventKind::StructureChanged => "structure_changed",
        xa11y::EventKind::WindowOpened => "window_opened",
        xa11y::EventKind::WindowClosed => "window_closed",
        xa11y::EventKind::WindowActivated => "window_activated",
        xa11y::EventKind::WindowDeactivated => "window_deactivated",
        xa11y::EventKind::SelectionChanged => "selection_changed",
        xa11y::EventKind::MenuOpened => "menu_opened",
        xa11y::EventKind::MenuClosed => "menu_closed",
        xa11y::EventKind::Alert => "alert",
        xa11y::EventKind::TextChanged => "text_changed",
    }
}

fn parse_state_flag(s: &str) -> PyResult<xa11y::StateFlag> {
    match s {
        "enabled" => Ok(xa11y::StateFlag::Enabled),
        "visible" => Ok(xa11y::StateFlag::Visible),
        "focused" => Ok(xa11y::StateFlag::Focused),
        "checked" => Ok(xa11y::StateFlag::Checked),
        "selected" => Ok(xa11y::StateFlag::Selected),
        "expanded" => Ok(xa11y::StateFlag::Expanded),
        "editable" => Ok(xa11y::StateFlag::Editable),
        "focusable" => Ok(xa11y::StateFlag::Focusable),
        "modal" => Ok(xa11y::StateFlag::Modal),
        "required" => Ok(xa11y::StateFlag::Required),
        "busy" => Ok(xa11y::StateFlag::Busy),
        _ => Err(PyValueError::new_err(format!("Unknown state flag: {s}"))),
    }
}

fn state_flag_to_str(f: xa11y::StateFlag) -> &'static str {
    match f {
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

fn parse_element_state(s: &str) -> PyResult<xa11y::ElementState> {
    match s {
        "attached" => Ok(xa11y::ElementState::Attached),
        "detached" => Ok(xa11y::ElementState::Detached),
        "visible" => Ok(xa11y::ElementState::Visible),
        "hidden" => Ok(xa11y::ElementState::Hidden),
        "enabled" => Ok(xa11y::ElementState::Enabled),
        "disabled" => Ok(xa11y::ElementState::Disabled),
        "focused" => Ok(xa11y::ElementState::Focused),
        "unfocused" => Ok(xa11y::ElementState::Unfocused),
        _ => Err(PyValueError::new_err(format!("Unknown element state: {s}"))),
    }
}

fn text_change_type_to_str(t: xa11y::TextChangeType) -> &'static str {
    match t {
        xa11y::TextChangeType::Insert => "insert",
        xa11y::TextChangeType::Delete => "delete",
        xa11y::TextChangeType::Replace => "replace",
        xa11y::TextChangeType::Unknown => "unknown",
    }
}

fn build_event_filter(
    kinds: Option<Vec<String>>,
    selector: Option<String>,
    state_flags: Option<Vec<String>>,
) -> PyResult<xa11y::EventFilter> {
    let kinds = match kinds {
        Some(ks) => ks
            .iter()
            .map(|s| parse_event_kind(s))
            .collect::<PyResult<Vec<_>>>()?,
        None => vec![],
    };
    let state_flags = match state_flags {
        Some(fs) => fs
            .iter()
            .map(|s| parse_state_flag(s))
            .collect::<PyResult<Vec<_>>>()?,
        None => vec![],
    };
    Ok(xa11y::EventFilter {
        kinds,
        selector,
        state_flags,
    })
}

// ── Event data classes ──────────────────────────────────────────────────────

#[pyclass(frozen)]
#[derive(Clone)]
struct TextChangeData {
    #[pyo3(get)]
    change_type: String,
    #[pyo3(get)]
    position: Option<u32>,
}

#[pymethods]
impl TextChangeData {
    fn __repr__(&self) -> String {
        match self.position {
            Some(pos) => format!(
                "TextChangeData(change_type='{}', position={})",
                self.change_type, pos
            ),
            None => format!("TextChangeData(change_type='{}')", self.change_type),
        }
    }
}

#[pyclass(frozen)]
struct Event {
    #[pyo3(get)]
    kind: String,
    #[pyo3(get)]
    app: AppInfo,
    #[pyo3(get)]
    target: Option<Py<Node>>,
    #[pyo3(get)]
    state_flag: Option<String>,
    #[pyo3(get)]
    state_value: Option<bool>,
    #[pyo3(get)]
    text_change: Option<TextChangeData>,
}

#[pymethods]
impl Event {
    fn __repr__(&self) -> String {
        format!("Event(kind='{}', app='{}')", self.kind, self.app.name)
    }
}

fn make_py_event(py: Python<'_>, e: &xa11y::Event) -> PyResult<Event> {
    let target = match &e.target {
        Some(n) => Some(make_py_node(py, n)?),
        None => None,
    };
    let text_change = e.text_change.as_ref().map(|tc| TextChangeData {
        change_type: text_change_type_to_str(tc.change_type).to_string(),
        position: tc.position,
    });
    Ok(Event {
        kind: event_kind_to_str(e.kind).to_string(),
        app: AppInfo {
            name: e.app.name.clone(),
            pid: e.app.pid,
            bundle_id: e.app.bundle_id.clone(),
        },
        target,
        state_flag: e.state_flag.map(|f| state_flag_to_str(f).to_string()),
        state_value: e.state_value,
        text_change,
    })
}

// ── Subscription ────────────────────────────────────────────────────────────

#[pyclass(unsendable)]
struct Subscription {
    inner: Option<xa11y::Subscription>,
}

#[pymethods]
impl Subscription {
    /// Try to receive an event without blocking. Returns None if no event is ready.
    fn try_recv(&self, py: Python<'_>) -> PyResult<Option<Event>> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Subscription has been closed"))?;
        match inner.try_recv() {
            Some(e) => Ok(Some(make_py_event(py, &e)?)),
            None => Ok(None),
        }
    }

    /// Close the subscription, stopping event delivery.
    fn close(&mut self) {
        self.inner.take();
    }

    fn __repr__(&self) -> String {
        if self.inner.is_some() {
            "Subscription(active)".to_string()
        } else {
            "Subscription(closed)".to_string()
        }
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &mut self,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) {
        self.close();
    }
}

// ── Module-level functions ──────────────────────────────────────────────────

/// Subscribe to accessibility events from an application.
#[pyfunction]
#[pyo3(signature = (name=None, *, pid=None, kinds=None, selector=None, state_flags=None))]
fn subscribe(
    py: Python<'_>,
    name: Option<&str>,
    pid: Option<u32>,
    kinds: Option<Vec<String>>,
    selector: Option<String>,
    state_flags: Option<Vec<String>>,
) -> PyResult<Subscription> {
    let provider = get_event_provider()?;
    let target = resolve_app_target(name, pid)?;
    let filter = build_event_filter(kinds, selector, state_flags)?;
    let sub = py
        .allow_threads(|| provider.subscribe(&target, filter))
        .map_err(to_py_err)?;
    Ok(Subscription { inner: Some(sub) })
}

/// Wait for a single event matching the filter, with timeout.
#[pyfunction]
#[pyo3(signature = (name=None, *, pid=None, kinds=None, selector=None, state_flags=None, timeout=5.0))]
fn wait_for_event(
    py: Python<'_>,
    name: Option<&str>,
    pid: Option<u32>,
    kinds: Option<Vec<String>>,
    selector: Option<String>,
    state_flags: Option<Vec<String>>,
    timeout: f64,
) -> PyResult<Event> {
    let provider = get_event_provider()?;
    let target = resolve_app_target(name, pid)?;
    let filter = build_event_filter(kinds, selector, state_flags)?;
    let dur = Duration::from_secs_f64(timeout);
    let event = py
        .allow_threads(|| provider.wait_for_event(&target, filter, dur))
        .map_err(to_py_err)?;
    make_py_event(py, &event)
}

/// Wait for an element to reach a desired state.
#[pyfunction]
#[pyo3(signature = (name=None, *, pid=None, selector, state, timeout=5.0))]
fn wait_for(
    py: Python<'_>,
    name: Option<&str>,
    pid: Option<u32>,
    selector: &str,
    state: &str,
    timeout: f64,
) -> PyResult<Py<Node>> {
    let provider = get_event_provider()?;
    let target = resolve_app_target(name, pid)?;
    let element_state = parse_element_state(state)?;
    let dur = Duration::from_secs_f64(timeout);
    let node = py
        .allow_threads(|| provider.wait_for(&target, selector, element_state, dur))
        .map_err(to_py_err)?;
    make_py_node(py, &node)
}

// ── Module-level functions ──────────────────────────────────────────────────

/// Get an app's accessibility tree.
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
) -> PyResult<Py<Tree>> {
    let provider = get_provider()?;
    let target = resolve_app_target(name, pid)?;
    let opts = build_query_options(max_depth, max_elements, visible_only, roles);
    let p = provider.clone();
    let rust_tree = py
        .allow_threads(|| p.get_app_tree(&target, &opts))
        .map_err(to_py_err)?;
    convert_tree(py, rust_tree, provider, target, opts)
}

/// Get accessibility trees for all running applications.
#[pyfunction]
#[pyo3(signature = (*, max_depth=None, max_elements=None, visible_only=false, roles=None))]
fn all_apps(
    py: Python<'_>,
    max_depth: Option<u32>,
    max_elements: Option<u32>,
    visible_only: bool,
    roles: Option<Vec<String>>,
) -> PyResult<Py<Tree>> {
    let provider = get_provider()?;
    let opts = build_query_options(max_depth, max_elements, visible_only, roles);
    let p = provider.clone();
    let rust_tree = py
        .allow_threads(|| p.get_all_apps(&opts))
        .map_err(to_py_err)?;
    let target = xa11y::AppTarget::ByName(String::new());
    convert_tree(py, rust_tree, provider, target, opts)
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

/// List running applications.
#[pyfunction]
fn list_apps(py: Python<'_>) -> PyResult<Vec<AppInfo>> {
    let provider = get_provider()?;
    let apps = py
        .allow_threads(|| provider.list_apps())
        .map_err(to_py_err)?;
    Ok(apps
        .into_iter()
        .map(|a| AppInfo {
            name: a.name,
            pid: a.pid,
            bundle_id: a.bundle_id,
        })
        .collect())
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
    m.add_class::<Tree>()?;
    m.add_class::<Node>()?;
    m.add_class::<Locator>()?;
    m.add_class::<Rect>()?;
    m.add_class::<AppInfo>()?;
    m.add_class::<Event>()?;
    m.add_class::<TextChangeData>()?;
    m.add_class::<Subscription>()?;

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
    m.add_function(wrap_pyfunction!(all_apps, m)?)?;
    m.add_function(wrap_pyfunction!(locator, m)?)?;
    m.add_function(wrap_pyfunction!(list_apps, m)?)?;
    m.add_function(wrap_pyfunction!(check_permissions, m)?)?;
    m.add_function(wrap_pyfunction!(subscribe, m)?)?;
    m.add_function(wrap_pyfunction!(wait_for_event, m)?)?;
    m.add_function(wrap_pyfunction!(wait_for, m)?)?;

    // Test helpers
    m.add_function(wrap_pyfunction!(_make_test_tree, m)?)?;
    m.add_function(wrap_pyfunction!(_make_test_apps, m)?)?;

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

    fn get_all_apps(&self, _opts: &xa11y::QueryOptions) -> xa11y::Result<xa11y::Tree> {
        Ok(self.tree.clone())
    }

    fn perform_action(
        &self,
        _tree: &xa11y::Tree,
        node: &xa11y::Node,
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

    fn list_apps(&self) -> xa11y::Result<Vec<xa11y::AppInfo>> {
        Ok(vec![
            xa11y::AppInfo {
                name: "TestApp".to_string(),
                pid: 1234,
                bundle_id: Some("com.test.app".to_string()),
            },
            xa11y::AppInfo {
                name: "OtherApp".to_string(),
                pid: 5678,
                bundle_id: None,
            },
        ])
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
        Node {
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
            raw: xa11y::RawPlatformData::Synthetic,
            index: 0,
            children_indices: vec![1],
            parent_index: None,
        },
        // [1] window "Main Window"
        Node {
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
            raw: xa11y::RawPlatformData::Synthetic,
            index: 1,
            children_indices: vec![2, 5],
            parent_index: Some(0),
        },
        // [2] toolbar "Navigation"
        Node {
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
            raw: xa11y::RawPlatformData::Synthetic,
            index: 2,
            children_indices: vec![3, 4],
            parent_index: Some(1),
        },
        // [3] button "Back"
        Node {
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

            actions: vec![Action::Press, Action::Focus],
            states: StateSet {
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: Some("btn-back".to_string()),
            raw: xa11y::RawPlatformData::Synthetic,
            index: 3,
            children_indices: vec![],
            parent_index: Some(2),
        },
        // [4] button "Forward" (disabled)
        Node {
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

            actions: vec![Action::Press, Action::Focus],
            states: StateSet {
                enabled: false,
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 4,
            children_indices: vec![],
            parent_index: Some(2),
        },
        // [5] group "Content"
        Node {
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
            raw: xa11y::RawPlatformData::Synthetic,
            index: 5,
            children_indices: vec![6, 7, 8, 9, 10],
            parent_index: Some(1),
        },
        // [6] text_field "Search"
        Node {
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

            actions: vec![Action::Focus, Action::SetValue, Action::TypeText],
            states: StateSet {
                editable: true,
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 6,
            children_indices: vec![],
            parent_index: Some(5),
        },
        // [7] check_box "Agree" (checked=on)
        Node {
            role: Role::CheckBox,
            name: Some("Agree".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![Action::Toggle, Action::Focus],
            states: StateSet {
                checked: Some(Toggled::On),
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 7,
            children_indices: vec![],
            parent_index: Some(5),
        },
        // [8] slider "Volume"
        Node {
            role: Role::Slider,
            name: Some("Volume".to_string()),
            value: Some("75".to_string()),
            description: None,
            bounds: None,

            actions: vec![
                Action::Increment,
                Action::Decrement,
                Action::SetValue,
                Action::Focus,
            ],
            states: StateSet {
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: Some(75.0),
            min_value: Some(0.0),
            max_value: Some(100.0),
            stable_id: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 8,
            children_indices: vec![],
            parent_index: Some(5),
        },
        // [9] static_text "Status" (hidden)
        Node {
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
            raw: xa11y::RawPlatformData::Synthetic,
            index: 9,
            children_indices: vec![],
            parent_index: Some(5),
        },
        // [10] list "Items"
        Node {
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
            raw: xa11y::RawPlatformData::Synthetic,
            index: 10,
            children_indices: vec![11, 12],
            parent_index: Some(5),
        },
        // [11] list_item "Item 1" (selected)
        Node {
            role: Role::ListItem,
            name: Some("Item 1".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![Action::Select, Action::Focus],
            states: StateSet {
                selected: true,
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 11,
            children_indices: vec![],
            parent_index: Some(10),
        },
        // [12] list_item "Item 2"
        Node {
            role: Role::ListItem,
            name: Some("Item 2".to_string()),
            value: None,
            description: None,
            bounds: None,

            actions: vec![Action::Select, Action::Focus],
            states: StateSet {
                focusable: true,
                ..StateSet::default()
            },

            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            raw: xa11y::RawPlatformData::Synthetic,
            index: 12,
            children_indices: vec![],
            parent_index: Some(10),
        },
    ];

    Tree::new(
        "TestApp".to_string(),
        Some(1234),
        (1920, 1080),
        nodes,
        QueryOptions::default(),
    )
}

/// Create a test tree (for Python unit tests). Returns a Tree backed by a mock provider.
#[pyfunction]
fn _make_test_tree(py: Python<'_>) -> PyResult<Py<Tree>> {
    let tree = build_test_tree();
    let provider: Arc<dyn xa11y::Provider> = Arc::new(MockProvider {
        tree: tree.clone(),
        actions: std::sync::Mutex::new(Vec::new()),
    });
    let target = xa11y::AppTarget::ByName("TestApp".to_string());
    let opts = xa11y::QueryOptions::default();
    convert_tree(py, tree, provider, target, opts)
}

/// Create a mock-backed list of apps (for Python unit tests).
#[pyfunction]
fn _make_test_apps() -> Vec<AppInfo> {
    vec![
        AppInfo {
            name: "TestApp".to_string(),
            pid: 1234,
            bundle_id: Some("com.test.app".to_string()),
        },
        AppInfo {
            name: "OtherApp".to_string(),
            pid: 5678,
            bundle_id: None,
        },
    ]
}
