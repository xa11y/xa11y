use std::sync::Arc;
use std::time::Duration;

use pyo3::exceptions::{PyRuntimeError, PyTimeoutError, PyValueError};
use pyo3::prelude::*;

// ── Error conversion ─────────────────────────────────────────────────

fn to_py_err(e: xa11y_core::Error) -> PyErr {
    match e {
        xa11y_core::Error::Timeout { elapsed } => {
            PyTimeoutError::new_err(format!("Timeout after {elapsed:?}"))
        }
        xa11y_core::Error::InvalidSelector { selector, message } => {
            PyValueError::new_err(format!("Invalid selector '{selector}': {message}"))
        }
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

// ── Role enum ────────────────────────────────────────────────────────

#[pyclass(eq, eq_int, hash, frozen)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Unknown = 0,
    Window = 1,
    Application = 2,
    Button = 3,
    CheckBox = 4,
    RadioButton = 5,
    TextField = 6,
    TextArea = 7,
    StaticText = 8,
    ComboBox = 9,
    List = 10,
    ListItem = 11,
    Menu = 12,
    MenuItem = 13,
    MenuBar = 14,
    Tab = 15,
    TabGroup = 16,
    Table = 17,
    TableRow = 18,
    TableCell = 19,
    Toolbar = 20,
    ScrollBar = 21,
    Slider = 22,
    Image = 23,
    Link = 24,
    Group = 25,
    Dialog = 26,
    Alert = 27,
    ProgressBar = 28,
    TreeItem = 29,
    WebArea = 30,
    Heading = 31,
    Separator = 32,
    SplitGroup = 33,
    Switch = 34,
    SpinButton = 35,
    Tooltip = 36,
    Status = 37,
    Navigation = 38,
}

impl From<xa11y_core::Role> for Role {
    fn from(r: xa11y_core::Role) -> Self {
        match r {
            xa11y_core::Role::Unknown => Role::Unknown,
            xa11y_core::Role::Window => Role::Window,
            xa11y_core::Role::Application => Role::Application,
            xa11y_core::Role::Button => Role::Button,
            xa11y_core::Role::CheckBox => Role::CheckBox,
            xa11y_core::Role::RadioButton => Role::RadioButton,
            xa11y_core::Role::TextField => Role::TextField,
            xa11y_core::Role::TextArea => Role::TextArea,
            xa11y_core::Role::StaticText => Role::StaticText,
            xa11y_core::Role::ComboBox => Role::ComboBox,
            xa11y_core::Role::List => Role::List,
            xa11y_core::Role::ListItem => Role::ListItem,
            xa11y_core::Role::Menu => Role::Menu,
            xa11y_core::Role::MenuItem => Role::MenuItem,
            xa11y_core::Role::MenuBar => Role::MenuBar,
            xa11y_core::Role::Tab => Role::Tab,
            xa11y_core::Role::TabGroup => Role::TabGroup,
            xa11y_core::Role::Table => Role::Table,
            xa11y_core::Role::TableRow => Role::TableRow,
            xa11y_core::Role::TableCell => Role::TableCell,
            xa11y_core::Role::Toolbar => Role::Toolbar,
            xa11y_core::Role::ScrollBar => Role::ScrollBar,
            xa11y_core::Role::Slider => Role::Slider,
            xa11y_core::Role::Image => Role::Image,
            xa11y_core::Role::Link => Role::Link,
            xa11y_core::Role::Group => Role::Group,
            xa11y_core::Role::Dialog => Role::Dialog,
            xa11y_core::Role::Alert => Role::Alert,
            xa11y_core::Role::ProgressBar => Role::ProgressBar,
            xa11y_core::Role::TreeItem => Role::TreeItem,
            xa11y_core::Role::WebArea => Role::WebArea,
            xa11y_core::Role::Heading => Role::Heading,
            xa11y_core::Role::Separator => Role::Separator,
            xa11y_core::Role::SplitGroup => Role::SplitGroup,
            xa11y_core::Role::Switch => Role::Switch,
            xa11y_core::Role::SpinButton => Role::SpinButton,
            xa11y_core::Role::Tooltip => Role::Tooltip,
            xa11y_core::Role::Status => Role::Status,
            xa11y_core::Role::Navigation => Role::Navigation,
        }
    }
}

impl From<Role> for xa11y_core::Role {
    fn from(r: Role) -> Self {
        match r {
            Role::Unknown => xa11y_core::Role::Unknown,
            Role::Window => xa11y_core::Role::Window,
            Role::Application => xa11y_core::Role::Application,
            Role::Button => xa11y_core::Role::Button,
            Role::CheckBox => xa11y_core::Role::CheckBox,
            Role::RadioButton => xa11y_core::Role::RadioButton,
            Role::TextField => xa11y_core::Role::TextField,
            Role::TextArea => xa11y_core::Role::TextArea,
            Role::StaticText => xa11y_core::Role::StaticText,
            Role::ComboBox => xa11y_core::Role::ComboBox,
            Role::List => xa11y_core::Role::List,
            Role::ListItem => xa11y_core::Role::ListItem,
            Role::Menu => xa11y_core::Role::Menu,
            Role::MenuItem => xa11y_core::Role::MenuItem,
            Role::MenuBar => xa11y_core::Role::MenuBar,
            Role::Tab => xa11y_core::Role::Tab,
            Role::TabGroup => xa11y_core::Role::TabGroup,
            Role::Table => xa11y_core::Role::Table,
            Role::TableRow => xa11y_core::Role::TableRow,
            Role::TableCell => xa11y_core::Role::TableCell,
            Role::Toolbar => xa11y_core::Role::Toolbar,
            Role::ScrollBar => xa11y_core::Role::ScrollBar,
            Role::Slider => xa11y_core::Role::Slider,
            Role::Image => xa11y_core::Role::Image,
            Role::Link => xa11y_core::Role::Link,
            Role::Group => xa11y_core::Role::Group,
            Role::Dialog => xa11y_core::Role::Dialog,
            Role::Alert => xa11y_core::Role::Alert,
            Role::ProgressBar => xa11y_core::Role::ProgressBar,
            Role::TreeItem => xa11y_core::Role::TreeItem,
            Role::WebArea => xa11y_core::Role::WebArea,
            Role::Heading => xa11y_core::Role::Heading,
            Role::Separator => xa11y_core::Role::Separator,
            Role::SplitGroup => xa11y_core::Role::SplitGroup,
            Role::Switch => xa11y_core::Role::Switch,
            Role::SpinButton => xa11y_core::Role::SpinButton,
            Role::Tooltip => xa11y_core::Role::Tooltip,
            Role::Status => xa11y_core::Role::Status,
            Role::Navigation => xa11y_core::Role::Navigation,
        }
    }
}

// ── Action enum ──────────────────────────────────────────────────────

#[pyclass(eq, eq_int, hash, frozen)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Press = 0,
    Focus = 1,
    SetValue = 2,
    Toggle = 3,
    Expand = 4,
    Collapse = 5,
    Select = 6,
    ShowMenu = 7,
    ScrollIntoView = 8,
    Scroll = 9,
    Increment = 10,
    Decrement = 11,
    Blur = 12,
    SetTextSelection = 13,
    TypeText = 14,
}

