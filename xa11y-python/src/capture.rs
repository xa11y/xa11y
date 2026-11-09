//! Python surface for bounded capture (#482).
//!
//! `capture(...)` on `Element`, `App`, `Locator` and `ShellSurface` takes
//! the options as keyword arguments (the options-struct fold) and returns a
//! [`Capture`]. Nodes refer to each other by index into `Capture.nodes`
//! (`parent`, `children`, and in-capture relation targets), never by object,
//! so a capture holds no reference cycles.

use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

/// Build core options from the keyword arguments. Runs before any platform
/// call, with the GIL held.
pub(crate) fn parse_options(
    max_depth: Option<usize>,
    max_nodes: usize,
    max_text_bytes: usize,
    detail: Option<&Bound<'_, PyAny>>,
    timeout: Option<f64>,
    max_runs: usize,
    cancel: Option<&CancelToken>,
) -> PyResult<xa11y::CaptureOptions> {
    let mut options = xa11y::CaptureOptions::new()
        .max_nodes(max_nodes)
        .max_text_bytes(max_text_bytes)
        .max_runs(max_runs);
    if let Some(token) = cancel {
        options = options.cancel(token.inner.clone());
    }
    if let Some(depth) = max_depth {
        options = options.max_depth(depth);
    }
    let names: Vec<String> = match detail {
        None => Vec::new(),
        // A bare string is iterable, as its characters: say what was meant.
        Some(d) if d.is_instance_of::<pyo3::types::PyString>() => {
            return Err(PyValueError::new_err(
                "detail is a collection of names, such as {\"text\"}, not a string",
            ))
        }
        Some(d) => d
            .try_iter()?
            .map(|item| item?.extract::<String>())
            .collect::<PyResult<_>>()?,
    };
    for name in names {
        options = match name.as_str() {
            "text" => options.text(true),
            "relations" => options.relations(true),
            "formatting" => options.formatting(true),
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown capture detail {other:?}; expected \"text\", \"formatting\" \
                     or \"relations\""
                )))
            }
        };
    }
    if let Some(seconds) = timeout {
        options = options.timeout(crate::parse_duration(seconds)?);
    }
    Ok(options)
}

fn epoch_seconds(t: std::time::SystemTime) -> f64 {
    // A clock before 1970 reads as a negative offset rather than failing.
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    }
}

/// Convert a core capture, with the GIL held.
pub(crate) fn to_py(py: Python<'_>, cap: xa11y::Capture) -> PyResult<Capture> {
    let nodes = cap
        .nodes
        .iter()
        .map(|node| Py::new(py, CapturedNode::from_core(py, node)?))
        .collect::<PyResult<Vec<_>>>()?;
    Ok(Capture {
        nodes,
        truncated: Truncation {
            inner: cap.truncated,
        },
        vanished: cap.vanished,
        started: epoch_seconds(cap.started),
        finished: epoch_seconds(cap.finished),
        backend: cap.backend,
        stats: CaptureStats { inner: cap.stats },
    })
}

/// Stops a capture from another thread: pass it as ``capture(cancel=...)``
/// and call :meth:`cancel`. The capture returns what it read, with
/// ``truncated.by_cancel``.
#[pyclass(frozen)]
pub(crate) struct CancelToken {
    inner: xa11y::CancelToken,
}

#[pymethods]
impl CancelToken {
    #[new]
    fn new() -> Self {
        Self {
            inner: xa11y::CancelToken::new(),
        }
    }
    /// Cancel every capture holding this token. One not yet started stops
    /// at once.
    fn cancel(&self) {
        self.inner.cancel();
    }
    /// Whether :meth:`cancel` was called.
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
    fn __repr__(&self) -> String {
        format!(
            "CancelToken(cancelled={})",
            if self.inner.is_cancelled() {
                "True"
            } else {
                "False"
            }
        )
    }
}

/// The reads a capture made, by kind. A read is one call into the backend
/// for one node.
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct CaptureStats {
    inner: xa11y::CaptureStats,
}

