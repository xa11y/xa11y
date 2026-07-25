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

/// One `[[types.flatten]]` entry: the sources folded into a target type, plus
/// any per-source member renames.
struct Flatten {
    /// Core types whose members surface on the target type.
    from: Vec<String>,
    /// `Source::member` -> the name the bindings use. Flattening merges by
    /// name, so two sources contributing the same member (`Keyboard::down`
    /// and `Mouse::down`) would silently collapse into one. A rename
    /// disambiguates them (`key_down` / `mouse_down`).
    rename: BTreeMap<String, String>,
}

struct Allowlist {
    tiers: BTreeMap<String, Tier>,
    /// Target type -> the types whose members are folded into it.
    ///
    /// Some core types have no binding class of their own; their members
    /// surface as members of another type. `Element` derefs to
    /// `ElementData`, and both bindings expose `StateSet`'s booleans as
    /// getters straight on `Element`. Declaring that here means those
    /// members are *required* on the binding type, rather than needing one
    /// allowlist entry per flattened member.
    flatten: BTreeMap<String, Flatten>,
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

    let mut flatten: BTreeMap<String, Flatten> = BTreeMap::new();
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
            let mut rename = BTreeMap::new();
            for r in entry
                .get("rename")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                let (Some(member), Some(to)) = (
                    r.get("member").and_then(|v| v.as_str()),
                    r.get("to").and_then(|v| v.as_str()),
                ) else {
                    return Err(format!(
                        "{}: a [[types.flatten]] `{into}` rename entry needs both \
                         `member = \"Source::name\"` and `to = \"binding_name\"`",
                        path.display()
                    ));
                };
                rename.insert(member.to_string(), to.to_string());
            }
            flatten.insert(
                into.to_string(),
                Flatten {
                    from: from
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect(),
                    rename,
                },
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

/// Fold every type declared to flatten into `base`, applying per-source
/// renames, and return the effective core type the bindings must mirror.
///
/// Flattening merges by name. Two sources contributing the same member —
/// `Keyboard::down` and `Mouse::down` both landing on `InputSim` — would
/// otherwise collapse into a single required `down`, quietly excusing the
/// bindings from exposing one of them. That is reported rather than tolerated:
/// a `rename` entry is the fix.
fn fold_flattened(
    base: &ApiType,
    flatten: Option<&Flatten>,
    core: &crate::api::ApiSurface,
) -> (ApiType, Vec<String>) {
    let mut effective = base.clone();
    let mut errs = Vec::new();
    let Some(f) = flatten else {
        return (effective, errs);
    };

    // Folded member name -> the `Source::member` it came from, so a collision
    // can name both sides.
    let mut origin: BTreeMap<String, String> = BTreeMap::new();
    for src in &f.from {
        let Some(t) = core.get(src) else {
            errs.push(format!(
                "!! [[types.flatten]] into `{}` names `{src}`, \
                 which is not a public type in xa11y-core.",
                base.name
            ));
            continue;
        };
        for member in &t.members {
            let from = format!("{src}::{}", member.name);
            let mut member = member.clone();
            if let Some(to) = f.rename.get(&from) {
                member.name = to.clone();
            }
            if let Some(prev) = origin.insert(member.name.clone(), from.clone()) {
                errs.push(format!(
                    "!! [[types.flatten]] into `{}`: `{prev}` and `{from}` both flatten to \
                     `{}`, so only one can be required.\n   \
                     Fix: disambiguate in bindings/parity_allowlist.toml with \
                     `rename = [{{ member = \"{from}\", to = \"...\" }}]`.",
                    base.name, member.name
                ));
            }
            effective.members.push(member);
        }
    }

    // Renames that match nothing are stale bookkeeping, same as a stale type.
    for from in f.rename.keys() {
        let Some((src, member)) = from.split_once("::") else {
            errs.push(format!(
                "!! [[types.flatten]] into `{}`: rename `member = \"{from}\"` is not \
                 of the form `Source::member`.",
                base.name
            ));
            continue;
        };
        let known = core
            .get(src)
            .is_some_and(|t| t.members.iter().any(|m| m.name == member));
        if !known {
            errs.push(format!(
                "!! [[types.flatten]] into `{}`: rename names `{from}`, which is not a \
                 public member of a flattened source.\n   \
                 Fix: remove the rename from bindings/parity_allowlist.toml.",
                base.name
            ));
        }
    }

    (effective, errs)
}

/// Report allowlist *member* entries that no longer correspond to anything.
///
/// The `[types]` staleness check above catches a classified type that left
/// core. This catches the same rot one level down: the pre-rewrite allowlist
/// carried `App::by_name_with_timeout` and `App::by_pid_with_timeout` entries
/// for methods core's public API never had, and nothing noticed.
///
/// `core_members` holds the post-flatten member names of each mirrored type;
/// `binding_members` the same for the binding under test. Entries naming a
/// type that isn't mirrored are stale too — the comparison never consults
/// them.
fn stale_member_entries(
    allow: &LangAllow,
    core_members: &BTreeMap<String, BTreeSet<String>>,
    binding_members: &BTreeMap<String, BTreeSet<String>>,
    lang: &str,
    table: &str,
    only_key: &str,
) -> Vec<String> {
    let mut errs = Vec::new();

    let check = |errs: &mut Vec<String>,
                 entries: &BTreeSet<String>,
                 members: &BTreeMap<String, BTreeSet<String>>,
                 key: &str,
                 side: &str| {
        for entry in entries {
            let Some((ty, member)) = entry.split_once("::") else {
                // An unqualified entry excuses that member name on every type,
                // so it is stale only when no mirrored type has it at all.
                if !members.values().any(|m| m.contains(entry)) {
                    errs.push(format!(
                        "!! Stale [{table}.{key}] entry: `{entry}` — no mirrored type has \
                         a member by that name in {side}."
                    ));
                }
                continue;
            };
            let Some(have) = members.get(ty) else {
                errs.push(format!(
                    "!! Stale [{table}.{key}] entry: `{entry}` — `{ty}` is not a mirrored \
                     type, so the entry is never consulted.\n   \
                     Fix: remove it, or classify `{ty}` as `mirrored` under [types]."
                ));
                continue;
            };
            if !have.contains(member) {
                errs.push(format!(
                    "!! Stale [{table}.{key}] entry: `{entry}` — {side} has no `{member}` \
                     on `{ty}`.\n   \
                     Fix: remove it from bindings/parity_allowlist.toml (or fix the name)."
                ));
            }
        }
    };

    check(
        &mut errs,
        &allow.rust_only,
        core_members,
        "rust_only",
        "xa11y-core",
    );
    check(
        &mut errs,
        &allow.extra,
        binding_members,
        only_key,
        &format!("the {lang} binding"),
    );
    errs
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
    // Post-flatten member names per mirrored type, kept for the staleness
    // pass below so it agrees exactly with what the comparison required.
    let mut core_by_type: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut py_by_type: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut js_by_type: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
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
        let (mut effective, fold_errs) = fold_flattened(base, allow.flatten.get(name), &core);
        for e in &fold_errs {
            ok = false;
            eprintln!("{e}");
        }
        effective.members.sort_by(|a, b| a.name.cmp(&b.name));
        effective.members.dedup_by(|a, b| a.name == b.name);
        let core_ty = &effective;
        let core_members = core_ty.member_names();
        core_by_type.insert(name.clone(), core_members.clone());

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
        if let Some(m) = &py_members {
            py_by_type.insert(name.clone(), m.clone());
        }
        if let Some(m) = &js_members {
            js_by_type.insert(name.clone(), m.clone());
        }

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

    // ── Stale member entries ────────────────────────────────────────────
    // An allowlist entry naming a member that no longer exists silently
    // excuses nothing, and reads as a live design decision. Same rot as a
    // stale [types] entry, one level down.
    let stale_members: Vec<String> = stale_member_entries(
        &allow.python,
        &core_by_type,
        &py_by_type,
        "Python",
        "python",
        "python_only",
    )
    .into_iter()
    .chain(stale_member_entries(
        &allow.js,
        &core_by_type,
        &js_by_type,
        "JS",
        "js",
        "js_only",
    ))
    .collect();
    if !stale_members.is_empty() {
        ok = false;
        eprintln!();
        for e in &stale_members {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ApiSurface, ApiType, Member, TypeKind};

    fn ty(name: &str, members: &[&str]) -> ApiType {
        ApiType {
            name: name.to_string(),
            kind: TypeKind::Struct,
            members: members.iter().map(|m| Member::method(*m, true)).collect(),
        }
    }

    fn surface(types: &[ApiType]) -> ApiSurface {
        types.iter().map(|t| (t.name.clone(), t.clone())).collect()
    }

    fn names(t: &ApiType) -> Vec<String> {
        t.member_names().into_iter().collect()
    }

    fn flatten(from: &[&str], rename: &[(&str, &str)]) -> Flatten {
        Flatten {
            from: from.iter().map(|s| s.to_string()).collect(),
            rename: rename
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn members_of(pairs: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        pairs
            .iter()
            .map(|(t, ms)| {
                (
                    (*t).to_string(),
                    ms.iter().map(|m| (*m).to_string()).collect(),
                )
            })
            .collect()
    }

    fn allow(rust_only: &[&str], extra: &[&str]) -> LangAllow {
        LangAllow {
            rust_only: rust_only.iter().map(|s| s.to_string()).collect(),
            extra: extra.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ── fold_flattened ──────────────────────────────────────────────────

    #[test]
    fn fold_without_flatten_is_the_base_type() {
        let base = ty("InputSim", &["click"]);
        let (effective, errs) = fold_flattened(&base, None, &surface(&[]));
        assert!(errs.is_empty());
        assert_eq!(names(&effective), ["click"]);
    }

    #[test]
    fn fold_merges_source_members() {
        let core = surface(&[ty("Keyboard", &["press"]), ty("Mouse", &["click"])]);
        let (effective, errs) = fold_flattened(
            &ty("InputSim", &["new"]),
            Some(&flatten(&["Keyboard", "Mouse"], &[])),
            &core,
        );
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(names(&effective), ["click", "new", "press"]);
    }

    /// The wrinkle this mechanism exists for: two sources contributing the
    /// same name would collapse into one required member, quietly excusing
    /// the bindings from exposing the other.
    #[test]
    fn undeclared_collision_is_reported() {
        let core = surface(&[ty("Keyboard", &["down"]), ty("Mouse", &["down"])]);
        let (_, errs) = fold_flattened(
            &ty("InputSim", &[]),
            Some(&flatten(&["Keyboard", "Mouse"], &[])),
            &core,
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("Keyboard::down"), "{}", errs[0]);
        assert!(errs[0].contains("Mouse::down"), "{}", errs[0]);
        assert!(errs[0].contains("rename"), "{}", errs[0]);
    }

    #[test]
    fn rename_disambiguates_a_collision() {
        let core = surface(&[ty("Keyboard", &["down"]), ty("Mouse", &["down"])]);
        let (effective, errs) = fold_flattened(
            &ty("InputSim", &[]),
            Some(&flatten(
                &["Keyboard", "Mouse"],
                &[
                    ("Keyboard::down", "key_down"),
                    ("Mouse::down", "mouse_down"),
                ],
            )),
            &core,
        );
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(names(&effective), ["key_down", "mouse_down"]);
    }

    /// A rename that renames nothing excuses nothing while still reading as
    /// a live design decision — same rot as a stale [types] entry.
    #[test]
    fn stale_rename_is_reported() {
        let core = surface(&[ty("Mouse", &["click"])]);
        let (_, errs) = fold_flattened(
            &ty("InputSim", &[]),
            Some(&flatten(&["Mouse"], &[("Mouse::gone", "mouse_gone")])),
            &core,
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("Mouse::gone"), "{}", errs[0]);
    }

    #[test]
    fn flatten_source_missing_from_core_is_reported() {
        let (_, errs) = fold_flattened(
            &ty("InputSim", &[]),
            Some(&flatten(&["Ghost"], &[])),
            &surface(&[]),
        );
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("Ghost"), "{}", errs[0]);
    }

    // ── stale_member_entries ────────────────────────────────────────────

    fn stale(rust_only: &[&str], extra: &[&str]) -> Vec<String> {
        let core = members_of(&[("App", &["by_name_with", "provider"])]);
        let binding = members_of(&[("App", &["by_name"])]);
        stale_member_entries(
            &allow(rust_only, extra),
            &core,
            &binding,
            "Python",
            "python",
            "python_only",
        )
    }

    #[test]
    fn live_entries_are_not_reported() {
        assert!(stale(&["App::by_name_with"], &["App::by_name"]).is_empty());
    }

    #[test]
    fn rust_only_entry_for_a_missing_core_member_is_reported() {
        let errs = stale(&["App::by_name_with_timeout"], &[]);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("by_name_with_timeout"), "{}", errs[0]);
        assert!(errs[0].contains("xa11y-core"), "{}", errs[0]);
    }

    /// An entry on a type that isn't mirrored is never consulted, so it is
    /// stale even though the member exists.
    #[test]
    fn entry_on_a_non_mirrored_type_is_reported() {
        let errs = stale(&["Rect::x"], &[]);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("not a mirrored type"), "{}", errs[0]);
    }

    #[test]
    fn language_only_entry_for_a_missing_binding_member_is_reported() {
        let errs = stale(&[], &["App::gone"]);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("the Python binding"), "{}", errs[0]);
        assert!(errs[0].contains("python_only"), "{}", errs[0]);
    }

    /// Unqualified entries excuse a name on every type, so they are stale
    /// only when no mirrored type has it.
    #[test]
    fn unqualified_entries_match_any_type() {
        assert!(stale(&[], &["by_name"]).is_empty());
        assert_eq!(stale(&[], &["nowhere"]).len(), 1);
    }

    // ── Allowlist schema ────────────────────────────────────────────────

    /// The real allowlist must parse, and its InputSim renames must survive
    /// — they are what keeps `Keyboard::down` and `Mouse::down` from
    /// collapsing into one required member.
    #[test]
    fn repo_allowlist_parses_with_its_renames() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a parent directory")
            .join("bindings/parity_allowlist.toml");
        let allow = parse_allowlist(&path).expect("the repo allowlist parses");
        assert_eq!(allow.tiers.get("InputSim"), Some(&Tier::Mirrored));
        let input = allow
            .flatten
            .get("InputSim")
            .expect("InputSim has a flatten entry");
        assert_eq!(
            input.rename.get("Mouse::down").map(String::as_str),
            Some("mouse_down")
        );
        assert_eq!(
            input.rename.get("Keyboard::down").map(String::as_str),
            Some("key_down")
        );
    }
}
