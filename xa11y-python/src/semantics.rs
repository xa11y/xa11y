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
