//! Python surface for document semantics (#476): live-region settings and
//! relation kinds. The scalar properties are `Element` getters in `lib.rs`.
//!
//! Values cross as the core's snake_case spellings: landmarks as their ARIA
//! names (`"main"`, `"contentinfo"`), politeness as `"off"` / `"polite"` /
//! `"assertive"`, relation kinds as `"labelled_by"` and so on.

use pyo3::prelude::*;

/// Live-region settings of an element. Every field is ``None`` when the
/// platform did not report it.
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct LiveRegionInfo {
    inner: xa11y::LiveRegionInfo,
}

impl LiveRegionInfo {
    pub(crate) fn new(inner: xa11y::LiveRegionInfo) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl LiveRegionInfo {
    /// ``"off"``, ``"polite"`` or ``"assertive"``.
    #[getter]
    fn politeness(&self) -> Option<&'static str> {
        self.inner.politeness.map(politeness_name)
    }
    /// Whether the whole region is announced when part of it changes.
    #[getter]
    fn atomic(&self) -> Option<bool> {
        self.inner.atomic
    }
    /// Which changes are announced, as the platform spells it
    /// (``"additions text"``).
    #[getter]
    fn relevant(&self) -> Option<String> {
        self.inner.relevant.clone()
    }
    fn __repr__(&self) -> String {
        let opt = |v: Option<String>| v.unwrap_or_else(|| "None".into());
        format!(
            "LiveRegionInfo(politeness={}, atomic={}, relevant={})",
            opt(self.politeness().map(|p| format!("'{p}'"))),
            opt(self
                .inner
                .atomic
                .map(|a| if a { "True" } else { "False" }.to_string())),
            opt(self.inner.relevant.as_ref().map(|r| format!("{r:?}"))),
        )
    }
}

/// The Python spelling of a politeness value.
pub(crate) fn politeness_name(p: xa11y::Politeness) -> &'static str {
    match p {
        xa11y::Politeness::Off => "off",
        xa11y::Politeness::Polite => "polite",
        xa11y::Politeness::Assertive => "assertive",
    }
}

/// The Python spelling of a landmark kind (its ARIA role name).
pub(crate) fn landmark_name(l: xa11y::Landmark) -> &'static str {
    match l {
        xa11y::Landmark::Banner => "banner",
        xa11y::Landmark::Complementary => "complementary",
        xa11y::Landmark::ContentInfo => "contentinfo",
        xa11y::Landmark::Form => "form",
        xa11y::Landmark::Main => "main",
        xa11y::Landmark::Navigation => "navigation",
        xa11y::Landmark::Region => "region",
        xa11y::Landmark::Search => "search",
        xa11y::Landmark::Unknown => "unknown",
        // `Landmark` is `#[non_exhaustive]`; variant coverage in the parity
        // check fails the build before a new one reaches this arm.
        other => other.to_snake_case(),
    }
}

/// What an ``announcement`` event carries (``Event.announcement``).
#[pyclass(frozen, from_py_object)]
#[derive(Clone)]
pub(crate) struct AnnouncementInfo {
    inner: xa11y::AnnouncementInfo,
}

impl AnnouncementInfo {
    pub(crate) fn new(inner: xa11y::AnnouncementInfo) -> Self {
        Self { inner }
    }
}

/// The Python spelling of an announcement origin.
pub(crate) fn origin_name(o: xa11y::AnnouncementOrigin) -> &'static str {
    match o {
        xa11y::AnnouncementOrigin::Request => "request",
        xa11y::AnnouncementOrigin::LiveRegion => "live_region",
        xa11y::AnnouncementOrigin::Alert => "alert",
    }
}

#[pymethods]
impl AnnouncementInfo {
    /// ``"request"`` (an app asked for text to be spoken), ``"live_region"``
    /// or ``"alert"`` (a region changed: read the target).
    #[getter]
    fn origin(&self) -> &'static str {
        origin_name(self.inner.origin)
    }
    /// The text to announce, for a request where the platform sends it.
    #[getter]
    fn text(&self) -> Option<String> {
        self.inner.text.clone()
    }
    /// ``"polite"`` or ``"assertive"``, normalised across platforms.
    #[getter]
    fn politeness(&self) -> Option<&'static str> {
        self.inner.politeness.map(politeness_name)
    }
    /// The platform's own priority, as it spells it.
    #[getter]
    fn platform_priority(&self) -> Option<String> {
        self.inner.platform_priority.clone()
    }
    /// UIA's ``activityId`` (Windows only).
    #[getter]
    fn activity_id(&self) -> Option<String> {
        self.inner.activity_id.clone()
    }
    fn __repr__(&self) -> String {
        format!(
            "AnnouncementInfo(origin='{}', text={:?}, politeness={:?})",
            self.origin(),
            self.inner.text,
            self.politeness()
        )
    }
}

/// One kind of relation and the elements it points at, as
/// ``Element.relations`` returns it.
#[pyclass(frozen)]
pub(crate) struct Relation {
    kind: &'static str,
    elements: Vec<Py<crate::Element>>,
}

impl Relation {
    pub(crate) fn from_core(py: Python<'_>, relation: &xa11y::Relation) -> PyResult<Self> {
        let elements = relation
            .elements
            .iter()
            .map(|t| crate::make_py_element(py, t.data(), t.provider().clone()))
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Self {
            kind: relation.kind.to_snake_case(),
            elements,
        })
    }
}

#[pymethods]
impl Relation {
    /// The relation kind, in snake_case (``"labelled_by"``, ``"controls"``).
    #[getter]
    fn kind(&self) -> &'static str {
        self.kind
    }
    /// The related elements, in platform order; ``[]`` for a kind the
    /// platform reports with no targets.
    #[getter]
    fn elements(&self, py: Python<'_>) -> Vec<Py<crate::Element>> {
        self.elements.iter().map(|e| e.clone_ref(py)).collect()
    }
    fn __repr__(&self) -> String {
        format!(
            "Relation(kind='{}', elements=<{} elements>)",
            self.kind,
            self.elements.len()
        )
    }
}
