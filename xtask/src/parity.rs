//! Compare `xa11y-core`'s public API against the Python and JS bindings.
//!
//! The check has two layers.
//!
//! **Type classification.** Every public type in `xa11y-core` must be
//! classified in `bindings/parity_allowlist.toml` as `mirrored`, `opaque`,
//! or `internal`. A new public type that nobody classified fails the check.
//! This is the property the old checker lacked: it hardcoded four type
//! names, so a new public type was invisible rather than flagged.
//!
//! **Member parity.** For `mirrored` types, every public core member must
//! appear in both bindings, and every binding member must correspond to a
//! core member — unless listed, with a reason, in the per-language
//! allowlist.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::api::ApiType;
use crate::binding_api;
use crate::rustdoc_api;

/// How a public core type relates to the binding surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Type and members are mirrored in both bindings.
    Mirrored,
    /// Reachable from the bindings, but its members are not individually
    /// mirrored — `Role` crosses the boundary as a string, `Error` as a set
    /// of exception classes.
    Opaque,
    /// Not part of the binding surface at all (provider traits, selector
    /// engine internals).
    Internal,
}

#[derive(Default)]
struct LangAllow {
    /// `Type::method` entries allowed to be absent from this binding.
    rust_only: BTreeSet<String>,
    /// Binding members allowed to have no core counterpart.
    extra: BTreeSet<String>,
}

struct Allowlist {
    tiers: BTreeMap<String, Tier>,
    /// Target type -> types whose members are folded into it.
    ///
    /// Some core types have no binding class of their own; their members
    /// surface as members of another type. `Element` derefs to
    /// `ElementData`, and both bindings expose `StateSet`'s booleans as
    /// getters straight on `Element`. Declaring that here means those
    /// members are *required* on the binding type, rather than needing one
    /// allowlist entry per flattened member.
    flatten: BTreeMap<String, Vec<String>>,
    python: LangAllow,
    js: LangAllow,
}

/// Read `method`/`type` out of either a bare string or an inline table.
fn entry_name(v: &toml::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    let t = v.as_table()?;
    t.get("method")
        .or_else(|| t.get("type"))
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
}

fn collect(v: &toml::Value, table: &str, key: &str) -> BTreeSet<String> {
    v.get(table)
        .and_then(|t| t.get(key))
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().filter_map(entry_name).collect())
        .unwrap_or_default()
}

fn parse_allowlist(path: &Path) -> Result<Allowlist, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let value: toml::Value =
        toml::from_str(&src).map_err(|e| format!("{}: parse error: {e}", path.display()))?;

    let mut tiers = BTreeMap::new();
    for (key, tier) in [
        ("mirrored", Tier::Mirrored),
        ("opaque", Tier::Opaque),
        ("internal", Tier::Internal),
    ] {
        for name in collect(&value, "types", key) {
            if let Some(prev) = tiers.insert(name.clone(), tier) {
                if prev != tier {
                    return Err(format!(
                        "{}: type `{name}` is classified twice with different tiers",
                        path.display()
                    ));
                }
            }
        }
    }

    let mut flatten: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(entries) = value
        .get("types")
        .and_then(|t| t.get("flatten"))
        .and_then(|f| f.as_array())
    {
        for entry in entries {
            let Some(into) = entry.get("into").and_then(|v| v.as_str()) else {
                return Err(format!(
                    "{}: a [[types.flatten]] entry is missing `into`",
                    path.display()
                ));
            };
            let Some(from) = entry.get("from").and_then(|v| v.as_array()) else {
                return Err(format!(
                    "{}: [[types.flatten]] `{into}` is missing `from`",
                    path.display()
                ));
            };
            flatten.insert(
                into.to_string(),
                from.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect(),
            );
        }
    }

    Ok(Allowlist {
        tiers,
        flatten,
        python: LangAllow {
            rust_only: collect(&value, "python", "rust_only"),
            extra: collect(&value, "python", "python_only"),
        },
        js: LangAllow {
            rust_only: collect(&value, "js", "rust_only"),
            extra: collect(&value, "js", "js_only"),
        },
    })
}

/// Python dunders never need a core counterpart.
fn is_python_idiomatic(name: &str) -> bool {
    name.starts_with("__") && name.ends_with("__") && name.len() > 4
}

fn is_js_idiomatic(name: &str) -> bool {
    name == "constructor"
}

