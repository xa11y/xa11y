//! Extract the Python (PyO3) and JS (napi) binding surfaces.
//!
//! The Rust-side bindings are parsed with `syn` rather than line matching.
//! The previous scraper keyed on exact strings like `impl Locator {` and a
//! column-zero `}`, which quietly missed anything reformatted, `cfg`-gated,
//! or wrapped across lines. A real AST makes those non-issues.
//!
//! `index.d.ts` is the exception: it declares the hand-written JS wrapper
//! layer (the EventEmitter `Subscription`, `App.find`'s poll loop) which has
//! no Rust source to parse. That extractor is line-based by necessity and is
//! carried over largely unchanged.

use std::collections::BTreeMap;
use std::path::Path;

use syn::{Attribute, ImplItem, Item, Meta};

use crate::api::{ApiSurface, ApiType, Member, TypeKind};

// ── Attribute helpers ───────────────────────────────────────────────────

/// Does `attr` have this path (`#[pymethods]`, `#[napi]`, `#[getter]`, …)?
fn is_attr(attr: &Attribute, name: &str) -> bool {
    attr.path().is_ident(name)
}

fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|a| is_attr(a, name))
}

/// Pull a `key = "value"` string out of an attribute's token stream.
///
/// Used for `#[pyclass(name = "_TestActionProbe")]`, `#[pyo3(name = "...")]`
/// and `#[napi(js_name = "select")]`. Token-stream scanning rather than
/// structured parsing keeps this tolerant of the many shapes these
/// attributes take (`#[napi(getter, js_name = "x")]`, multi-line, …).
fn attr_string_value(attr: &Attribute, key: &str) -> Option<String> {
    let Meta::List(list) = &attr.meta else {
        return None;
    };
    let tokens = list.tokens.to_string();
    let idx = tokens.find(key)?;
    let rest = &tokens[idx + key.len()..];
    let eq = rest.find('=')?;
    let after = &rest[eq + 1..];
    let open = after.find('"')?;
    let tail = &after[open + 1..];
    let close = tail.find('"')?;
    Some(tail[..close].to_string())
}

/// Find a `key = "value"` across every attribute in the list.
fn find_string_value(attrs: &[Attribute], attr_name: &str, key: &str) -> Option<String> {
    attrs
        .iter()
        .filter(|a| is_attr(a, attr_name))
        .find_map(|a| attr_string_value(a, key))
}

/// Does any `#[pyo3(...)]` attribute mark this field as a getter?
fn has_pyo3_get(attrs: &[Attribute]) -> bool {
    attrs.iter().filter(|a| is_attr(a, "pyo3")).any(|a| {
        let Meta::List(list) = &a.meta else {
            return false;
        };
        list.tokens
            .to_string()
            .split(',')
            .any(|t| t.trim() == "get" || t.trim().starts_with("get "))
    })
}

/// The type name an `impl` block is for, if it's a plain named type.
fn impl_target_name(imp: &syn::ItemImpl) -> Option<String> {
    // Skip trait impls — only inherent impls carry binding methods.
    if imp.trait_.is_some() {
        return None;
    }
    let syn::Type::Path(tp) = &*imp.self_ty else {
        return None;
    };
    Some(tp.path.segments.last()?.ident.to_string())
}

// ── PyO3 ────────────────────────────────────────────────────────────────

