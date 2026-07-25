//! Shared API model for the bindings-parity check.
//!
//! Three sources feed this model:
//!
//! * [`crate::rustdoc_api`] — `xa11y-core`'s true public surface, extracted
//!   from rustdoc JSON. This is the source of truth.
//! * [`crate::binding_api`] — the PyO3 and napi binding surfaces, parsed
//!   from Rust source with `syn`.
//! * [`crate::parity`] — compares them against `bindings/parity_allowlist.toml`.
//!
//! Keeping the model in one place means the comparison logic never has to
//! care which extractor produced a given entry.

use std::collections::BTreeMap;

/// What kind of item a public type is. Used only for reporting — the
/// comparison treats all of them the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Struct,
    Enum,
    Trait,
}

impl TypeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TypeKind::Struct => "struct",
            TypeKind::Enum => "enum",
            TypeKind::Trait => "trait",
        }
    }
}

/// One method or field on a public type.
///
/// Fields are modelled as members too: in Rust `element.name` is a field
/// access on `ElementData`, but every binding exposes it as a getter. For
/// parity accounting the distinction doesn't matter, so both collapse to a
/// named member.
#[derive(Debug, Clone)]
pub struct Member {
    pub name: String,
    /// `true` when this member came from a struct field rather than an `fn`.
    pub is_field: bool,
    /// Whether the member carries a doc comment. Only meaningful for core;
    /// bindings are checked for presence, not docs.
    pub documented: bool,
}

impl Member {
    pub fn method(name: impl Into<String>, documented: bool) -> Self {
        Self {
            name: name.into(),
            is_field: false,
            documented,
        }
    }

    pub fn field(name: impl Into<String>, documented: bool) -> Self {
        Self {
            name: name.into(),
            is_field: true,
            documented,
        }
    }
}

/// A public type plus everything it exposes.
#[derive(Debug, Clone)]
pub struct ApiType {
    pub name: String,
    pub kind: TypeKind,
    pub members: Vec<Member>,
}

impl ApiType {
    /// Member names, deduped and sorted. Fields and methods share a
    /// namespace here — a core field mirrored as a binding getter should
    /// compare equal.
    pub fn member_names(&self) -> std::collections::BTreeSet<String> {
        self.members.iter().map(|m| m.name.clone()).collect()
    }
}

/// The full extracted surface of one side (core, Python, or JS).
pub type ApiSurface = BTreeMap<String, ApiType>;