/// Compare one mirrored type against one binding, returning error lines.
fn compare_type(
    core: &ApiType,
    binding: Option<&BTreeSet<String>>,
    allow: &LangAllow,
    lang: &str,
    allowlist_table: &str,
    only_key: &str,
) -> Vec<String> {
    let ty = &core.name;
    let core_members = core.member_names();
    let mut errs = Vec::new();
    let Some(binding_members) = binding else {
        errs.push(format!(
            "!! {lang} binding has no `{ty}` at all, but it is classified `mirrored`.\n   \
             Fix: add the binding, or reclassify {ty} under [types] opaque/internal with a reason."
        ));
        return errs;
    };

    for member in &core.members {
        let name = &member.name;
        if binding_members.contains(name) {
            continue;
        }
        if allow.rust_only.contains(&format!("{ty}::{name}")) {
            continue;
        }
        // Say which shape the core side has: a binding author needs to know
        // whether to write a getter or a method.
        let what = if member.is_field {
            "field (bindings expose these as getters)"
        } else {
            "method"
        };
        errs.push(format!(
            "!! {lang} binding missing: {ty}::{name} — core {what}\n   \
             Fix: add the binding, or list `{ty}::{name}` in \
             bindings/parity_allowlist.toml [{allowlist_table}.rust_only] with a reason."
        ));
    }

    for name in binding_members {
        if core_members.contains(name) {
            continue;
        }
        if allow.extra.contains(name) || allow.extra.contains(&format!("{ty}::{name}")) {
            continue;
        }
        errs.push(format!(
            "!! {lang} member with no core counterpart: {ty}::{name}\n   \
             Fix: add `{ty}::{name}` to bindings/parity_allowlist.toml \
             [{allowlist_table}.{only_key}] with a reason (or mirror it in xa11y-core)."
        ));
    }
    errs
}