/// Extract the Python binding surface from `xa11y-python/src/lib.rs`.
///
/// Collects, per `#[pyclass]` type:
/// * every `fn` in a `#[pymethods]` impl (honouring `#[pyo3(name = "...")]`),
/// * `#[classattr]` constants (e.g. the `EventType` string constants),
/// * fields marked `#[pyo3(get)]`, which are Python attributes in all but name.
pub fn python_surface(path: &Path) -> Result<ApiSurface, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let file =
        syn::parse_file(&src).map_err(|e| format!("failed to parse {}: {e}", path.display()))?;

    // Rust struct name -> Python-visible class name (`#[pyclass(name = "…")]`).
    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    let mut surface: ApiSurface = ApiSurface::new();

    for item in &file.items {
        if let Item::Struct(s) = item {
            if has_attr(&s.attrs, "pyclass") {
                let rust_name = s.ident.to_string();
                let py_name = find_string_value(&s.attrs, "pyclass", "name")
                    .unwrap_or_else(|| rust_name.clone());
                renames.insert(rust_name, py_name.clone());

                let mut members = Vec::new();
                for field in &s.fields {
                    if !has_pyo3_get(&field.attrs) {
                        continue;
                    }
                    if let Some(ident) = &field.ident {
                        members.push(Member::field(ident.to_string(), true));
                    }
                }
                surface
                    .entry(py_name.clone())
                    .or_insert_with(|| ApiType {
                        name: py_name,
                        kind: TypeKind::Struct,
                        members: Vec::new(),
                    })
                    .members
                    .extend(members);
            }
        }
    }

    for item in &file.items {
        let Item::Impl(imp) = item else { continue };
        if !has_attr(&imp.attrs, "pymethods") {
            continue;
        }
        let Some(rust_name) = impl_target_name(imp) else {
            continue;
        };
        let py_name = renames.get(&rust_name).cloned().unwrap_or(rust_name);

        let mut members = Vec::new();
        for it in &imp.items {
            match it {
                ImplItem::Fn(f) => {
                    let name = find_string_value(&f.attrs, "pyo3", "name")
                        .unwrap_or_else(|| f.sig.ident.to_string());
                    members.push(Member::method(name, !f.attrs.is_empty()));
                }
                // `#[classattr] const FOO: &str = "…";` — a class constant,
                // user-visible exactly like an attribute.
                ImplItem::Const(c) if has_attr(&c.attrs, "classattr") => {
                    members.push(Member::field(c.ident.to_string(), true));
                }
                _ => {}
            }
        }

        surface
            .entry(py_name.clone())
            .or_insert_with(|| ApiType {
                name: py_name,
                kind: TypeKind::Struct,
                members: Vec::new(),
            })
            .members
            .extend(members);
    }

    for ty in surface.values_mut() {
        ty.members.sort_by(|a, b| a.name.cmp(&b.name));
        ty.members.dedup_by(|a, b| a.name == b.name);
    }
    Ok(surface)
}

// ── napi ────────────────────────────────────────────────────────────────

/// Extract the JS binding surface from the `#[napi]` impls in `paths`.
///
/// napi converts Rust `snake_case` to JS `camelCase` automatically, so the
/// Rust identifier already matches core's naming and needs no conversion.
/// An explicit `js_name = "…"` override is camelCase, so it is converted
/// back to snake_case for comparison.
///
/// `renames` maps a Rust impl target to the type name the check should use
/// — the JS `Subscription` is backed by a Rust `NativeSubscription`.
pub fn js_surface(paths: &[&Path], renames: &BTreeMap<&str, &str>) -> Result<ApiSurface, String> {
    let mut surface: ApiSurface = ApiSurface::new();

    for path in paths {
        let src = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let file = syn::parse_file(&src)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;

        for item in &file.items {
            let Item::Impl(imp) = item else { continue };
            if !has_attr(&imp.attrs, "napi") {
                continue;
            }
            let Some(target) = impl_target_name(imp) else {
                continue;
            };
            let logical_name = renames
                .get(target.as_str())
                .map(|s| (*s).to_string())
                .unwrap_or(target);

            let mut members = Vec::new();
            for it in &imp.items {
                let ImplItem::Fn(f) = it else { continue };
                // Only `pub fn` is exported to JS; private helpers
                // (`from_core`, …) live in the same impl block.
                if !matches!(f.vis, syn::Visibility::Public(_)) {
                    continue;
                }
                let name = match find_string_value(&f.attrs, "napi", "js_name") {
                    Some(js) => camel_to_snake(&js),
                    // Trailing underscore escapes a Rust keyword clash
                    // (`select_` -> `select`).
                    None => f.sig.ident.to_string().trim_end_matches('_').to_string(),
                };
                members.push(Member::method(name, !f.attrs.is_empty()));
            }

            surface
                .entry(logical_name.clone())
                .or_insert_with(|| ApiType {
                    name: logical_name,
                    kind: TypeKind::Struct,
                    members: Vec::new(),
                })
                .members
                .extend(members);
        }
    }

    for ty in surface.values_mut() {
        ty.members.sort_by(|a, b| a.name.cmp(&b.name));
        ty.members.dedup_by(|a, b| a.name == b.name);
    }
    Ok(surface)
}

