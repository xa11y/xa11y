//! JS surface for the capability table (`xa11y_core::capabilities`).
//!
//! Synchronous: the table is static data and nothing here touches an app.
//! Platform and support values cross as the core's snake_case spellings,
//! identical to Python's.

use crate::map_err;

/// One feature's answer on one platform.
#[napi(object)]
#[derive(Clone)]
pub struct Capability {
    /// Feature name, `<area>:<name>` (`event:menu_opened`).
    pub name: String,
    /// What the feature is, in one sentence.
    pub description: String,
    /// `supported`, `partial` or `unsupported`.
    pub support: String,
    /// How the platform provides it, or why it does not; `null` when there is
    /// nothing to add.
    pub note: Option<String>,
    /// The first OS version that supports it, when older ones do not.
    pub since: Option<String>,
}

impl From<&xa11y::Capability> for Capability {
    fn from(c: &xa11y::Capability) -> Self {
        Self {
            name: c.name.to_string(),
            description: c.description.to_string(),
            support: c.support.to_snake_case().to_string(),
            note: c.note.clone(),
            since: c.since.map(str::to_string),
        }
    }
}

/// Options for {@link capabilities}.
#[napi(object)]
#[allow(
    dead_code,
    reason = "Constructed by napi-derive from the JS caller's options; the lib-test clippy build doesn't see it"
)]
pub struct CapabilitiesOptions {
    /// `linux`, `macos` or `windows`; defaults to the running platform.
    pub platform: Option<String>,
    /// Dotted OS version (`14.5`, `10.0.19045`) to narrow the answers to.
    /// Linux takes none.
    pub os_version: Option<String>,
}

/// Every feature's answer on one platform. Get one from {@link capabilities}.
#[napi]
pub struct Capabilities {
    inner: xa11y::Capabilities,
}

#[napi]
impl Capabilities {
    /// Every feature name the table knows, in table order.
    #[napi]
    pub fn names() -> Vec<String> {
        xa11y::Capabilities::names().map(str::to_string).collect()
    }

    /// The platform described: `linux`, `macos` or `windows`.
    #[napi(getter)]
    pub fn platform(&self) -> String {
        self.inner.platform.to_snake_case().to_string()
    }

    /// The OS version the answers were narrowed to, or `null` when none was
    /// given or detected. Always `null` on Linux.
    #[napi(getter)]
    pub fn os_version(&self) -> Option<String> {
        self.inner.os_version.clone()
    }

    /// Every feature, in table order.
    #[napi]
    pub fn entries(&self) -> Vec<Capability> {
        self.inner.entries().iter().map(Capability::from).collect()
    }

    /// The capability named `name`. Throws `InvalidActionDataError` for an
    /// unknown name, listing the closest known ones.
    #[napi]
    pub fn get(&self, name: String) -> napi::Result<Capability> {
        self.inner.get(&name).map(Capability::from).map_err(map_err)
    }

    /// Shorthand for `get(name).support`.
    #[napi]
    pub fn support(&self, name: String) -> napi::Result<String> {
        self.inner
            .support(&name)
            .map(|s| s.to_snake_case().to_string())
            .map_err(map_err)
    }

    /// Shorthand for `get(name).note`.
    #[napi]
    pub fn note(&self, name: String) -> napi::Result<Option<String>> {
        self.inner
            .note(&name)
            .map(|n| n.map(str::to_string))
            .map_err(map_err)
    }

    /// Whether `name` is a feature the table knows.
    #[napi]
    pub fn has(&self, name: String) -> bool {
        self.inner.has(&name)
    }
}

/// What a platform's backend can report: one answer per feature, read from a
/// static table without connecting to any app. With no options, describes the
/// running platform and OS version.
#[napi]
#[allow(
    dead_code,
    reason = "Exported via napi-derive; clippy on the Rust-only build can't see the JS-side caller"
)]
pub fn capabilities(options: Option<CapabilitiesOptions>) -> napi::Result<Capabilities> {
    let (platform, os_version) = match options {
        Some(o) => (o.platform, o.os_version),
        None => (None, None),
    };
    let platform = platform
        .as_deref()
        .map(xa11y::Platform::parse)
        .transpose()
        .map_err(map_err)?;
    let inner = xa11y::capabilities_with(platform, os_version.as_deref()).map_err(map_err)?;
    Ok(Capabilities { inner })
}