/// Run the bindings-parity check. Returns `true` when the bindings are in
/// sync with core.
pub fn check(root: &Path) -> bool {
    eprintln!("=== Bindings parity check (xa11y-core vs Python & JS) ===\n");

    let core = match rustdoc_api::extract(root) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("!! {e}");
            return false;
        }
    };

    let allow_path = root.join("bindings/parity_allowlist.toml");
    let allow = match parse_allowlist(&allow_path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("!! {e}");
            return false;
        }
    };

    let python = match binding_api::python_surface(&root.join("xa11y-python/src/lib.rs")) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("!! {e}");
            return false;
        }
    };

    let js_paths: Vec<std::path::PathBuf> = [
        "xa11y-js/src/app.rs",
        "xa11y-js/src/locator.rs",
        "xa11y-js/src/element.rs",
        "xa11y-js/src/subscription.rs",
        "xa11y-js/src/input.rs",
        "xa11y-js/src/screenshot.rs",
        "xa11y-js/src/types.rs",
    ]
    .iter()
    .map(|p| root.join(p))
    .collect();
    let js_refs: Vec<&Path> = js_paths.iter().map(|p| p.as_path()).collect();
    let js_renames = BTreeMap::from([("NativeSubscription", "Subscription")]);
    let mut js = match binding_api::js_surface(&js_refs, &js_renames) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("!! {e}");
            return false;
        }
    };

    // Fold in the hand-written TypeScript wrapper layer, which has no Rust
    // source to parse but is real user-facing API.
    let dts = std::fs::read_to_string(root.join("xa11y-js/index.d.ts")).unwrap_or_default();
    for ty in allow
        .tiers
        .iter()
        .filter(|(_, t)| **t == Tier::Mirrored)
        .map(|(n, _)| n.clone())
        .collect::<Vec<_>>()
    {
        let extra = binding_api::index_dts_members(&dts, &ty);
        if extra.is_empty() {
            continue;
        }
        js.entry(ty.clone())
            .or_insert_with(|| crate::api::ApiType {
                name: ty,
                kind: crate::api::TypeKind::Struct,
                members: Vec::new(),
            })
            .members
            .extend(extra);
    }
    for t in js.values_mut() {
        t.members.sort_by(|a, b| a.name.cmp(&b.name));
        t.members.dedup_by(|a, b| a.name == b.name);
    }

    let mut ok = true;

    // ── Layer 1: every public core type must be classified ──────────────
    let unclassified: Vec<&String> = core
        .keys()
        .filter(|name| !allow.tiers.contains_key(*name))
        .collect();
    if !unclassified.is_empty() {
        ok = false;
        eprintln!(
            "!! {} public core type(s) are not classified in bindings/parity_allowlist.toml:",
            unclassified.len()
        );
        for name in &unclassified {
            let kind = core.get(*name).map(|t| t.kind.as_str()).unwrap_or("type");
            eprintln!("     {name}  ({kind})");
        }
        eprintln!(
            "   Fix: add each to [types] as `mirrored` (bindings must expose it),\n        \
             `opaque` (crosses the boundary as a primitive), or `internal`\n        \
             (not part of the binding surface). Non-mirrored entries need a reason.\n"
        );
    }

    // A classified type that no longer exists in core is stale bookkeeping.
    let stale: Vec<&String> = allow
        .tiers
        .keys()
        .filter(|name| !core.contains_key(*name))
        .collect();
    if !stale.is_empty() {
        ok = false;
        eprintln!("!! Stale [types] entries — no such public type in xa11y-core:");
        for name in &stale {
            eprintln!("     {name}");
        }
        eprintln!("   Fix: remove them from bindings/parity_allowlist.toml.\n");
    }

    // ── Layer 2: member parity for mirrored types ───────────────────────
    let (mut n_mirrored, mut n_opaque, mut n_internal) = (0, 0, 0);
    for (name, tier) in &allow.tiers {
        match tier {
            Tier::Mirrored => n_mirrored += 1,
            Tier::Opaque => n_opaque += 1,
            Tier::Internal => n_internal += 1,
        }
        if *tier != Tier::Mirrored {
            continue;
        }
        let Some(base) = core.get(name) else {
            continue; // already reported as stale
        };
        // Fold in any types declared to flatten into this one.
        let mut effective = base.clone();
        for src in allow.flatten.get(name).into_iter().flatten() {
            match core.get(src) {
                Some(t) => effective.members.extend(t.members.iter().cloned()),
                None => {
                    ok = false;
                    eprintln!(
                        "!! [[types.flatten]] into `{name}` names `{src}`, \
                         which is not a public type in xa11y-core."
                    );
                }
            }
        }
        effective.members.sort_by(|a, b| a.name.cmp(&b.name));
        effective.members.dedup_by(|a, b| a.name == b.name);
        let core_ty = &effective;
        let core_members = core_ty.member_names();

        let py_members: Option<BTreeSet<String>> = python.get(name).map(|t| {
            t.member_names()
                .into_iter()
                .filter(|n| !is_python_idiomatic(n))
                .collect()
        });
        let js_members: Option<BTreeSet<String>> = js.get(name).map(|t| {
            t.member_names()
                .into_iter()
                .filter(|n| !is_js_idiomatic(n))
                .collect()
        });

        let py_errs = compare_type(
            core_ty,
            py_members.as_ref(),
            &allow.python,
            "Python",
            "python",
            "python_only",
        );
        let js_errs = compare_type(
            core_ty,
            js_members.as_ref(),
            &allow.js,
            "JS",
            "js",
            "js_only",
        );

        eprintln!(
            "{name}: {} core members | Python: {} | JS: {}",
            core_members.len(),
            if py_errs.is_empty() {
                "all mirrored"
            } else {
                "DRIFT"
            },
            if js_errs.is_empty() {
                "all mirrored"
            } else {
                "DRIFT"
            },
        );
        for e in py_errs.iter().chain(js_errs.iter()) {
            ok = false;
            eprintln!("{e}");
        }
    }

    // ── Undocumented public core API ────────────────────────────────────
    // Scoped to methods on mirrored types: those are what the binding stubs
    // and the docs site render. Enum variants and plain data fields
    // (Rect::x, Role::Button) are self-describing and would only add noise.
    let mut undocumented = Vec::new();
    for (name, tier) in &allow.tiers {
        if *tier != Tier::Mirrored {
            continue;
        }
        if let Some(ty) = core.get(name) {
            for m in &ty.members {
                if !m.is_field && !m.documented {
                    undocumented.push(format!("{name}::{}", m.name));
                }
            }
        }
    }
    if !undocumented.is_empty() {
        ok = false;
        eprintln!(
            "\n!! {} public core member(s) have no doc comment:",
            undocumented.len()
        );
        for m in &undocumented {
            eprintln!("     {m}");
        }
        eprintln!(
            "   Fix: document them. Binding stubs and the docs site are generated\n        \
             from these, so an undocumented member ships undocumented everywhere."
        );
    }

    eprintln!(
        "\nCore public types: {} (mirrored={n_mirrored} opaque={n_opaque} internal={n_internal})",
        core.len(),
    );
    if ok {
        eprintln!("OK: bindings are in sync with xa11y-core.");
    } else {
        eprintln!("!! Bindings parity drift. See bindings/parity_allowlist.toml.");
    }
    ok
}