impl From<xa11y_core::Action> for Action {
    fn from(a: xa11y_core::Action) -> Self {
        match a {
            xa11y_core::Action::Press => Action::Press,
            xa11y_core::Action::Focus => Action::Focus,
            xa11y_core::Action::SetValue => Action::SetValue,
            xa11y_core::Action::Toggle => Action::Toggle,
            xa11y_core::Action::Expand => Action::Expand,
            xa11y_core::Action::Collapse => Action::Collapse,
            xa11y_core::Action::Select => Action::Select,
            xa11y_core::Action::ShowMenu => Action::ShowMenu,
            xa11y_core::Action::ScrollIntoView => Action::ScrollIntoView,
            xa11y_core::Action::Scroll => Action::Scroll,
            xa11y_core::Action::Increment => Action::Increment,
            xa11y_core::Action::Decrement => Action::Decrement,
            xa11y_core::Action::Blur => Action::Blur,
            xa11y_core::Action::SetTextSelection => Action::SetTextSelection,
            xa11y_core::Action::TypeText => Action::TypeText,
        }
    }
}

impl From<Action> for xa11y_core::Action {
    fn from(a: Action) -> Self {
        match a {
            Action::Press => xa11y_core::Action::Press,
            Action::Focus => xa11y_core::Action::Focus,
            Action::SetValue => xa11y_core::Action::SetValue,
            Action::Toggle => xa11y_core::Action::Toggle,
            Action::Expand => xa11y_core::Action::Expand,
            Action::Collapse => xa11y_core::Action::Collapse,
            Action::Select => xa11y_core::Action::Select,
            Action::ShowMenu => xa11y_core::Action::ShowMenu,
            Action::ScrollIntoView => xa11y_core::Action::ScrollIntoView,
            Action::Scroll => xa11y_core::Action::Scroll,
            Action::Increment => xa11y_core::Action::Increment,
            Action::Decrement => xa11y_core::Action::Decrement,
            Action::Blur => xa11y_core::Action::Blur,
            Action::SetTextSelection => xa11y_core::Action::SetTextSelection,
            Action::TypeText => xa11y_core::Action::TypeText,
        }
    }
}

// ── ScrollDirection enum ─────────────────────────────────────────────

#[pyclass(eq, eq_int, hash, frozen)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScrollDirection {
    Up = 0,
    Down = 1,
    Left = 2,
    Right = 3,
}

