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
//!
//! **Variant coverage.** `Error`, `EventKind`, and `StateFlag` cross the
//! boundary as hand-written per-variant mappings rather than a mechanical
//! conversion, and they are `#[non_exhaustive]`. Those two facts together
//! mean the compiler no longer forces a new variant to be handled: a
//! downstream `match` needs a `_` arm, and the arm swallows it. Each
//! `[[types.variant_coverage]]` entry names the files that must mention every
//! variant of a type, restoring the guard the exhaustive matches used to
//! provide.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use quote::ToTokens;
use syn::{Attribute, Item};

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

/// One `[[types.variant_coverage]]` entry: an enum whose variants must each
/// be named in every listed file.
struct VariantCoverage {
    /// The core enum whose variants are checked.
    ty: String,
    /// Files that must mention every variant, and how each spells it.
    files: Vec<CoveredFile>,
}

/// One file in a `[[types.variant_coverage]]` entry.
struct CoveredFile {
    /// Repo-relative path.
    path: String,
    /// How this file spells a variant.
    spelling: Spelling,
    /// Literal prefix the file puts in front of the spelled variant, e.g.
    /// `XA11Y_` for the JS error-code constants.
    prefix: String,
    /// Restrict the scan to one declaration in the file.
    ///
    /// Needles are otherwise matched file-wide, which is fine until a file
    /// carries more than one enum's spellings in a flat namespace.
    /// `patch-native-dts.mjs` does: `MouseButtonName`'s `'left'` would satisfy
    /// `Anchor::Left`, and `AnchorName`'s `'center'` would satisfy
    /// `MouseButton::Center`, so a new anchor could ship with no TypeScript
    /// name and a green check. Naming the declaration scopes the search to it.
    ///
    /// Slices from the line naming the declaration through the first line
    /// ending in `;` — the shape of a TypeScript type alias, which is where
    /// this is needed.
    within: Option<String>,
    /// Variants this file legitimately does not name.
    ///
    /// A payload-carrying variant may have no string spelling at all:
    /// `Anchor::Offset` crosses as a `(dx, dy)` pair, so the TypeScript
    /// `AnchorName` union has nothing to list for it. Entries are checked for
    /// staleness like everything else.
    exclude: BTreeSet<String>,
}

/// How a covered file names a variant.
///
/// A Rust mapping writes the variant as a path (`EventKind::FocusChanged`);
/// a generated `.pyi`, a `.d.ts` union, or a JS lookup table writes it as a
/// string in that language's own convention. Both are places a new variant
/// has to be handled, so both are checkable — they just need different
/// needles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// `EventKind::FocusChanged` — a path in Rust code.
    RustPath,
    /// `focus_changed`
    Snake,
    /// `focusChanged`
    Camel,
    /// `FOCUS_CHANGED`
    ScreamingSnake,
}

impl Spelling {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "rust_path" => Some(Self::RustPath),
            "snake" => Some(Self::Snake),
            "camel" => Some(Self::Camel),
            "screaming_snake" => Some(Self::ScreamingSnake),
            _ => None,
        }
    }
}

