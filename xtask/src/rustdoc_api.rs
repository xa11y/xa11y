//! Extract `xa11y-core`'s public API from rustdoc JSON.
//!
//! Why rustdoc JSON rather than parsing source: it is the only source that
//! knows what is *actually reachable* from the crate root. The previous
//! line-based scraper had to hardcode a list of four types (`App`,
//! `Locator`, `Element`, `Subscription`), which meant a newly added public
//! type was invisible to the parity check — 44 public types exist, so ~90%
//! of the surface went unchecked. rustdoc resolves modules, re-exports and
//! visibility for us, so new public API shows up automatically.
//!
//! It also gives two things for free:
//!
//! * `#[doc(hidden)]` items are excluded, so provider-plumbing accessors
//!   (`Locator::provider`, `root`, `nth_index`, …) no longer need allowlist
//!   entries — they simply aren't public API.
//! * Doc comments, so the check can require that public core API is
//!   documented.
//!
//! The cost is a nightly toolchain: `--output-format json` is still
//! unstable. The format is versioned, and [`SUPPORTED_FORMAT_VERSIONS`]
//! gates it — a rustdoc bump fails loudly with an actionable message rather
//! than silently mis-parsing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::api::{ApiSurface, ApiType, Member, TypeKind};

/// rustdoc JSON format versions this extractor understands.
///
/// When rustdoc bumps the format, add the new version here after checking
/// that the fields this module reads (`index`, `inner.{module,use,struct,
/// enum,trait,impl,function}`, `visibility`, `docs`) still mean the same
/// thing. Failing closed is deliberate: a silently mis-parsed API surface
/// would report "all mirrored" for an API it never actually read.
pub const SUPPORTED_FORMAT_VERSIONS: &[u64] = &[60];

/// Run `cargo +nightly rustdoc` and return the parsed JSON document.
fn generate(root: &Path) -> Result<Value, String> {
    let status = Command::new("cargo")
        .current_dir(root)
        .args([
            "+nightly",
            "rustdoc",
            "-p",
            "xa11y-core",
            "--lib",
            "--",
            "-Z",
            "unstable-options",
            "--output-format",
            "json",
        ])
        // rustdoc warnings (broken intra-doc links etc.) are the `docs`
        // job's business, not this check's — keep the parity output clean.
        .env("RUSTDOCFLAGS", "-A warnings")
        .status()
        .map_err(|e| {
            format!(
                "failed to run `cargo +nightly rustdoc`: {e}\n\
                 The bindings-parity check needs a nightly toolchain for rustdoc JSON.\n\
                 Install it with: rustup toolchain install nightly --profile minimal"
            )
        })?;
    if !status.success() {
        return Err("`cargo +nightly rustdoc` failed.\n\
             The bindings-parity check needs a nightly toolchain for rustdoc JSON.\n\
             Install it with: rustup toolchain install nightly --profile minimal"
            .to_string());
    }

    let json_path: PathBuf = root.join("target/doc/xa11y_core.json");
    let raw = std::fs::read_to_string(&json_path)
        .map_err(|e| format!("failed to read {}: {e}", json_path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("failed to parse {}: {e}", json_path.display()))
}

/// Is this item `pub` (as opposed to private / `pub(crate)` / `pub(super)`)?
///
/// Members of an inherent `impl` on a public type report `"default"`
/// visibility in rustdoc JSON — rustdoc only emits them at all when they
/// are publicly reachable, so `default` inside a public impl means public.
fn is_public(item: &Value) -> bool {
    match item.get("visibility") {
        Some(Value::String(s)) => s == "public" || s == "default",
        _ => false,
    }
}

fn docs_present(item: &Value) -> bool {
    item.get("docs")
        .and_then(|d| d.as_str())
        .is_some_and(|s| !s.trim().is_empty())
}

/// The single key of an item's `inner` object (`"struct"`, `"function"`, …).
fn inner_kind(item: &Value) -> Option<(&str, &Value)> {
    item.get("inner")?
        .as_object()?
        .iter()
        .next()
        .map(|(k, v)| (k.as_str(), v))
}

/// Walk the module tree from the crate root, following public modules and
/// `use` re-exports, and collect every publicly reachable struct / enum /
/// trait defined in this crate.
fn reachable_types(index: &serde_json::Map<String, Value>, root_id: &str) -> BTreeMap<String, u64> {
    let mut found = BTreeMap::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![root_id.to_string()];

    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        // Items re-exported from another crate have ids that aren't in this
        // crate's index; skipping them keeps the surface to our own API.
        let Some(item) = index.get(&id) else { continue };
        let Some((kind, inner)) = inner_kind(item) else {
            continue;
        };

        match kind {
            "module" => {
                if let Some(items) = inner.get("items").and_then(|i| i.as_array()) {
                    stack.extend(
                        items
                            .iter()
                            .filter_map(|v| v.as_u64())
                            .map(|v| v.to_string()),
                    );
                }
            }
            // `pub use` re-export: follow to the target so `pub use
            // app::App;` surfaces `App` even though it's defined elsewhere.
            "use" => {
                if let Some(target) = inner.get("id").and_then(|v| v.as_u64()) {
                    stack.push(target.to_string());
                }
            }
            "struct" | "enum" | "trait" => {
                if !is_public(item) {
                    continue;
                }
                if let (Some(name), Some(nid)) = (
                    item.get("name").and_then(|n| n.as_str()),
                    item.get("id").and_then(|v| v.as_u64()),
                ) {
                    found.insert(name.to_string(), nid);
                }
            }
            _ => {}
        }
    }
    found
}