impl From<ScrollDirection> for xa11y_core::ScrollDirection {
    fn from(d: ScrollDirection) -> Self {
        match d {
            ScrollDirection::Up => xa11y_core::ScrollDirection::Up,
            ScrollDirection::Down => xa11y_core::ScrollDirection::Down,
            ScrollDirection::Left => xa11y_core::ScrollDirection::Left,
            ScrollDirection::Right => xa11y_core::ScrollDirection::Right,
        }
    }
}

// ── Toggled enum ─────────────────────────────────────────────────────

#[pyclass(eq, eq_int, hash, frozen)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Toggled {
    Off = 0,
    On = 1,
    Mixed = 2,
}

impl From<xa11y_core::Toggled> for Toggled {
    fn from(t: xa11y_core::Toggled) -> Self {
        match t {
            xa11y_core::Toggled::Off => Toggled::Off,
            xa11y_core::Toggled::On => Toggled::On,
            xa11y_core::Toggled::Mixed => Toggled::Mixed,
        }
    }
}

// ── Rect ─────────────────────────────────────────────────────────────

#[pyclass(frozen)]
#[derive(Clone)]
pub struct Rect {
    #[pyo3(get)]
    pub x: i32,
    #[pyo3(get)]
    pub y: i32,
    #[pyo3(get)]
    pub width: u32,
    #[pyo3(get)]
    pub height: u32,
}

#[pymethods]
impl Rect {
    fn __repr__(&self) -> String {
        format!(
            "Rect(x={}, y={}, width={}, height={})",
            self.x, self.y, self.width, self.height
        )
    }
}

impl From<xa11y_core::Rect> for Rect {
    fn from(r: xa11y_core::Rect) -> Self {
        Rect {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        }
    }
}

// ── NormalizedRect ────────────────────────────────────────────────────

#[pyclass(frozen)]
#[derive(Clone)]
pub struct NormalizedRect {
    #[pyo3(get)]
    pub left: f64,
    #[pyo3(get)]
    pub top: f64,
    #[pyo3(get)]
    pub right: f64,
    #[pyo3(get)]
    pub bottom: f64,
}

#[pymethods]
impl NormalizedRect {
    fn __repr__(&self) -> String {
        format!(
            "NormalizedRect(left={}, top={}, right={}, bottom={})",
            self.left, self.top, self.right, self.bottom
        )
    }
}

