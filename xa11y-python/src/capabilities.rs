//! Python surface for the capability table (`xa11y_core::capabilities`).
//!
//! Platform and support values cross as the core's snake_case spellings
//! (`"linux"`, `"partial"`). Nothing here touches an app, so nothing blocks
//! and the GIL is never released.

use pyo3::exceptions::PyKeyError;
use pyo3::prelude::*;

use crate::to_py_err;

/// A Python string literal for `__repr__`: single-quoted, as `repr(str)`
/// prints the common case.
fn py_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn py_opt(s: Option<&str>) -> String {
    s.map_or_else(|| "None".to_string(), py_str)
}

/// One feature's answer on one platform.
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct Capability {
    inner: xa11y::Capability,
}

#[pymethods]
impl Capability {
    /// Feature name, ``<area>:<name>`` (``"event:menu_opened"``).
    #[getter]
    fn name(&self) -> &'static str {
        self.inner.name
    }
    /// What the feature is, in one sentence.
    #[getter]
    fn description(&self) -> &'static str {
        self.inner.description
    }
    /// ``"supported"``, ``"partial"`` or ``"unsupported"``.
    #[getter]
    fn support(&self) -> &'static str {
        self.inner.support.to_snake_case()
    }
    /// How the platform provides it, or why it does not; ``None`` when there
    /// is nothing to add.
    #[getter]
    fn note(&self) -> Option<String> {
        self.inner.note.clone()
    }
    /// The first OS version that supports it, when older ones do not.
    #[getter]
    fn since(&self) -> Option<&'static str> {
        self.inner.since
    }
    fn __repr__(&self) -> String {
        let head = format!(
            "Capability({}, {}",
            py_str(self.inner.name),
            py_str(self.inner.support.to_snake_case())
        );
        match &self.inner.note {
            Some(note) => format!("{head}, note={})", py_str(note)),
            None => format!("{head})"),
        }
    }
}

/// Every feature's answer on one platform. Get one from
/// :func:`xa11y.capabilities`.
///
/// Iterating yields :class:`Capability` objects in table order; ``name in
/// caps`` tests whether a feature name is known.
#[pyclass(frozen)]
pub(crate) struct Capabilities {
    inner: xa11y::Capabilities,
}

#[pymethods]
impl Capabilities {
    /// The accessibility source described.
    #[getter]
    fn backend(&self) -> &'static str {
        self.inner.backend
    }
    /// Every feature name the table knows, in table order.
    #[staticmethod]
    fn names() -> Vec<&'static str> {
        xa11y::Capabilities::names().collect()
    }

    /// The platform described: ``"linux"``, ``"macos"`` or ``"windows"``.
    #[getter]
    fn platform(&self) -> &'static str {
        self.inner.platform.to_snake_case()
    }

    /// The OS version the answers were narrowed to, or ``None`` when none was
    /// given or detected. Always ``None`` on Linux.
    #[getter]
    fn os_version(&self) -> Option<String> {
        self.inner.os_version.clone()
    }

    /// Every feature, in table order.
    fn entries(&self) -> Vec<Capability> {
        self.inner
            .entries()
            .iter()
            .map(|c| Capability { inner: c.clone() })
            .collect()
    }

    /// The capability named ``name``. Raises :class:`InvalidActionDataError`
    /// for an unknown name, listing the closest known ones.
    fn get(&self, name: &str) -> PyResult<Capability> {
        self.inner
            .get(name)
            .map(|c| Capability { inner: c.clone() })
            .map_err(to_py_err)
    }

    /// Shorthand for ``get(name).support``.
    fn support(&self, name: &str) -> PyResult<&'static str> {
        self.inner
            .support(name)
            .map(|s| s.to_snake_case())
            .map_err(to_py_err)
    }

    /// Shorthand for ``get(name).note``.
    fn note(&self, name: &str) -> PyResult<Option<String>> {
        self.inner
            .note(name)
            .map(|n| n.map(str::to_string))
            .map_err(to_py_err)
    }

    /// Whether ``name`` is a feature the table knows (also ``name in caps``).
    fn has(&self, name: &str) -> bool {
        self.inner.has(name)
    }

    fn __len__(&self) -> usize {
        self.inner.entries().len()
    }

    fn __contains__(&self, name: &Bound<'_, PyAny>) -> bool {
        // Like `dict`: a key of another type is simply not present.
        match name.extract::<String>() {
            Ok(name) => self.inner.has(&name),
            Err(_) => false,
        }
    }

    fn __getitem__(&self, name: &str) -> PyResult<Capability> {
        // Mapping protocol: an unknown key is a KeyError, as `dict` raises;
        // `get()` raises the library's own error with suggestions.
        self.inner
            .get(name)
            .map(|c| Capability { inner: c.clone() })
            .map_err(|e| PyKeyError::new_err(e.to_string()))
    }

    fn __iter__(&self) -> CapabilityIter {
        CapabilityIter {
            items: self.entries(),
            next: 0,
        }
    }

    fn __repr__(&self) -> String {
        let count = |s: xa11y::Support| {
            self.inner
                .entries()
                .iter()
                .filter(|c| c.support == s)
                .count()
        };
        format!(
            "Capabilities(platform={}, os_version={}, supported={}, partial={}, unsupported={})",
            py_str(self.inner.platform.to_snake_case()),
            py_opt(self.inner.os_version.as_deref()),
            count(xa11y::Support::Supported),
            count(xa11y::Support::Partial),
            count(xa11y::Support::Unsupported),
        )
    }
}

#[pyclass]
pub(crate) struct CapabilityIter {
    items: Vec<Capability>,
    next: usize,
}

#[pymethods]
impl CapabilityIter {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }
    fn __next__(&mut self) -> Option<Capability> {
        let item = self.items.get(self.next).cloned();
        self.next += 1;
        item
    }
}

/// What a platform's backend can report: one answer per feature, read from a
/// static table without connecting to any app.
///
/// With no arguments, describes the running platform and OS version. Pass
/// ``platform`` (``"linux"``, ``"macos"``, ``"windows"``) to describe another
/// one, and ``os_version`` to narrow it to an OS release.
#[pyfunction]
#[pyo3(signature = (platform=None, os_version=None, *, backend=None))]
pub(crate) fn capabilities(
    py: Python<'_>,
    platform: Option<&str>,
    os_version: Option<&str>,
    backend: Option<&str>,
) -> PyResult<Capabilities> {
    let platform = platform
        .map(xa11y::Platform::parse)
        .transpose()
        .map_err(to_py_err)?;
    // The first call for the running platform reads the OS version, which
    // spawns `sysctl` / `cmd /C ver`: release the GIL for it (tenet 5).
    let inner = py
        .detach(|| xa11y::capabilities_for_backend(platform, os_version, backend))
        .map_err(to_py_err)?;
    Ok(Capabilities { inner })
}