#[pymethods]
impl CaptureStats {
    /// Children reads: one per node whose children were read.
    #[getter]
    fn children_reads(&self) -> usize {
        self.inner.children_reads
    }
    /// Identity reads, for cycle detection and relation resolution.
    #[getter]
    fn identity_reads(&self) -> usize {
        self.inner.identity_reads
    }
    /// Text reads, with ``"text"`` detail.
    #[getter]
    fn text_reads(&self) -> usize {
        self.inner.text_reads
    }
    /// Formatting-run reads, with ``"formatting"`` detail: one per run.
    #[getter]
    fn formatting_reads(&self) -> usize {
        self.inner.formatting_reads
    }
    /// Relation reads, with ``"relations"`` detail: one per node.
    #[getter]
    fn relation_reads(&self) -> usize {
        self.inner.relation_reads
    }
    /// Bytes of text in the capture, as UTF-8.
    #[getter]
    fn text_bytes(&self) -> usize {
        self.inner.text_bytes
    }
    fn __repr__(&self) -> String {
        let s = &self.inner;
        format!(
            "CaptureStats(children_reads={}, identity_reads={}, text_reads={}, \
             formatting_reads={}, relation_reads={}, text_bytes={})",
            s.children_reads,
            s.identity_reads,
            s.text_reads,
            s.formatting_reads,
            s.relation_reads,
            s.text_bytes
        )
    }
}

/// A bounded read of a subtree: every node read, pre-order, and which limits
/// cut it. See ``Element.capture``.
#[pyclass(frozen)]
pub(crate) struct Capture {
    nodes: Vec<Py<CapturedNode>>,
    truncated: Truncation,
    vanished: usize,
    started: f64,
    finished: f64,
    backend: Option<&'static str>,
    stats: CaptureStats,
}

#[pymethods]
impl Capture {
    /// Every node read, pre-order. The root is index 0.
    #[getter]
    fn nodes(&self, py: Python<'_>) -> Vec<Py<CapturedNode>> {
        self.nodes.iter().map(|n| n.clone_ref(py)).collect()
    }
    /// Which limits dropped something.
    #[getter]
    fn truncated(&self) -> Truncation {
        self.truncated.clone()
    }
    /// Nodes that disappeared while the capture read them, and were dropped.
    #[getter]
    fn vanished(&self) -> usize {
        self.vanished
    }
    /// When the walk began, in seconds since the epoch.
    /// The source that read the tree: ``"atspi"``, ``"ax"``, ``"uia"`` or
    /// ``"ia2"``; ``None`` when the root reported no identity.
    #[getter]
    fn backend(&self) -> Option<&'static str> {
        self.backend
    }
    /// The reads the capture made, by kind.
    #[getter]
    fn stats(&self) -> CaptureStats {
        self.stats.clone()
    }
    #[getter]
    fn started(&self) -> f64 {
        self.started
    }
    /// When the walk ended, in seconds since the epoch.
    #[getter]
    fn finished(&self) -> f64 {
        self.finished
    }
    /// The root node.
    fn root(&self, py: Python<'_>) -> Py<CapturedNode> {
        self.nodes[0].clone_ref(py)
    }
    /// The nodes of ``index``'s subtree, itself first, in pre-order. Empty
    /// when ``index`` is out of range.
    fn subtree(&self, py: Python<'_>, index: usize) -> Vec<Py<CapturedNode>> {
        let Some(start) = self.nodes.get(index) else {
            return Vec::new();
        };
        let depth = start.get().depth;
        let mut out = vec![start.clone_ref(py)];
        out.extend(
            self.nodes[index + 1..]
                .iter()
                .take_while(|n| n.get().depth > depth)
                .map(|n| n.clone_ref(py)),
        );
        out
    }
    fn __len__(&self) -> usize {
        self.nodes.len()
    }
    fn __repr__(&self) -> String {
        format!(
            "Capture(nodes={}, truncated={}, vanished={})",
            self.nodes.len(),
            self.truncated.__repr__(),
            self.vanished
        )
    }
}

/// Which limits dropped something.
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct Truncation {
    inner: xa11y::Truncation,
}