/// Convert `waitUntil` -> `wait_until`.
pub fn camel_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

// ── index.d.ts (hand-written JS wrapper layer) ──────────────────────────

/// Extract method declarations for `ty` from `index.d.ts`.
///
/// The JS package layers a hand-written wrapper (`index.js`) over the napi
/// module, and `index.d.ts` is its only machine-readable description. Two
/// shapes are recognised: `export declare class TY { … }` and `interface TY
/// { … }` inside a `declare module` augmentation.
pub fn index_dts_members(src: &str, ty: &str) -> Vec<Member> {
    let mut out = Vec::new();
    let class_header = format!("class {ty}");
    let iface_header = format!("interface {ty}");
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim();
        let is_open = contains_header_token(trimmed, &class_header)
            || contains_header_token(trimmed, &iface_header);
        if !is_open {
            i += 1;
            continue;
        }
        // Advance to the opening brace, which may be on a later line.
        let Some(mut k) = (i..lines.len()).find(|&j| lines[j].contains('{')) else {
            i += 1;
            continue;
        };

        let mut brace_depth = 0i32;
        let mut paren_depth = 0i32;
        for ch in lines[k].chars() {
            match ch {
                '{' => brace_depth += 1,
                '}' => brace_depth -= 1,
                '(' => paren_depth += 1,
                ')' => paren_depth -= 1,
                _ => {}
            }
        }
        k += 1;

        while k < lines.len() && brace_depth > 0 {
            let line = lines[k];
            let (brace_before, paren_before) = (brace_depth, paren_depth);
            for ch in line.chars() {
                match ch {
                    '{' => brace_depth += 1,
                    '}' => brace_depth -= 1,
                    '(' => paren_depth += 1,
                    ')' => paren_depth -= 1,
                    _ => {}
                }
            }
            // A direct member sits at brace depth 1 and outside any wrapped
            // signature — otherwise a multi-line parameter list would read
            // as members named after its parameters.
            if brace_before == 1 && paren_before == 0 {
                if let Some(name) = parse_ts_member(line) {
                    out.push(Member::method(camel_to_snake(&name), true));
                }
            }
            k += 1;
        }
        i = k.max(i + 1);
    }
    out
}

/// Whole-word match, so `Subscription` doesn't match `Subscriber`.
fn contains_header_token(haystack: &str, token: &str) -> bool {
    let (bytes, tb) = (haystack.as_bytes(), token.as_bytes());
    if tb.is_empty() || tb.len() > bytes.len() {
        return false;
    }
    (0..=bytes.len() - tb.len()).any(|i| {
        &bytes[i..i + tb.len()] == tb
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
            && (i + tb.len() >= bytes.len() || !is_ident_byte(bytes[i + tb.len()]))
    })
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Parse `  methodName(args): Ret;` / `  readonly prop: Type;` and friends.
fn parse_ts_member(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = ["static ", "readonly ", "get ", "set ", "async "]
        .iter()
        .find_map(|p| trimmed.strip_prefix(p))
        .unwrap_or(trimmed);

    let first = rest.chars().next()?;
    if !first.is_ascii_alphabetic() && first != '_' {
        return None;
    }
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    let name = &rest[..end];
    let after = rest[end..].trim_start();

    let is_method = after.starts_with('(') || after.starts_with('<') || after.starts_with("?(");
    let is_prop = after.starts_with(':') || after.starts_with("?:");
    if !is_method && !is_prop {
        return None;
    }
    // EventEmitter overloads are inherited, not part of the mirrored API.
    const SKIP: &[&str] = &["on", "once", "off", "emit", "addListener", "removeListener"];
    if SKIP.contains(&name) {
        return None;
    }
    Some(name.to_string())
}