/// Collect the public members of one type: inherent methods, public struct
/// fields, and enum variants.
fn members_of(index: &serde_json::Map<String, Value>, type_id: u64) -> (TypeKind, Vec<Member>) {
    let mut members = Vec::new();
    let Some(item) = index.get(&type_id.to_string()) else {
        return (TypeKind::Struct, members);
    };
    let Some((kind_str, inner)) = inner_kind(item) else {
        return (TypeKind::Struct, members);
    };

    let kind = match kind_str {
        "enum" => TypeKind::Enum,
        "trait" => TypeKind::Trait,
        _ => TypeKind::Struct,
    };

    // ── Inherent methods (and, for traits, required/provided methods) ──
    let impl_ids: Vec<u64> = match kind {
        TypeKind::Trait => inner
            .get("items")
            .and_then(|i| i.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_u64()).collect())
            .unwrap_or_default(),
        _ => inner
            .get("impls")
            .and_then(|i| i.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_u64()).collect())
            .unwrap_or_default(),
    };

    if kind == TypeKind::Trait {
        // Trait items are the methods directly.
        for fid in impl_ids {
            if let Some(f) = index.get(&fid.to_string()) {
                if matches!(inner_kind(f), Some(("function", _))) {
                    if let Some(name) = f.get("name").and_then(|n| n.as_str()) {
                        members.push(Member::method(name, docs_present(f)));
                    }
                }
            }
        }
    } else {
        for iid in impl_ids {
            let Some(imp) = index.get(&iid.to_string()) else {
                continue;
            };
            let Some((_, idata)) = inner_kind(imp) else {
                continue;
            };
            // `trait: null` marks an inherent impl. Trait impls (Clone,
            // Display, Deref, …) are not part of the mirrored surface —
            // bindings implement the host language's equivalents instead.
            if !idata.get("trait").is_some_and(|t| t.is_null()) {
                continue;
            }
            let Some(items) = idata.get("items").and_then(|i| i.as_array()) else {
                continue;
            };
            for fid in items.iter().filter_map(|v| v.as_u64()) {
                let Some(f) = index.get(&fid.to_string()) else {
                    continue;
                };
                if !matches!(inner_kind(f), Some(("function", _))) || !is_public(f) {
                    continue;
                }
                if let Some(name) = f.get("name").and_then(|n| n.as_str()) {
                    members.push(Member::method(name, docs_present(f)));
                }
            }
        }
    }

    // ── Public struct fields ──
    if kind == TypeKind::Struct {
        let field_ids = inner
            .get("kind")
            .and_then(|k| k.get("plain"))
            .and_then(|p| p.get("fields"))
            .and_then(|f| f.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_u64()).collect::<Vec<_>>())
            .unwrap_or_default();
        for fid in field_ids {
            if let Some(f) = index.get(&fid.to_string()) {
                if !is_public(f) {
                    continue;
                }
                if let Some(name) = f.get("name").and_then(|n| n.as_str()) {
                    members.push(Member::field(name, docs_present(f)));
                }
            }
        }
    }

    // ── Enum variants ──
    if kind == TypeKind::Enum {
        let variant_ids = inner
            .get("variants")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_u64()).collect::<Vec<_>>())
            .unwrap_or_default();
        for vid in variant_ids {
            if let Some(v) = index.get(&vid.to_string()) {
                if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                    members.push(Member::field(name, docs_present(v)));
                }
            }
        }
    }

    members.sort_by(|a, b| a.name.cmp(&b.name));
    members.dedup_by(|a, b| a.name == b.name);
    (kind, members)
}

/// Extract the full public API surface of `xa11y-core`.
pub fn extract(root: &Path) -> Result<ApiSurface, String> {
    let doc = generate(root)?;

    let format_version = doc
        .get("format_version")
        .and_then(|v| v.as_u64())
        .ok_or("rustdoc JSON has no `format_version`")?;
    if !SUPPORTED_FORMAT_VERSIONS.contains(&format_version) {
        return Err(format!(
            "unsupported rustdoc JSON format_version {format_version} \
             (this extractor understands {SUPPORTED_FORMAT_VERSIONS:?}).\n\
             Your nightly toolchain bumped the format. Verify that \
             xtask/src/rustdoc_api.rs still reads the right fields, then add \
             {format_version} to SUPPORTED_FORMAT_VERSIONS."
        ));
    }

    let index = doc
        .get("index")
        .and_then(|i| i.as_object())
        .ok_or("rustdoc JSON has no `index` object")?;
    let root_id = doc
        .get("root")
        .and_then(|r| r.as_u64())
        .ok_or("rustdoc JSON has no `root` id")?
        .to_string();

    let mut surface = ApiSurface::new();
    for (name, id) in reachable_types(index, &root_id) {
        let (kind, members) = members_of(index, id);
        surface.insert(
            name.clone(),
            ApiType {
                name,
                kind,
                members,
            },
        );
    }
    Ok(surface)
}