#[pymethods]
impl Truncation {
    /// Children below ``max_depth`` were left out.
    #[getter]
    fn by_depth(&self) -> bool {
        self.inner.by_depth
    }
    /// Nodes past ``max_nodes`` were left out.
    #[getter]
    fn by_nodes(&self) -> bool {
        self.inner.by_nodes
    }
    /// Text past ``max_text_bytes`` was left out.
    #[getter]
    fn by_text(&self) -> bool {
        self.inner.by_text
    }
    /// The time budget ran out before the walk finished.
    #[getter]
    fn by_time(&self) -> bool {
        self.inner.by_time
    }
    /// Formatting runs past ``max_runs`` were left out.
    #[getter]
    fn by_formatting(&self) -> bool {
        self.inner.by_formatting
    }
    /// The capture's :class:`CancelToken` was cancelled before it finished.
    #[getter]
    fn by_cancel(&self) -> bool {
        self.inner.by_cancel
    }
    /// Whether any limit dropped something.
    fn any(&self) -> bool {
        self.inner.any()
    }
    fn __bool__(&self) -> bool {
        self.inner.any()
    }
    fn __repr__(&self) -> String {
        let b = |v: bool| if v { "True" } else { "False" };
        format!(
            "Truncation(by_depth={}, by_nodes={}, by_text={}, by_time={}, by_formatting={}, \
             by_cancel={})",
            b(self.inner.by_depth),
            b(self.inner.by_nodes),
            b(self.inner.by_text),
            b(self.inner.by_time),
            b(self.inner.by_formatting),
            b(self.inner.by_cancel)
        )
    }
}

/// An element's text, as far as the capture's text budget reached.
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct CapturedText {
    inner: xa11y::CapturedText,
}

#[pymethods]
impl CapturedText {
    /// The text read, from offset 0.
    #[getter]
    fn content(&self) -> String {
        self.inner.content.clone()
    }
    /// The element's full text length, in UTF-16 code units.
    #[getter]
    fn length(&self) -> u32 {
        self.inner.length
    }
    /// Whether ``content`` stops short of ``length``.
    #[getter]
    fn truncated(&self) -> bool {
        self.inner.truncated
    }
    /// With ``"formatting"`` detail, the :class:`TextRun` s covering
    /// ``content``, in order; ``None`` when they were not read.
    #[getter]
    fn runs(&self) -> Option<Vec<crate::rich::TextRun>> {
        self.inner.runs.as_ref().map(|runs| {
            runs.iter()
                .cloned()
                .map(crate::rich::TextRun::from_core)
                .collect()
        })
    }
    /// Whether ``runs`` stops short of the end of ``content``.
    #[getter]
    fn runs_truncated(&self) -> bool {
        self.inner.runs_truncated
    }
    fn __repr__(&self) -> String {
        format!(
            "CapturedText(length={}, truncated={}, content={:?})",
            self.inner.length, self.inner.truncated, self.inner.content
        )
    }
}

/// A read of one node that failed. The capture continued past it.
#[pyclass(frozen)]
pub(crate) struct CaptureError {
    step: &'static str,
    error: Py<PyAny>,
}

#[pymethods]
impl CaptureError {
    /// What was being read: ``"children"``, ``"identity"``, ``"text"``,
    /// ``"relations"`` or ``"cycle"``.
    #[getter]
    fn step(&self) -> &'static str {
        self.step
    }
    /// The exception the read raised, as an xa11y exception instance.
    #[getter]
    fn error(&self, py: Python<'_>) -> Py<PyAny> {
        self.error.clone_ref(py)
    }
    fn __repr__(&self, py: Python<'_>) -> String {
        let shown = self
            .error
            .bind(py)
            .repr()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| "<error>".into());
        format!("CaptureError(step='{}', error={shown})", self.step)
    }
}

/// One node of a :class:`Capture`.
#[pyclass(frozen)]
pub(crate) struct CapturedNode {
    index: usize,
    parent: Option<usize>,
    depth: usize,
    element: Py<crate::Element>,
    children: Vec<usize>,
    children_count: Option<usize>,
    children_truncated: bool,
    errors: Vec<Py<CaptureError>>,
    text: Option<CapturedText>,
    /// kind → targets: an ``int`` index for a node inside the capture, an
    /// ``Element`` for one outside it.
    relations: Py<PyDict>,
}