/// `FocusChanged` -> `focus_changed`.
fn to_snake(variant: &str) -> String {
    let mut out = String::with_capacity(variant.len() + 4);
    for (i, c) in variant.chars().enumerate() {
        if c.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// `FocusChanged` -> `focusChanged`.
fn to_camel(variant: &str) -> String {
    let mut chars = variant.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

struct Allowlist {
    tiers: BTreeMap<String, Tier>,
    /// Enums whose variants are hand-mapped rather than mechanically
    /// converted, plus the files doing the mapping.
    variant_coverage: Vec<VariantCoverage>,
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

    let mut variant_coverage = Vec::new();
    if let Some(entries) = value
        .get("types")
        .and_then(|t| t.get("variant_coverage"))
        .and_then(|v| v.as_array())
    {
        for entry in entries {
            let Some(ty) = entry.get("type").and_then(|v| v.as_str()) else {
                return Err(format!(
                    "{}: a [[types.variant_coverage]] entry is missing `type`",
                    path.display()
                ));
            };
            let Some(files) = entry.get("files").and_then(|v| v.as_array()) else {
                return Err(format!(
                    "{}: [[types.variant_coverage]] `{ty}` is missing `files`",
                    path.display()
                ));
            };
            if entry.get("reason").and_then(|v| v.as_str()).is_none() {
                return Err(format!(
                    "{}: [[types.variant_coverage]] `{ty}` is missing `reason`",
                    path.display()
                ));
            }
            let mut covered = Vec::new();
            for f in files {
                // A bare string is the common case: a Rust mapping that
                // writes `Type::Variant`.
                if let Some(fp) = f.as_str() {
                    covered.push(CoveredFile {
                        path: fp.to_string(),
                        spelling: Spelling::RustPath,
                        prefix: String::new(),
                        exclude: BTreeSet::new(),
                        within: None,
                    });
                    continue;
                }
                let Some(fp) = f.get("path").and_then(|v| v.as_str()) else {
                    return Err(format!(
                        "{}: a [[types.variant_coverage]] `{ty}` file entry needs `path`",
                        path.display()
                    ));
                };
                let spelling = match f.get("spelling").and_then(|v| v.as_str()) {
                    None => Spelling::RustPath,
                    Some(name) => match Spelling::parse(name) {
                        Some(sp) => sp,
                        None => {
                            return Err(format!(
                                "{}: [[types.variant_coverage]] `{ty}` file `{fp}` has \
                                 unknown spelling `{name}` (expected rust_path, snake, \
                                 camel, or screaming_snake)",
                                path.display()
                            ))
                        }
                    },
                };
                covered.push(CoveredFile {
                    path: fp.to_string(),
                    spelling,
                    prefix: f
                        .get("prefix")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    exclude: f
                        .get("exclude")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                    within: f.get("within").and_then(|v| v.as_str()).map(String::from),
                });
            }
            variant_coverage.push(VariantCoverage {
                ty: ty.to_string(),
                files: covered,
            });
        }
    }

    Ok(Allowlist {
        tiers,
        variant_coverage,
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

/// Variants of `ty` that a Rust file mentions as a path in real code.
///
/// Reads the file with the Rust lexer and parser rather than scanning text.
/// That is the whole point: comments never reach the token stream, and a
/// string literal collapses into a single `Literal` token, so
/// `"handle Error::Timeout"` contains no `Ident` for the scan to trip on.
/// Nested block comments, raw strings, and the `&'static`
/// lifetime-versus-char-literal ambiguity are the real lexer's problem, not
/// ours — every one of those was a bug in the hand-rolled scanner this
/// replaced.
///
/// `use` items and `#[cfg(test)]` items are dropped before scanning:
/// importing a variant or naming it in a test is not mapping it. Doing that
/// at the AST level is exact, unlike the brace-counting it replaced.
fn rust_mentions(src: &str, path: &str, ty: &str) -> Result<BTreeSet<String>, String> {
    let file = syn::parse_file(src).map_err(|e| {
        format!(
            "[[types.variant_coverage]] reads `{path}` as Rust (the `rust_path` \
             spelling) but it does not parse: {e}\n   \
             Fix: give the entry a `spelling` if it is not a Rust file."
        )
    })?;
    let mut tokens = proc_macro2::TokenStream::new();
    collect_code_tokens(&file.items, &mut tokens);
    let mut found = BTreeSet::new();
    scan_paths(tokens, ty, &mut found);
    Ok(found)
}

/// Attributes of an item, for the `#[cfg(test)]` check.
fn item_attrs(item: &syn::Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

/// Whether any attribute is a `cfg` gate mentioning `test`.
///
/// Token-level, so `#[cfg(all(test))]`, `#[cfg( test )]`, and
/// `#[cfg(any(test, foo))]` are all recognised, while
/// `#[cfg(feature = "testing")]` is not — `"testing"` is a `Literal`, never
/// an `Ident`.
fn is_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && {
            let mut found = false;
            find_ident(a.meta.to_token_stream(), "test", &mut found);
            found
        }
    })
}

fn find_ident(ts: proc_macro2::TokenStream, name: &str, found: &mut bool) {
    for tt in ts {
        match tt {
            proc_macro2::TokenTree::Ident(id) if id == name => *found = true,
            proc_macro2::TokenTree::Group(g) => find_ident(g.stream(), name, found),
            _ => {}
        }
    }
}

/// Flatten items into a token stream, skipping imports and test-only code.
fn collect_code_tokens(items: &[Item], out: &mut proc_macro2::TokenStream) {
    for item in items {
        if matches!(item, Item::Use(_)) || is_cfg_test(item_attrs(item)) {
            continue;
        }
        // Recurse into inline modules so a nested `#[cfg(test)]` is caught
        // rather than swallowed whole with its parent.
        if let Item::Mod(m) = item {
            if let Some((_, inner)) = &m.content {
                collect_code_tokens(inner, out);
                continue;
            }
        }
        out.extend(item.to_token_stream());
    }
}

/// Collect every `V` appearing as `ty::V` in the token stream.
fn scan_paths(ts: proc_macro2::TokenStream, ty: &str, found: &mut BTreeSet<String>) {
    let tt: Vec<proc_macro2::TokenTree> = ts.into_iter().collect();
    for (i, t) in tt.iter().enumerate() {
        if let proc_macro2::TokenTree::Group(g) = t {
            scan_paths(g.stream(), ty, found);
        }
        let proc_macro2::TokenTree::Ident(id) = t else {
            continue;
        };
        if id != ty {
            continue;
        }
        if let (
            Some(proc_macro2::TokenTree::Punct(a)),
            Some(proc_macro2::TokenTree::Punct(b)),
            Some(proc_macro2::TokenTree::Ident(v)),
        ) = (tt.get(i + 1), tt.get(i + 2), tt.get(i + 3))
        {
            if a.as_char() == ':' && b.as_char() == ':' {
                found.insert(v.to_string());
            }
        }
    }
}

/// Strip line comments from a non-Rust file.
///
/// Only used for the string spellings, which live in `.pyi`, `.mjs`, and
/// `.js` files where no parser is available. String literals are kept — for
/// those spellings, matching a literal is the whole job. A `//` or `#` inside
/// a string does not start a comment, which is what stops a URL from eating
/// the rest of its line.
///
/// Line at a time on purpose: whatever this gets wrong stays on one line.
fn strip_line_comments(src: &str, path: &str) -> String {
    let marker = if path.ends_with(".py") || path.ends_with(".pyi") {
        "#"
    } else {
        "//"
    };
    src.lines()
        .map(|line| {
            let mut quote: Option<char> = None;
            let mut cut = line.len();
            let mut it = line.char_indices();
            while let Some((i, c)) = it.next() {
                match quote {
                    Some(q) => {
                        if c == '\\' {
                            it.next();
                        } else if c == q {
                            quote = None;
                        }
                    }
                    None => {
                        if line[i..].starts_with(marker) {
                            cut = i;
                            break;
                        }
                        if c == '"' || c == '\'' {
                            quote = Some(c);
                        }
                    }
                }
            }
            &line[..cut]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Return only the lines of `decl`'s declaration.
///
/// From the line naming `decl` through the first line ending in `;`. That is
/// the shape of a TypeScript type alias, which is the case this exists for —
/// see [`CoveredFile::within`].
fn slice_declaration(src: &str, decl: &str) -> Option<String> {
    let lines: Vec<&str> = src.lines().collect();
    let start = lines.iter().position(|l| mentions_path(l, decl))?;
    let end = lines[start..]
        .iter()
        .position(|l| l.trim_end().ends_with(';'))
        .map(|o| start + o)
        .unwrap_or(lines.len() - 1);
    Some(lines[start..=end].join("\n"))
}

/// Whether `haystack` names `needle` as a whole path segment.
///
/// A plain substring search would let `Error::Platform` be satisfied by a
/// mention of a hypothetical `Error::PlatformTimeout`, so the character after
/// the match must not continue the identifier.
fn mentions_path(haystack: &str, needle: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        // Both ends must be boundaries: `Error::Platform` is not satisfied by
        // `Error::PlatformTimeout`, and `focus_changed` is not satisfied by
        // `my_focus_changed`.
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !ident(c));
        let after_ok = haystack[end..].chars().next().is_none_or(ident_not);
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// Helper for the trailing-boundary test above.
fn ident_not(c: char) -> bool {
    !(c.is_alphanumeric() || c == '_')
}

/// Check that every variant of each `[[types.variant_coverage]]` type is
/// named in each of that entry's files.
///
/// This is the guard that `#[non_exhaustive]` took away. Before it, a new
/// `Error` variant failed the bindings' build because their `match` had no
/// `_` arm; now the arm exists for forward compatibility and this check is
/// what fails instead.
fn check_variant_coverage(
    root: &Path,
    core: &crate::api::ApiSurface,
    allow: &Allowlist,
) -> Vec<String> {
    let mut errs = Vec::new();
    for entry in &allow.variant_coverage {
        let Some(ty) = core.get(&entry.ty) else {
            errs.push(format!(
                "!! Stale [[types.variant_coverage]] entry: `{}` is not a public type \
                 in xa11y-core.\n   \
                 Fix: remove it from bindings/parity_allowlist.toml.",
                entry.ty
            ));
            continue;
        };
        if ty.kind != crate::api::TypeKind::Enum {
            errs.push(format!(
                "!! [[types.variant_coverage]] `{}` is a {}, not an enum — it has no \
                 variants to cover.\n   \
                 Fix: remove it from bindings/parity_allowlist.toml.",
                entry.ty,
                ty.kind.as_str()
            ));
            continue;
        }
        // On an enum, `is_field` members are the variants; inherent methods
        // come through as methods and are not part of this check.
        let variants: Vec<&str> = ty
            .members
            .iter()
            .filter(|m| m.is_field)
            .map(|m| m.name.as_str())
            .collect();

        for file in &entry.files {
            let path = root.join(&file.path);
            let Ok(raw) = std::fs::read_to_string(&path) else {
                errs.push(format!(
                    "!! [[types.variant_coverage]] `{}` names `{}`, which could not \
                     be read.\n   \
                     Fix: correct the path in bindings/parity_allowlist.toml.",
                    entry.ty, file.path
                ));
                continue;
            };
            // Rust files go through the compiler's own lexer and parser;
            // everything else gets a line-comment strip.
            let rust_paths = if file.spelling == Spelling::RustPath {
                match rust_mentions(&raw, &file.path, &entry.ty) {
                    Ok(found) => found,
                    Err(e) => {
                        errs.push(format!("!! {e}"));
                        continue;
                    }
                }
            } else {
                BTreeSet::new()
            };
            let scrubbed = strip_line_comments(&raw, &file.path);
            let src = match &file.within {
                None => scrubbed,
                Some(decl) => match slice_declaration(&scrubbed, decl) {
                    Some(slice) => slice,
                    None => {
                        errs.push(format!(
                            "!! Stale `within` in [[types.variant_coverage]] `{}` for `{}`: \
                             no declaration named `{decl}`.\n   \
                             Fix: correct or remove it in bindings/parity_allowlist.toml.",
                            entry.ty, file.path
                        ));
                        continue;
                    }
                },
            };
            let needle = |v: &str| match file.spelling {
                Spelling::RustPath => format!("{}::{v}", entry.ty),
                Spelling::Snake => format!("{}{}", file.prefix, to_snake(v)),
                Spelling::Camel => format!("{}{}", file.prefix, to_camel(v)),
                Spelling::ScreamingSnake => {
                    format!("{}{}", file.prefix, to_snake(v).to_uppercase())
                }
            };
            // A string spelling must appear quoted, or as a declared name.
            // Without this a bare word anywhere in the file stands in for a
            // mapping — `Toggled::Left` was satisfied by `MouseButtonName`'s
            // `'left'`, which says nothing about checked state.
            let present = |n: &str| {
                if file.spelling == Spelling::RustPath {
                    // `n` is `Ty::Variant`; the AST scan already keyed by type.
                    return n
                        .rsplit("::")
                        .next()
                        .is_some_and(|v| rust_paths.contains(v));
                }
                mentions_path(&src, &format!("\"{n}\""))
                    || mentions_path(&src, &format!("'{n}'"))
                    || mentions_path(&src, &format!("{n}:"))
            };
            // An `exclude` naming a variant the enum no longer has excuses
            // nothing while still reading as a live decision.
            for gone in file
                .exclude
                .iter()
                .filter(|e| !variants.contains(&e.as_str()))
            {
                errs.push(format!(
                    "!! Stale exclude in [[types.variant_coverage]] `{}` for `{}`: \
                     `{gone}` is not a variant.\n   \
                     Fix: remove it from bindings/parity_allowlist.toml.",
                    entry.ty, file.path
                ));
            }
            let missing: Vec<String> = variants
                .iter()
                .copied()
                .filter(|v| !file.exclude.contains(*v))
                .filter(|v| !present(&needle(v)))
                .map(|v| format!("{v} (as `{}`)", needle(v)))
                .collect();
            if !missing.is_empty() {
                errs.push(format!(
                    "!! {} does not handle {} variant(s) of `{}`: {}\n   \
                     `{}` is #[non_exhaustive], so the compiler accepts a `_` arm \
                     instead of failing here.\n   \
                     Fix: handle each explicitly, or drop the variant from core.",
                    file.path,
                    missing.len(),
                    entry.ty,
                    missing.join(", "),
                    entry.ty,
                ));
            }
        }
    }
    errs
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
///
/// A source *method* that shadows a base *method* of the same name is
/// reported too: `ElementData::new` landing on a type that already has
/// `Element::new` is two operations sharing one allowlist entry, which lets
/// one member's design decision silently stand in for the other's.
///
/// Method-vs-method only. A source *field* landing on a base accessor of the
/// same name — `ElementData::pid` and `Element::pid` — is the deref pattern
/// working as designed: they are the same value, and one binding getter
/// genuinely covers both.
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
    // The base type's own methods, for the shadowing check below.
    let base_methods: BTreeSet<&str> = base
        .members
        .iter()
        .filter(|m| !m.is_field)
        .map(|m| m.name.as_str())
        .collect();
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
            if !member.is_field && base_methods.contains(member.name.as_str()) {
                errs.push(format!(
                    "!! [[types.flatten]] into `{0}`: `{from}` shadows the existing method \
                     `{0}::{1}`, so both would be satisfied by one allowlist entry.\n   \
                     Fix: rename the core method, or disambiguate in \
                     bindings/parity_allowlist.toml with \
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

    // ── Layer 3: variant coverage for hand-mapped non_exhaustive enums ──
    let variant_errs = check_variant_coverage(root, &core, &allow);
    if !variant_errs.is_empty() {
        ok = false;
        eprintln!();
        for e in &variant_errs {
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

    // ── Variant coverage ────────────────────────────────────────────────

    fn enum_ty(name: &str, variants: &[&str]) -> ApiType {
        ApiType {
            name: name.to_string(),
            kind: TypeKind::Enum,
            members: variants.iter().map(|v| Member::field(*v, true)).collect(),
        }
    }

    fn coverage_allow(ty: &str, files: &[&str]) -> Allowlist {
        Allowlist {
            tiers: BTreeMap::new(),
            variant_coverage: vec![VariantCoverage {
                ty: ty.to_string(),
                files: files
                    .iter()
                    .map(|f| CoveredFile {
                        path: f.to_string(),
                        spelling: Spelling::RustPath,
                        prefix: String::new(),
                        exclude: BTreeSet::new(),
                        within: None,
                    })
                    .collect(),
            }],
            flatten: BTreeMap::new(),
            python: LangAllow::default(),
            js: LangAllow::default(),
        }
    }

    /// A path mention only counts when it ends at an identifier boundary —
    /// otherwise `Error::Platform` would be satisfied by an unrelated
    /// `Error::PlatformTimeout` arm.
    #[test]
    fn mentions_path_requires_an_identifier_boundary() {
        assert!(mentions_path(
            "match e { Error::Platform { .. } => {} }",
            "Error::Platform"
        ));
        assert!(mentions_path("Error::Platform,", "Error::Platform"));
        assert!(!mentions_path(
            "Error::PlatformTimeout => {}",
            "Error::Platform"
        ));
        assert!(!mentions_path("Error::Platform2 => {}", "Error::Platform"));
        // Leading boundary too: `my_focus_changed` is not `focus_changed`.
        assert!(!mentions_path("my_focus_changed: str", "focus_changed"));
        assert!(mentions_path("  focus_changed: str", "focus_changed"));
        // A later real mention still counts even after a near-miss.
        assert!(mentions_path(
            "Error::PlatformTimeout => {} Error::Platform => {}",
            "Error::Platform"
        ));
    }

    /// The check reads real files, so it needs one on disk. `xtask/src/api.rs`
    /// is a stable Rust file that mentions no `Error` variants.
    fn repo_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a parent directory")
            .to_path_buf()
    }

    #[test]
    fn variant_coverage_reports_a_variant_no_file_mentions() {
        let core = surface(&[enum_ty("Error", &["Timeout", "Platform"])]);
        let allow = coverage_allow("Error", &["xtask/src/api.rs"]);
        let errs = check_variant_coverage(&repo_root(), &core, &allow);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("Timeout"), "{}", errs[0]);
        assert!(errs[0].contains("Platform"), "{}", errs[0]);
    }

    #[test]
    fn variant_coverage_passes_when_every_variant_is_named() {
        // The real Error mapping in the Python binding covers every variant.
        let core = surface(&[enum_ty(
            "Error",
            &["Timeout", "Platform", "NoElementBounds"],
        )]);
        let allow = coverage_allow("Error", &["xa11y-python/src/lib.rs"]);
        assert!(check_variant_coverage(&repo_root(), &core, &allow).is_empty());
    }

    #[test]
    fn variant_coverage_entry_for_a_missing_type_is_stale() {
        let core = surface(&[enum_ty("Error", &["Timeout"])]);
        let allow = coverage_allow("Gone", &["xtask/src/api.rs"]);
        let errs = check_variant_coverage(&repo_root(), &core, &allow);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("Stale"), "{}", errs[0]);
    }

    #[test]
    fn variant_coverage_entry_for_a_struct_is_reported() {
        let core = surface(&[ty("Rect", &["x", "y"])]);
        let allow = coverage_allow("Rect", &["xtask/src/api.rs"]);
        let errs = check_variant_coverage(&repo_root(), &core, &allow);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("not an enum"), "{}", errs[0]);
    }

    #[test]
    fn variant_coverage_reports_an_unreadable_file() {
        let core = surface(&[enum_ty("Error", &["Timeout"])]);
        let allow = coverage_allow("Error", &["no/such/file.rs"]);
        let errs = check_variant_coverage(&repo_root(), &core, &allow);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("no/such/file.rs"), "{}", errs[0]);
    }

    /// The repo's own entries must cover the enums whose bindings mappings
    /// are hand-written — that is the guard `#[non_exhaustive]` replaced.
    #[test]
    fn repo_allowlist_covers_the_hand_mapped_enums() {
        let path = repo_root().join("bindings/parity_allowlist.toml");
        let allow = parse_allowlist(&path).expect("the repo allowlist parses");
        let covered: BTreeSet<&str> = allow
            .variant_coverage
            .iter()
            .map(|v| v.ty.as_str())
            .collect();
        for ty in ["Error", "EventKind", "StateFlag"] {
            assert!(covered.contains(ty), "{ty} needs a variant_coverage entry");
        }
    }

    // ── Flatten shadowing ───────────────────────────────────────────────

    /// A flattened *method* that shadows a base method is two operations
    /// sharing one allowlist entry.
    #[test]
    fn flattened_method_shadowing_a_base_method_is_reported() {
        let base = ty("Element", &["new"]);
        let core = surface(&[base.clone(), ty("ElementData", &["new"])]);
        let (_, errs) = fold_flattened(&base, Some(&flatten(&["ElementData"], &[])), &core);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("shadows"), "{}", errs[0]);
    }

    /// A flattened *field* landing on a base accessor of the same name is
    /// the deref pattern working as designed — `Element::pid` and
    /// `ElementData::pid` are the same value.
    #[test]
    fn flattened_field_under_a_base_accessor_is_fine() {
        let base = ty("Element", &["pid"]);
        let data = ApiType {
            name: "ElementData".to_string(),
            kind: TypeKind::Struct,
            members: vec![Member::field("pid", true)],
        };
        let core = surface(&[base.clone(), data]);
        let (_, errs) = fold_flattened(&base, Some(&flatten(&["ElementData"], &[])), &core);
        assert!(errs.is_empty(), "{errs:?}");
    }

    // ── Rust files: read by the real lexer and parser ───────────────────
    //
    // Every case below was a bug in the hand-rolled text scanner this
    // replaced. They are kept as tests because they are the reason for the
    // approach, not because syn is in doubt.

    fn mentions(src: &str) -> BTreeSet<String> {
        rust_mentions(src, "x.rs", "Error").expect("parses")
    }

    #[test]
    fn a_real_match_arm_counts() {
        assert!(mentions("fn f(e: E) { match e { Error::Timeout => {} } }").contains("Timeout"));
    }

    /// A variant named in an arm *body* counts too — `parse_anchor` maps
    /// string to variant, so the mention is on the right-hand side.
    #[test]
    fn a_variant_in_an_arm_body_counts() {
        assert!(
            mentions(r#"fn f() { match s { "t" => Error::Timeout, _ => x } }"#).contains("Timeout")
        );
    }

    /// The realistic miss: someone meant to come back to it.
    #[test]
    fn comments_never_count() {
        for src in [
            "// TODO: map Error::Timeout later\nfn f() {}",
            "/* Error::Timeout */\nfn f() {}",
            "/* outer /* inner */ Error::Timeout still comment */\nfn f() {}",
            "/// Doc: see Error::Timeout\nfn f() {}",
        ] {
            assert!(!mentions(src).contains("Timeout"), "{src}");
        }
    }

    /// A string collapses to one `Literal` token, so it has no idents at all.
    #[test]
    fn string_literals_never_count() {
        for src in [
            r#"fn f() { let m = "handle Error::Timeout"; }"#,
            r##"fn f() { let m = r#"Error::Timeout"#; }"##,
        ] {
            assert!(!mentions(src).contains("Timeout"), "{src}");
        }
    }

    /// Importing a variant is not mapping it; neither is naming it in a test.
    #[test]
    fn imports_and_test_items_never_count() {
        assert!(!mentions("use xa11y::Error::Timeout;\nfn f() {}").contains("Timeout"));
        for gate in ["#[cfg(test)]", "#[cfg(all(test))]", "#[cfg( test )]"] {
            let src =
                format!("{gate}\nmod t {{ fn f() {{ let _ = Error::Timeout; }} }}\nfn g() {{}}");
            assert!(!mentions(&src).contains("Timeout"), "{gate}");
        }
        // A bodyless `mod tests;` used to swallow whatever followed it.
        let src = "#[cfg(test)]\nmod tests;\nfn f() { match e { Error::Timeout => {} } }";
        assert!(mentions(src).contains("Timeout"));
    }

    /// `#[cfg(feature = "testing")]` is not a test gate — `"testing"` is a
    /// literal, never an ident.
    #[test]
    fn a_feature_gate_named_testing_is_not_a_test_gate() {
        let src = "#[cfg(feature = \"testing\")]\nfn f() { let _ = Error::Timeout; }";
        assert!(mentions(src).contains("Timeout"));
    }

    /// The hazards that broke two hand-rolled scanners are the lexer's
    /// problem now. None of them may hide following code.
    #[test]
    fn lexer_hazards_do_not_hide_following_code() {
        for hazard in [
            r#"const A: &str = "/*";"#,
            r#"const B: &str = "https://xa11y.dev";"#,
            "const C: &'static str = \"x\";",
            "const D: char = '\\'';",
            r##"const E: &str = r#"unterminated "quote"#;"##,
        ] {
            let src = format!("{hazard}\nfn f() {{ match e {{ Error::Timeout => {{}} }} }}");
            assert!(mentions(&src).contains("Timeout"), "hazard: {hazard}");
        }
    }

    /// A file that does not parse is an error, not a silent pass.
    #[test]
    fn unparseable_rust_is_reported() {
        assert!(rust_mentions("fn f( {", "x.rs", "Error").is_err());
    }

    // ── Non-Rust files: line-comment strip only ─────────────────────────

    #[test]
    fn line_comments_are_stripped_per_language() {
        assert!(
            !strip_line_comments("# focus_changed here\nX = 1\n", "s.pyi")
                .contains("focus_changed")
        );
        assert!(
            !strip_line_comments("// 'focusChanged' here\nx;\n", "i.mjs").contains("focusChanged")
        );
    }

    /// String spellings must keep their literals — matching one is the job.
    #[test]
    fn string_literals_survive_for_string_spellings() {
        let pyi = "    FOCUS_CHANGED: str = \"focus_changed\"\n";
        assert!(mentions_path(
            &strip_line_comments(pyi, "x.pyi"),
            "focus_changed"
        ));
    }

    /// A `//` inside a string is not a comment, and damage stays on its line.
    #[test]
    fn strip_line_comments_never_reaches_past_its_line() {
        for hazard in [
            "const S = 'a//b';",
            "const U = \"https://x\";",
            "const T = `x//y`;",
        ] {
            let src = format!("{hazard}\nXA11Y_TIMEOUT: TimeoutError,\n");
            assert!(
                mentions_path(&strip_line_comments(&src, "index.js"), "XA11Y_TIMEOUT"),
                "hazard: {hazard}"
            );
        }
    }

    /// Needles are matched file-wide, which breaks down when one file holds
    /// several enums' spellings — `patch-native-dts.mjs` does. `within`
    /// scopes the search to one declaration.
    #[test]
    fn within_scopes_the_search_to_one_declaration() {
        let mjs = "export type CheckedState = 'on' | 'off' | 'mixed';\n\n\
                   export type MouseButtonName = 'left' | 'right' | 'middle';\n\n\
                   export type AnchorName =\n  | 'center'\n  | 'top_left';\n";
        // Without scoping, MouseButtonName's 'left' answers for an anchor.
        assert!(mentions_path(mjs, "'left'"));
        // Scoped to AnchorName, it does not.
        let anchors = slice_declaration(mjs, "AnchorName").expect("AnchorName is declared");
        assert!(!mentions_path(&anchors, "'left'"));
        assert!(mentions_path(&anchors, "'top_left'"));
        // And the button union still answers for itself.
        let buttons = slice_declaration(mjs, "MouseButtonName").expect("declared");
        assert!(mentions_path(&buttons, "'left'"));
        assert!(!mentions_path(&buttons, "'center'"));
    }

    #[test]
    fn a_within_naming_nothing_is_reported_as_stale() {
        assert!(slice_declaration("export type Foo = 'a';\n", "Nope").is_none());
    }

    /// The repo's `.mjs` entries must all be scoped — that file is the one
    /// place several enums share a namespace.
    #[test]
    fn repo_allowlist_scopes_every_shared_namespace_file() {
        let path = repo_root().join("bindings/parity_allowlist.toml");
        let allow = parse_allowlist(&path).expect("the repo allowlist parses");
        for entry in &allow.variant_coverage {
            for f in &entry.files {
                if f.path.ends_with("patch-native-dts.mjs") {
                    assert!(
                        f.within.is_some(),
                        "{} -> {} needs `within`: that file carries four enums' \
                         spellings in one namespace",
                        entry.ty,
                        f.path
                    );
                }
            }
        }
    }

    #[test]
    fn variant_spellings_convert_as_documented() {
        assert_eq!(to_snake("FocusChanged"), "focus_changed");
        assert_eq!(to_snake("NoElementBounds"), "no_element_bounds");
        assert_eq!(to_camel("FocusChanged"), "focusChanged");
        assert_eq!(to_snake("Timeout").to_uppercase(), "TIMEOUT");
    }

    /// The repo's own entries must cover the string-spelled files, not just
    /// the Rust mappings — those were missed the first time round.
    #[test]
    fn repo_allowlist_covers_the_string_spelled_binding_files() {
        let path = repo_root().join("bindings/parity_allowlist.toml");
        let allow = parse_allowlist(&path).expect("the repo allowlist parses");
        let event = allow
            .variant_coverage
            .iter()
            .find(|v| v.ty == "EventKind")
            .expect("EventKind has a variant_coverage entry");
        let paths: Vec<&str> = event.files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths.iter().any(|p| p.ends_with("_native.pyi")),
            "Python's EventType constants must be covered: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with("patch-native-dts.mjs")),
            "the JS EventTypeName union must be covered: {paths:?}"
        );
        let err = allow
            .variant_coverage
            .iter()
            .find(|v| v.ty == "Error")
            .expect("Error has a variant_coverage entry");
        assert!(
            err.files.iter().any(|f| f.path.ends_with("index.js")
                && f.spelling == Spelling::ScreamingSnake
                && f.prefix == "XA11Y_"),
            "the JS error-code table must be covered"
        );
    }
}