impl From<xa11y_core::NormalizedRect> for NormalizedRect {
    fn from(r: xa11y_core::NormalizedRect) -> Self {
        NormalizedRect {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

// ── StateSet ─────────────────────────────────────────────────────────

#[pyclass(frozen)]
#[derive(Clone)]
pub struct StateSet {
    #[pyo3(get)]
    pub enabled: bool,
    #[pyo3(get)]
    pub visible: bool,
    #[pyo3(get)]
    pub focused: bool,
    #[pyo3(get)]
    pub checked: Option<Toggled>,
    #[pyo3(get)]
    pub selected: bool,
    #[pyo3(get)]
    pub expanded: Option<bool>,
    #[pyo3(get)]
    pub editable: bool,
    #[pyo3(get)]
    pub focusable: bool,
    #[pyo3(get)]
    pub modal: bool,
    #[pyo3(get)]
    pub required: bool,
    #[pyo3(get)]
    pub busy: bool,
}

#[pymethods]
impl StateSet {
    fn __repr__(&self) -> String {
        let mut flags = Vec::new();
        if self.enabled {
            flags.push("enabled");
        }
        if self.visible {
            flags.push("visible");
        }
        if self.focused {
            flags.push("focused");
        }
        if self.selected {
            flags.push("selected");
        }
        if self.editable {
            flags.push("editable");
        }
        if self.focusable {
            flags.push("focusable");
        }
        if self.modal {
            flags.push("modal");
        }
        if self.required {
            flags.push("required");
        }
        if self.busy {
            flags.push("busy");
        }
        format!("StateSet({})", flags.join(", "))
    }
}

impl From<xa11y_core::StateSet> for StateSet {
    fn from(s: xa11y_core::StateSet) -> Self {
        StateSet {
            enabled: s.enabled,
            visible: s.visible,
            focused: s.focused,
            checked: s.checked.map(Toggled::from),
            selected: s.selected,
            expanded: s.expanded,
            editable: s.editable,
            focusable: s.focusable,
            modal: s.modal,
            required: s.required,
            busy: s.busy,
        }
    }
}

// ── Node ─────────────────────────────────────────────────────────────

#[pyclass(frozen)]
#[derive(Clone)]
pub struct Node {
    #[pyo3(get)]
    pub role: Role,
    #[pyo3(get)]
    pub name: Option<String>,
    #[pyo3(get)]
    pub value: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
    pub bounds: Option<Rect>,
    #[pyo3(get)]
    pub bounds_normalized: Option<NormalizedRect>,
    #[pyo3(get)]
    pub depth: u32,
    #[pyo3(get)]
    pub numeric_value: Option<f64>,
    #[pyo3(get)]
    pub min_value: Option<f64>,
    #[pyo3(get)]
    pub max_value: Option<f64>,
    #[pyo3(get)]
    pub stable_id: Option<String>,
    // Not exposed as Python getters — internal
    actions_inner: Vec<Action>,
    states_inner: StateSet,
    index: u32,
    children_indices: Vec<u32>,
    parent_index: Option<u32>,
}

#[pymethods]
impl Node {
    /// Available actions on this element.
    #[getter]
    fn actions(&self) -> Vec<Action> {
        self.actions_inner.clone()
    }

    /// Current state flags.
    #[getter]
    fn states(&self) -> StateSet {
        self.states_inner.clone()
    }

    fn __repr__(&self) -> String {
        let name_part = self
            .name
            .as_ref()
            .map(|n| format!(" \"{n}\""))
            .unwrap_or_default();
        format!("Node({:?}{})", self.role, name_part)
    }
}

impl From<&xa11y_core::Node> for Node {
    fn from(n: &xa11y_core::Node) -> Self {
        Node {
            role: Role::from(n.role),
            name: n.name.clone(),
            value: n.value.clone(),
            description: n.description.clone(),
            bounds: n.bounds.map(Rect::from),
            bounds_normalized: n.bounds_normalized.map(NormalizedRect::from),
            actions_inner: n.actions.iter().map(|a| Action::from(*a)).collect(),
            states_inner: StateSet::from(n.states.clone()),
            depth: n.depth,
            numeric_value: n.numeric_value,
            min_value: n.min_value,
            max_value: n.max_value,
            stable_id: n.stable_id.clone(),
            index: n.index,
            children_indices: n.children_indices.clone(),
            parent_index: n.parent_index,
        }
    }
}

// ── AppInfo ──────────────────────────────────────────────────────────

#[pyclass(frozen)]
#[derive(Clone)]
pub struct AppInfo {
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub pid: u32,
    #[pyo3(get)]
    pub bundle_id: Option<String>,
}

#[pymethods]
impl AppInfo {
    fn __repr__(&self) -> String {
        format!("AppInfo(name=\"{}\", pid={})", self.name, self.pid)
    }
}

impl From<xa11y_core::AppInfo> for AppInfo {
    fn from(a: xa11y_core::AppInfo) -> Self {
        AppInfo {
            name: a.name,
            pid: a.pid,
            bundle_id: a.bundle_id,
        }
    }
}

// ── QueryOptions ─────────────────────────────────────────────────────

#[pyclass]
#[derive(Clone)]
pub struct QueryOptions {
    #[pyo3(get, set)]
    pub max_depth: Option<u32>,
    #[pyo3(get, set)]
    pub max_elements: Option<u32>,
    #[pyo3(get, set)]
    pub visible_only: bool,
    #[pyo3(get, set)]
    pub include_raw: bool,
}

#[pymethods]
impl QueryOptions {
    #[new]
    #[pyo3(signature = (*, max_depth=None, max_elements=None, visible_only=false, include_raw=false))]
    fn new(
        max_depth: Option<u32>,
        max_elements: Option<u32>,
        visible_only: bool,
        include_raw: bool,
    ) -> Self {
        QueryOptions {
            max_depth,
            max_elements,
            visible_only,
            include_raw,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "QueryOptions(max_depth={:?}, max_elements={:?}, visible_only={}, include_raw={})",
            self.max_depth, self.max_elements, self.visible_only, self.include_raw
        )
    }
}

impl From<&QueryOptions> for xa11y_core::QueryOptions {
    fn from(o: &QueryOptions) -> Self {
        xa11y_core::QueryOptions {
            max_depth: o.max_depth,
            max_elements: o.max_elements,
            visible_only: o.visible_only,
            roles: None,
            include_raw: o.include_raw,
        }
    }
}

// ── Tree ─────────────────────────────────────────────────────────────

/// An immutable snapshot of an application's accessibility tree.
#[pyclass(frozen)]
pub struct Tree {
    inner: xa11y_core::Tree,
    py_nodes: Vec<Node>,
}

impl Tree {
    fn from_rust(tree: xa11y_core::Tree) -> Self {
        let py_nodes: Vec<Node> = tree.iter().map(Node::from).collect();
        Tree {
            inner: tree,
            py_nodes,
        }
    }
}

#[pymethods]
impl Tree {
    /// Application name.
    #[getter]
    fn app_name(&self) -> &str {
        &self.inner.app_name
    }

    /// Process ID (None for multi-app queries).
    #[getter]
    fn pid(&self) -> Option<u32> {
        self.inner.pid
    }

    /// Screen dimensions (width, height).
    #[getter]
    fn screen_size(&self) -> (u32, u32) {
        self.inner.screen_size
    }

    /// Number of nodes.
    fn __len__(&self) -> usize {
        self.py_nodes.len()
    }

    /// Get the root node.
    fn root(&self) -> Node {
        self.py_nodes[0].clone()
    }

    /// Get parent of a node.
    fn parent(&self, node: &Node) -> Option<Node> {
        node.parent_index
            .and_then(|idx| self.py_nodes.get(idx as usize))
            .cloned()
    }

    /// Get direct children of a node.
    fn children(&self, node: &Node) -> Vec<Node> {
        node.children_indices
            .iter()
            .filter_map(|&idx| self.py_nodes.get(idx as usize))
            .cloned()
            .collect()
    }

    /// Get all nodes in the subtree rooted at a node.
    fn subtree(&self, node: &Node) -> Vec<Node> {
        let mut result = Vec::new();
        self.collect_subtree(node.index, &mut result);
        result
    }

    /// All nodes in DFS order.
    #[getter]
    fn nodes(&self) -> Vec<Node> {
        self.py_nodes.clone()
    }

    /// Query nodes matching a CSS-like selector.
    fn query(&self, selector: &str) -> PyResult<Vec<Node>> {
        let matches = self.inner.query(selector).map_err(to_py_err)?;
        Ok(matches.iter().map(|n| Node::from(*n)).collect())
    }

    /// Find nodes by role.
    fn find_by_role(&self, role: Role) -> Vec<Node> {
        self.py_nodes
            .iter()
            .filter(|n| n.role == role)
            .cloned()
            .collect()
    }

    /// Find nodes by name (substring, case-insensitive).
    fn find_by_name(&self, pattern: &str) -> Vec<Node> {
        let pattern_lower = pattern.to_lowercase();
        self.py_nodes
            .iter()
            .filter(|n| {
                n.name
                    .as_ref()
                    .is_some_and(|name| name.to_lowercase().contains(&pattern_lower))
            })
            .cloned()
            .collect()
    }

    /// Render the tree as indented text for debugging.
    fn dump(&self) -> String {
        self.inner.dump()
    }

    fn __repr__(&self) -> String {
        format!(
            "Tree(app=\"{}\", nodes={})",
            self.inner.app_name,
            self.py_nodes.len()
        )
    }
}

impl Tree {
    fn collect_subtree(&self, index: u32, result: &mut Vec<Node>) {
        if let Some(node) = self.py_nodes.get(index as usize) {
            result.push(node.clone());
            for &child_idx in &node.children_indices {
                self.collect_subtree(child_idx, result);
            }
        }
    }
}

// ── Provider ─────────────────────────────────────────────────────────

/// The accessibility provider. Created via `create_provider()`.
#[pyclass]
pub struct Provider {
    inner: Arc<Box<dyn xa11y_core::Provider>>,
}

fn parse_target(target: &str) -> xa11y_core::AppTarget {
    if let Ok(pid) = target.parse::<u32>() {
        xa11y_core::AppTarget::ByPid(pid)
    } else {
        xa11y_core::AppTarget::ByName(target.to_string())
    }
}

#[pymethods]
impl Provider {
    /// Get the accessibility tree for an application.
    ///
    /// Args:
    ///     target: Application name (substring match) or PID as string.
    ///     options: Optional QueryOptions.
    #[pyo3(signature = (target, options=None))]
    fn get_tree(&self, target: &str, options: Option<&QueryOptions>) -> PyResult<Tree> {
        let app_target = parse_target(target);
        let opts = options
            .map(xa11y_core::QueryOptions::from)
            .unwrap_or_default();
        let tree = self
            .inner
            .get_app_tree(&app_target, &opts)
            .map_err(to_py_err)?;
        Ok(Tree::from_rust(tree))
    }

    /// Get the accessibility tree for an application by PID.
    #[pyo3(signature = (pid, options=None))]
    fn get_tree_by_pid(&self, pid: u32, options: Option<&QueryOptions>) -> PyResult<Tree> {
        let opts = options
            .map(xa11y_core::QueryOptions::from)
            .unwrap_or_default();
        let tree = self
            .inner
            .get_app_tree(&xa11y_core::AppTarget::ByPid(pid), &opts)
            .map_err(to_py_err)?;
        Ok(Tree::from_rust(tree))
    }

    /// Get a shallow tree of all running applications.
    #[pyo3(signature = (options=None))]
    fn get_all_apps(&self, options: Option<&QueryOptions>) -> PyResult<Tree> {
        let opts = options
            .map(xa11y_core::QueryOptions::from)
            .unwrap_or_default();
        let tree = self.inner.get_all_apps(&opts).map_err(to_py_err)?;
        Ok(Tree::from_rust(tree))
    }

    /// Perform an action on a node from a tree snapshot.
    #[pyo3(signature = (tree, node_index, action, value=None))]
    fn perform_action(
        &self,
        tree: &Tree,
        node_index: u32,
        action: Action,
        value: Option<&str>,
    ) -> PyResult<()> {
        let rust_node = tree
            .inner
            .get(node_index)
            .ok_or_else(|| PyValueError::new_err("Node index out of range"))?;
        let data = value.map(|v| xa11y_core::ActionData::Value(v.to_string()));
        self.inner
            .perform_action(&tree.inner, rust_node, action.into(), data)
            .map_err(to_py_err)
    }

    /// Check if accessibility permissions are granted.
    /// Returns True if granted, raises RuntimeError if denied.
    fn check_permissions(&self) -> PyResult<bool> {
        match self.inner.check_permissions().map_err(to_py_err)? {
            xa11y_core::PermissionStatus::Granted => Ok(true),
            xa11y_core::PermissionStatus::Denied { instructions } => Err(
                PyRuntimeError::new_err(format!("Accessibility denied: {instructions}")),
            ),
        }
    }

    /// List running applications.
    fn list_apps(&self) -> PyResult<Vec<AppInfo>> {
        let apps = self.inner.list_apps().map_err(to_py_err)?;
        Ok(apps.into_iter().map(AppInfo::from).collect())
    }

    /// Create a Locator for lazy element resolution (Playwright-style).
    ///
    /// Args:
    ///     target: Application name or PID string.
    ///     selector: CSS-like accessibility selector.
    ///     options: Optional QueryOptions.
    #[pyo3(signature = (target, selector, options=None))]
    fn locator(&self, target: &str, selector: &str, options: Option<QueryOptions>) -> Locator {
        Locator {
            provider: Arc::clone(&self.inner),
            target: parse_target(target),
            selector: selector.to_string(),
            opts: options
                .as_ref()
                .map(xa11y_core::QueryOptions::from)
                .unwrap_or_default(),
            nth: None,
        }
    }

    fn __repr__(&self) -> String {
        "Provider()".to_string()
    }
}

// ── Locator ──────────────────────────────────────────────────────────

/// Lazy element descriptor that re-resolves on every operation.
/// Inspired by Playwright's Locator pattern — never holds a live
/// reference to a UI element, so it's immune to staleness.
#[pyclass]
#[derive(Clone)]
pub struct Locator {
    provider: Arc<Box<dyn xa11y_core::Provider>>,
    target: xa11y_core::AppTarget,
    selector: String,
    opts: xa11y_core::QueryOptions,
    nth: Option<usize>,
}

impl Locator {
    fn resolve(&self) -> Result<(xa11y_core::Tree, u32), PyErr> {
        let tree = self
            .provider
            .get_app_tree(&self.target, &self.opts)
            .map_err(to_py_err)?;
        let matches = tree.query(&self.selector).map_err(to_py_err)?;
        let idx = self.nth.unwrap_or(0);
        let node = matches.get(idx).ok_or_else(|| {
            PyRuntimeError::new_err(format!("No element matched selector: {}", self.selector))
        })?;
        let node_index = node.index;
        Ok((tree, node_index))
    }

    fn resolve_node(&self) -> PyResult<Node> {
        let (tree, idx) = self.resolve()?;
        let rust_node = tree
            .get(idx)
            .ok_or_else(|| PyRuntimeError::new_err("Node disappeared after resolve"))?;
        Ok(Node::from(rust_node))
    }

    fn do_action(
        &self,
        action: xa11y_core::Action,
        data: Option<xa11y_core::ActionData>,
    ) -> PyResult<()> {
        let (tree, idx) = self.resolve()?;
        let node = tree
            .get(idx)
            .ok_or_else(|| PyRuntimeError::new_err("Node disappeared after resolve"))?;
        self.provider
            .perform_action(&tree, node, action, data)
            .map_err(to_py_err)
    }

    fn poll_state(&self, state: xa11y_core::ElementState, timeout_secs: f64) -> PyResult<Node> {
        let timeout = Duration::from_secs_f64(timeout_secs);
        let poll_interval = Duration::from_millis(100);
        let start = std::time::Instant::now();

        loop {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(PyTimeoutError::new_err(format!(
                    "Timeout after {elapsed:?} waiting for {state:?}",
                )));
            }

            let tree = self
                .provider
                .get_app_tree(&self.target, &self.opts)
                .map_err(to_py_err)?;
            let matches = tree.query(&self.selector).ok();
            let idx = self.nth.unwrap_or(0);
            let node = matches.as_ref().and_then(|m| m.get(idx).copied());

            let met = match state {
                xa11y_core::ElementState::Attached => node.is_some(),
                xa11y_core::ElementState::Detached => node.is_none(),
                xa11y_core::ElementState::Visible => node.is_some_and(|n| n.states.visible),
                xa11y_core::ElementState::Hidden => {
                    node.is_none() || node.is_some_and(|n| !n.states.visible)
                }
                xa11y_core::ElementState::Enabled => node.is_some_and(|n| n.states.enabled),
            };

            if met {
                return Ok(node.map(Node::from).unwrap_or_else(|| Node {
                    role: Role::Unknown,
                    name: None,
                    value: None,
                    description: None,
                    bounds: None,
                    bounds_normalized: None,
                    actions_inner: vec![],
                    states_inner: StateSet {
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
                    },
                    depth: 0,
                    numeric_value: None,
                    min_value: None,
                    max_value: None,
                    stable_id: None,
                    index: 0,
                    children_indices: vec![],
                    parent_index: None,
                }));
            }

            std::thread::sleep(poll_interval);
        }
    }
}

#[pymethods]
impl Locator {
    /// The selector string.
    #[getter]
    fn selector(&self) -> &str {
        &self.selector
    }

    /// Return a new Locator selecting the nth match (0-based).
    fn nth(&self, n: usize) -> Locator {
        let mut loc = self.clone();
        loc.nth = Some(n);
        loc
    }

    /// Return a new Locator selecting the first match.
    fn first(&self) -> Locator {
        self.nth(0)
    }

    /// Return a new Locator scoped to a direct child.
    fn child(&self, selector: &str) -> Locator {
        let mut loc = self.clone();
        loc.selector = format!("{} > {}", self.selector, selector);
        loc.nth = None;
        loc
    }

    /// Return a new Locator scoped to a descendant.
    fn descendant(&self, selector: &str) -> Locator {
        let mut loc = self.clone();
        loc.selector = format!("{} {}", self.selector, selector);
        loc.nth = None;
        loc
    }

    // ── Query methods ────────────────────────────────────────────

    /// Get the matched element's role.
    fn role(&self) -> PyResult<Role> {
        Ok(self.resolve_node()?.role)
    }

    /// Get the matched element's name.
    fn name(&self) -> PyResult<Option<String>> {
        Ok(self.resolve_node()?.name)
    }

    /// Get the matched element's value.
    fn value(&self) -> PyResult<Option<String>> {
        Ok(self.resolve_node()?.value)
    }

    /// Get the matched element's description.
    fn description(&self) -> PyResult<Option<String>> {
        Ok(self.resolve_node()?.description)
    }

    /// Get the matched element's bounding rectangle.
    fn bounds(&self) -> PyResult<Option<Rect>> {
        Ok(self.resolve_node()?.bounds)
    }

    /// Get the matched element's state flags.
    fn states(&self) -> PyResult<StateSet> {
        Ok(self.resolve_node()?.states_inner)
    }

    /// Get the matched element's numeric value.
    fn numeric_value(&self) -> PyResult<Option<f64>> {
        Ok(self.resolve_node()?.numeric_value)
    }

    /// Check if the matched element is visible.
    fn is_visible(&self) -> PyResult<bool> {
        Ok(self.resolve_node()?.states_inner.visible)
    }

    /// Check if the matched element is enabled.
    fn is_enabled(&self) -> PyResult<bool> {
        Ok(self.resolve_node()?.states_inner.enabled)
    }

    /// Check if the matched element is focused.
    fn is_focused(&self) -> PyResult<bool> {
        Ok(self.resolve_node()?.states_inner.focused)
    }

    /// Check if a matching element exists.
    fn exists(&self) -> PyResult<bool> {
        match self.resolve() {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Count matching elements.
    fn count(&self) -> PyResult<usize> {
        let tree = self
            .provider
            .get_app_tree(&self.target, &self.opts)
            .map_err(to_py_err)?;
        let matches = tree.query(&self.selector).map_err(to_py_err)?;
        Ok(matches.len())
    }

    /// Get a snapshot of the matched node.
    fn get(&self) -> PyResult<Node> {
        self.resolve_node()
    }

    // ── Action methods ───────────────────────────────────────────

    /// Click / invoke the matched element.
    fn press(&self) -> PyResult<()> {
        self.do_action(xa11y_core::Action::Press, None)
    }

    /// Set keyboard focus.
    fn focus(&self) -> PyResult<()> {
        self.do_action(xa11y_core::Action::Focus, None)
    }

    /// Toggle checkbox or switch.
    fn toggle(&self) -> PyResult<()> {
        self.do_action(xa11y_core::Action::Toggle, None)
    }

    /// Select a list/table item.
    fn select(&self) -> PyResult<()> {
        self.do_action(xa11y_core::Action::Select, None)
    }

    /// Expand a collapsible element.
    fn expand(&self) -> PyResult<()> {
        self.do_action(xa11y_core::Action::Expand, None)
    }

    /// Collapse an expanded element.
    fn collapse(&self) -> PyResult<()> {
        self.do_action(xa11y_core::Action::Collapse, None)
    }

    /// Set text value.
    fn set_value(&self, value: &str) -> PyResult<()> {
        self.do_action(
            xa11y_core::Action::SetValue,
            Some(xa11y_core::ActionData::Value(value.to_string())),
        )
    }

    /// Set numeric value (slider, spinner).
    fn set_numeric_value(&self, value: f64) -> PyResult<()> {
        self.do_action(
            xa11y_core::Action::SetValue,
            Some(xa11y_core::ActionData::NumericValue(value)),
        )
    }

    /// Increment slider or spinner.
    fn increment(&self) -> PyResult<()> {
        self.do_action(xa11y_core::Action::Increment, None)
    }

    /// Decrement slider or spinner.
    fn decrement(&self) -> PyResult<()> {
        self.do_action(xa11y_core::Action::Decrement, None)
    }

    /// Show context menu.
    fn show_menu(&self) -> PyResult<()> {
        self.do_action(xa11y_core::Action::ShowMenu, None)
    }

    /// Scroll element into view.
    fn scroll_into_view(&self) -> PyResult<()> {
        self.do_action(xa11y_core::Action::ScrollIntoView, None)
    }

    /// Type text via accessibility API.
    fn type_text(&self, text: &str) -> PyResult<()> {
        self.do_action(
            xa11y_core::Action::TypeText,
            Some(xa11y_core::ActionData::Value(text.to_string())),
        )
    }

    /// Select a text range.
    fn select_text(&self, start: u32, end: u32) -> PyResult<()> {
        self.do_action(
            xa11y_core::Action::SetTextSelection,
            Some(xa11y_core::ActionData::TextSelection { start, end }),
        )
    }

    /// Scroll in a direction.
    #[pyo3(signature = (direction, amount=1.0))]
    fn scroll(&self, direction: ScrollDirection, amount: f64) -> PyResult<()> {
        self.do_action(
            xa11y_core::Action::Scroll,
            Some(xa11y_core::ActionData::ScrollAmount {
                direction: direction.into(),
                amount,
            }),
        )
    }

    // ── Wait methods ─────────────────────────────────────────────

    /// Wait until the element is visible.
    #[pyo3(signature = (timeout_secs=5.0))]
    fn wait_visible(&self, timeout_secs: f64) -> PyResult<Node> {
        self.poll_state(xa11y_core::ElementState::Visible, timeout_secs)
    }

    /// Wait until the element exists in the tree.
    #[pyo3(signature = (timeout_secs=5.0))]
    fn wait_attached(&self, timeout_secs: f64) -> PyResult<Node> {
        self.poll_state(xa11y_core::ElementState::Attached, timeout_secs)
    }

    /// Wait until the element is removed from the tree.
    #[pyo3(signature = (timeout_secs=5.0))]
    fn wait_detached(&self, timeout_secs: f64) -> PyResult<Node> {
        self.poll_state(xa11y_core::ElementState::Detached, timeout_secs)
    }

    /// Wait until the element is enabled.
    #[pyo3(signature = (timeout_secs=5.0))]
    fn wait_enabled(&self, timeout_secs: f64) -> PyResult<Node> {
        self.poll_state(xa11y_core::ElementState::Enabled, timeout_secs)
    }

    /// Wait until the element is hidden or removed.
    #[pyo3(signature = (timeout_secs=5.0))]
    fn wait_hidden(&self, timeout_secs: f64) -> PyResult<Node> {
        self.poll_state(xa11y_core::ElementState::Hidden, timeout_secs)
    }

    fn __repr__(&self) -> String {
        format!("Locator(\"{}\")", self.selector)
    }
}

// ── Module ───────────────────────────────────────────────────────────

/// Create the platform accessibility provider.
#[pyfunction]
fn create_provider() -> PyResult<Provider> {
    let inner = xa11y::create_provider().map_err(to_py_err)?;
    Ok(Provider {
        inner: Arc::new(inner),
    })
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(create_provider, m)?)?;
    m.add_class::<Role>()?;
    m.add_class::<Action>()?;
    m.add_class::<ScrollDirection>()?;
    m.add_class::<Toggled>()?;
    m.add_class::<Rect>()?;
    m.add_class::<NormalizedRect>()?;
    m.add_class::<StateSet>()?;
    m.add_class::<Node>()?;
    m.add_class::<AppInfo>()?;
    m.add_class::<QueryOptions>()?;
    m.add_class::<Tree>()?;
    m.add_class::<Provider>()?;
    m.add_class::<Locator>()?;
    Ok(())
}