impl CapturedNode {
    fn from_core(py: Python<'_>, node: &xa11y::CapturedNode) -> PyResult<Self> {
        let provider: Arc<dyn xa11y::Provider> = node.element.provider().clone();
        let element = crate::make_py_element(py, node.element.data(), provider)?;
        let errors = node
            .errors
            .iter()
            .map(|e| {
                let err = crate::to_py_err(e.error.clone());
                Py::new(
                    py,
                    CaptureError {
                        step: e.step.to_snake_case(),
                        error: err.value(py).clone().into_any().unbind(),
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;
        let relations = PyDict::new(py);
        for (kind, targets) in &node.relations {
            let list = PyList::empty(py);
            for target in targets {
                match target {
                    xa11y::RelationTarget::Node(i) => list.append(*i)?,
                    xa11y::RelationTarget::Outside(el) => list.append(crate::make_py_element(
                        py,
                        el.data(),
                        el.provider().clone(),
                    )?)?,
                }
            }
            relations.set_item(kind.to_snake_case(), list)?;
        }
        Ok(Self {
            index: node.index,
            parent: node.parent,
            depth: node.depth,
            element,
            children: node.children.clone(),
            children_count: node.children_count,
            children_truncated: node.children_truncated,
            errors,
            text: node.text.clone().map(|inner| CapturedText { inner }),
            relations: relations.unbind(),
        })
    }
}

#[pymethods]
impl CapturedNode {
    /// Index in ``Capture.nodes``, which is pre-order. 0-based.
    #[getter]
    fn index(&self) -> usize {
        self.index
    }
    /// The parent's index; ``None`` on the root.
    #[getter]
    fn parent(&self) -> Option<usize> {
        self.parent
    }
    /// Depth below the root, which is 0.
    #[getter]
    fn depth(&self) -> usize {
        self.depth
    }
    /// The element, which names exactly this node: actions on it reach it or
    /// raise :class:`ElementStaleError`, never a look-alike.
    #[getter]
    fn element(&self, py: Python<'_>) -> Py<crate::Element> {
        self.element.clone_ref(py)
    }
    /// Indices of the children in the capture, in platform order.
    #[getter]
    fn children(&self) -> Vec<usize> {
        self.children.clone()
    }
    /// How many children the node has, including ones the capture left out.
    /// ``None`` when they were not read.
    #[getter]
    fn children_count(&self) -> Option<usize> {
        self.children_count
    }
    /// Whether children of this node are missing from the capture.
    #[getter]
    fn children_truncated(&self) -> bool {
        self.children_truncated
    }
    /// Reads of this node that failed.
    #[getter]
    fn errors(&self, py: Python<'_>) -> Vec<Py<CaptureError>> {
        self.errors.iter().map(|e| e.clone_ref(py)).collect()
    }
    /// The node's text, with ``detail={"text"}``, when it has a text
    /// interface.
    #[getter]
    fn text(&self) -> Option<CapturedText> {
        self.text.clone()
    }
    /// With ``detail={"relations"}``: kind → targets, each an ``int`` index
    /// into ``Capture.nodes`` when inside the capture, or an ``Element``
    /// when outside it. Every kind the platform reports appears, ``[]`` when
    /// it has no targets.
    #[getter]
    fn relations(&self, py: Python<'_>) -> Py<PyDict> {
        self.relations.clone_ref(py)
    }
    /// Shortcut for ``element.role``.
    #[getter]
    fn role(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(self.element.bind(py).getattr("role")?.unbind())
    }
    /// Shortcut for ``element.name``.
    #[getter]
    fn name(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(self.element.bind(py).getattr("name")?.unbind())
    }
    fn __repr__(&self, py: Python<'_>) -> String {
        let element = self
            .element
            .bind(py)
            .repr()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| "<element>".into());
        format!(
            "CapturedNode(index={}, depth={}, element={element})",
            self.index, self.depth
        )
    }
}
