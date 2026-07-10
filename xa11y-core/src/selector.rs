//! CSS-like selector parser and matcher for accessibility tree queries.
//!
//! Grammar:
//! ```text
//! group         := selector ("," selector)*
//! selector      := simple_selector (combinator simple_selector)*
//! combinator    := " "          // descendant (any depth)
//!                | " > "       // direct child
//! simple_selector := role_name? attr_filter* pseudo?
//! role_name     := [a-z_]+     // snake_case role name
//! attr_filter   := "[" attr_name op value "]"
//! attr_name     := "name" | "value" | "description" | "role"
//! op            := "=" | "*=" | "^=" | "$="
//! value         := '"' [^"]* '"' | "'" [^']* "'"
//! pseudo        := ":nth(" integer ")"
//! integer       := [1-9][0-9]*
//! ```
//!
//! A top-level comma separates an *alternation group*: the result is the
//! union of each clause's matches, deduplicated by element identity and
//! returned in document order. See [`SelectorGroup`].

use std::collections::HashSet;

use crate::element::{ElementData, Toggled};
use crate::error::{Error, Result};
use crate::role::Role;

/// A parsed CSS-like selector for matching accessibility tree elements.
#[derive(Debug, Clone)]
pub struct Selector {
    /// Chain of simple selectors with combinators.
    pub segments: Vec<SelectorSegment>,
}

#[derive(Debug, Clone)]
pub struct SelectorSegment {
    pub combinator: Combinator,
    pub simple: SimpleSelector,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Combinator {
    /// Root (first segment, no combinator)
    Root,
    /// Descendant (space) — any depth
    Descendant,
    /// Direct child (>)
    Child,
}

/// How a role is matched in a selector.
#[derive(Debug, Clone)]
pub enum RoleMatch {
    /// Match against a normalized role (e.g., `button`, `text_field`).
    Normalized(Role),
    /// Match against an original platform role name (e.g., `AXButton`, `PUSH_BUTTON`).
    Platform(String),
}

#[derive(Debug, Clone)]
pub struct SimpleSelector {
    pub role: Option<RoleMatch>,
    pub filters: Vec<AttrFilter>,
    pub nth: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct AttrFilter {
    pub attr: AttrName,
    pub op: MatchOp,
    pub value: String,
}

/// Attribute name for selector filters. Any `snake_case` string is valid —
/// normalized names (e.g. `name`, `enabled`, `checked`) dispatch to the
/// corresponding `ElementData` field; anything else is looked up in the
/// element's `raw` platform-data map at match time.
pub type AttrName = String;

/// Comparison operator for an attribute filter.
///
/// # Case-insensitivity limitations
///
/// The case-insensitive operators (`Contains`, `StartsWith`, `EndsWith`)
/// compare via [`str::to_lowercase`]: ASCII plus the *simple* Unicode
/// lowercase mapping — **not** full Unicode case folding. Notable
/// consequences:
///
/// - Turkish/Azerbaijani dotted/dotless I: `"I"` lowercases to `"i"`, never
///   to `"ı"`, so `"ı"` does not match a filter value of `"I"`.
/// - German sharp S: `"ß"` does not match `"SS"` (full case folding would
///   equate them).
/// - Other multi-character and locale-dependent foldings are likewise not
///   applied.
///
/// For names that differ only in such edge cases, use the case-sensitive
/// `Exact` operator with the precise string instead.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchOp {
    /// Exact match (case-sensitive)
    Exact,
    /// Substring match (case-insensitive; see the enum docs for the
    /// lowercase-mapping limitations)
    Contains,
    /// Starts-with match (case-insensitive; see the enum docs for the
    /// lowercase-mapping limitations)
    StartsWith,
    /// Ends-with match (case-insensitive; see the enum docs for the
    /// lowercase-mapping limitations)
    EndsWith,
}

impl Selector {
    /// Parse a selector string into a Selector.
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(Error::InvalidSelector {
                selector: input.to_string(),
                message: "empty selector".to_string(),
            });
        }

        let mut segments = Vec::new();
        let mut pos = 0;
        let chars: Vec<char> = input.chars().collect();
        let len = chars.len();

        // Parse first simple selector
        let (simple, new_pos) = Self::parse_simple(&chars, pos, input)?;
        segments.push(SelectorSegment {
            combinator: Combinator::Root,
            simple,
        });
        pos = new_pos;

        // Parse remaining segments with combinators
        while pos < len {
            // Skip whitespace and detect combinator
            let (combinator, new_pos) = Self::parse_combinator(&chars, pos);
            pos = new_pos;

            if pos >= len {
                break;
            }

            // parse_combinator returns Root when it finds neither a space nor
            // '>'. A Root combinator is only valid for the very first segment;
            // anywhere else it means two selectors are concatenated with no
            // combinator between them (e.g. "button:nth(1):nth(2)"), which
            // would produce a segment that panics in find_elements_in_tree.
            if combinator == Combinator::Root {
                return Err(Error::InvalidSelector {
                    selector: input.to_string(),
                    message: "expected combinator (space or '>') between selectors".to_string(),
                });
            }

            let (simple, new_pos) = Self::parse_simple(&chars, pos, input)?;
            segments.push(SelectorSegment { combinator, simple });
            pos = new_pos;
        }

        Ok(Selector { segments })
    }

    fn parse_combinator(chars: &[char], mut pos: usize) -> (Combinator, usize) {
        let mut has_space = false;
        while pos < chars.len() && chars[pos] == ' ' {
            has_space = true;
            pos += 1;
        }

        if pos < chars.len() && chars[pos] == '>' {
            pos += 1;
            // Skip trailing spaces after >
            while pos < chars.len() && chars[pos] == ' ' {
                pos += 1;
            }
            (Combinator::Child, pos)
        } else if has_space {
            (Combinator::Descendant, pos)
        } else {
            (Combinator::Root, pos)
        }
    }

    fn parse_simple(
        chars: &[char],
        mut pos: usize,
        input: &str,
    ) -> Result<(SimpleSelector, usize)> {
        let mut role = None;
        let mut filters = Vec::new();
        let mut nth = None;

        // Try to parse role name. Normalized roles are snake_case (e.g., `button`).
        // Platform roles may include uppercase (e.g., `AXButton`, `PUSH_BUTTON`).
        let start = pos;
        while pos < chars.len() && (chars[pos].is_ascii_alphanumeric() || chars[pos] == '_') {
            pos += 1;
        }
        if pos > start {
            let role_str: String = chars[start..pos].iter().collect();
            match Role::from_snake_case(&role_str) {
                Some(r) => role = Some(RoleMatch::Normalized(r)),
                None => {
                    // Not a normalized role — treat as a platform role name.
                    role = Some(RoleMatch::Platform(role_str));
                }
            }
        }

        // Parse attribute filters
        while pos < chars.len() && chars[pos] == '[' {
            let (filter, new_pos) = Self::parse_attr_filter(chars, pos, input)?;
            filters.push(filter);
            pos = new_pos;
        }

        // Parse :nth() pseudo
        if pos + 4 < chars.len() && chars[pos] == ':' {
            pos += 1;
            let kw_start = pos;
            while pos < chars.len() && chars[pos].is_ascii_alphabetic() {
                pos += 1;
            }
            let kw: String = chars[kw_start..pos].iter().collect();
            if kw == "nth" && pos < chars.len() && chars[pos] == '(' {
                pos += 1; // skip (
                let num_start = pos;
                while pos < chars.len() && chars[pos].is_ascii_digit() {
                    pos += 1;
                }
                let num_str: String = chars[num_start..pos].iter().collect();
                let n: usize = num_str.parse().map_err(|_| Error::InvalidSelector {
                    selector: input.to_string(),
                    message: format!("invalid number in :nth({})", num_str),
                })?;
                if n == 0 {
                    return Err(Error::InvalidSelector {
                        selector: input.to_string(),
                        message: ":nth() is 1-based, got 0".to_string(),
                    });
                }
                if pos < chars.len() && chars[pos] == ')' {
                    pos += 1;
                    nth = Some(n);
                } else {
                    return Err(Error::InvalidSelector {
                        selector: input.to_string(),
                        message: "expected ')' after :nth number".to_string(),
                    });
                }
            } else {
                return Err(Error::InvalidSelector {
                    selector: input.to_string(),
                    message: format!("unknown pseudo-class ':{}'", kw),
                });
            }
        }

        if role.is_none() && filters.is_empty() && nth.is_none() {
            return Err(Error::InvalidSelector {
                selector: input.to_string(),
                message: "empty simple selector".to_string(),
            });
        }

        Ok((SimpleSelector { role, filters, nth }, pos))
    }

    fn parse_attr_filter(
        chars: &[char],
        mut pos: usize,
        input: &str,
    ) -> Result<(AttrFilter, usize)> {
        // Skip [
        pos += 1;

        // Parse attribute name (allows snake_case: [a-z0-9_]+)
        let attr_start = pos;
        while pos < chars.len() && (chars[pos].is_ascii_alphanumeric() || chars[pos] == '_') {
            pos += 1;
        }
        let attr: String = chars[attr_start..pos].iter().collect();
        if attr.is_empty() {
            return Err(Error::InvalidSelector {
                selector: input.to_string(),
                message: "empty attribute name in filter".to_string(),
            });
        }

        // Parse operator
        let op = if pos + 1 < chars.len() && chars[pos] == '*' && chars[pos + 1] == '=' {
            pos += 2;
            MatchOp::Contains
        } else if pos + 1 < chars.len() && chars[pos] == '^' && chars[pos + 1] == '=' {
            pos += 2;
            MatchOp::StartsWith
        } else if pos + 1 < chars.len() && chars[pos] == '$' && chars[pos + 1] == '=' {
            pos += 2;
            MatchOp::EndsWith
        } else if pos < chars.len() && chars[pos] == '=' {
            pos += 1;
            MatchOp::Exact
        } else {
            return Err(Error::InvalidSelector {
                selector: input.to_string(),
                message: "expected operator (=, *=, ^=, $=)".to_string(),
            });
        };

        // Parse quoted value (single or double quotes)
        let quote = match chars.get(pos) {
            Some(&'"') | Some(&'\'') => chars[pos],
            _ => {
                return Err(Error::InvalidSelector {
                    selector: input.to_string(),
                    message: "expected '\"' or \"'\" to start attribute value".to_string(),
                });
            }
        };
        pos += 1; // skip opening quote
        let val_start = pos;
        while pos < chars.len() && chars[pos] != quote {
            pos += 1;
        }
        if pos >= chars.len() {
            return Err(Error::InvalidSelector {
                selector: input.to_string(),
                message: "unterminated string in attribute value".to_string(),
            });
        }
        let value: String = chars[val_start..pos].iter().collect();
        pos += 1; // skip closing quote

        // Skip ]
        if pos >= chars.len() || chars[pos] != ']' {
            return Err(Error::InvalidSelector {
                selector: input.to_string(),
                message: "expected ']' to close attribute filter".to_string(),
            });
        }
        pos += 1;

        Ok((AttrFilter { attr, op, value }, pos))
    }
}

/// A comma-separated alternation of [`Selector`] clauses.
///
/// Mirrors CSS selector lists: matching produces the union of each clause's
/// matches, deduplicated by element identity and returned in document order.
/// A group with a single clause behaves identically to that lone selector.
///
/// Constructed from a string via [`SelectorGroup::parse`]. Commas inside
/// quoted attribute values are not treated as separators.
#[derive(Debug, Clone)]
pub struct SelectorGroup {
    /// Selector clauses, in source order.
    pub clauses: Vec<Selector>,
}

impl SelectorGroup {
    /// Parse a (possibly comma-separated) selector string into a group.
    ///
    /// Single-clause inputs round-trip exactly as if parsed via
    /// [`Selector::parse`]; multi-clause inputs are split on top-level commas
    /// (quoted attribute values are respected).
    pub fn parse(input: &str) -> Result<Self> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(Error::InvalidSelector {
                selector: input.to_string(),
                message: "empty selector".to_string(),
            });
        }
        let parts = split_top_level_commas(trimmed);
        let mut clauses = Vec::with_capacity(parts.len());
        for part in parts {
            let p = part.trim();
            if p.is_empty() {
                return Err(Error::InvalidSelector {
                    selector: input.to_string(),
                    message: "empty clause in selector group".to_string(),
                });
            }
            clauses.push(Selector::parse(p)?);
        }
        // `split_top_level_commas` always returns at least one part, so the
        // emptiness check above guarantees we have ≥1 clause here.
        Ok(SelectorGroup { clauses })
    }

    /// True if the group has exactly one clause (no top-level comma).
    pub fn is_single(&self) -> bool {
        self.clauses.len() == 1
    }
}

/// Split a selector string on top-level commas.
///
/// Commas inside quoted attribute values (single or double quotes) are
/// treated as content, not separators. Returns one or more parts; whitespace
/// trimming and emptiness checks are the caller's responsibility.
///
/// An unterminated quoted string is tolerated here — it falls through as one
/// trailing part, and the eventual `Selector::parse` call will surface the
/// real "unterminated string" error with the original input.
pub fn split_top_level_commas(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in input.chars() {
        match quote {
            Some(q) if ch == q => {
                quote = None;
                current.push(ch);
            }
            Some(_) => {
                current.push(ch);
            }
            None => {
                if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    current.push(ch);
                } else if ch == ',' {
                    parts.push(std::mem::take(&mut current));
                } else {
                    current.push(ch);
                }
            }
        }
    }
    parts.push(current);
    parts
}

/// Distribute a `combinator + suffix` over each clause of `existing`.
///
/// Used by `Locator::descendant` / `Locator::child` so that chained
/// navigation on a group locator applies to every clause:
///
/// ```text
/// chain_combinator("a, b", " ", "c")          => "a c, b c"
/// chain_combinator("x", " > ", "a, b")        => "x > a, x > b"
/// chain_combinator("x, y", " > ", "a, b")     => "x > a, x > b, y > a, y > b"
/// ```
///
/// Each clause is trimmed before being concatenated; the produced string is
/// always re-parseable as a [`SelectorGroup`] when both inputs were.
pub fn chain_combinator(existing: &str, combinator: &str, suffix: &str) -> String {
    let existing_parts = split_top_level_commas(existing);
    let suffix_parts = split_top_level_commas(suffix);
    let mut out = Vec::with_capacity(existing_parts.len() * suffix_parts.len());
    for ep in &existing_parts {
        let ep = ep.trim();
        for sp in &suffix_parts {
            let sp = sp.trim();
            out.push(format!("{}{}{}", ep, combinator, sp));
        }
    }
    out.join(", ")
}

/// Check if an element matches a simple selector (no combinators).
pub fn matches_simple(element: &ElementData, simple: &SimpleSelector) -> bool {
    // Check role
    if let Some(ref role_match) = simple.role {
        match role_match {
            RoleMatch::Normalized(role) => {
                if element.role != *role {
                    return false;
                }
            }
            RoleMatch::Platform(platform_role) => {
                // Check only raw platform role keys, not every string value in
                // the raw map. Without this allowlist a selector like
                // `[platform:AXButton]` would match any element whose AXTitle /
                // AXDescription / class_name happens to be "AXButton".
                //
                // Allowlisted keys correspond to actual platform role fields:
                //   - ax_role, ax_subrole   (macOS)
                //   - atspi_role            (Linux / AT-SPI2)
                //   - class_name            (Windows / UIA — Windows uses a
                //                            numeric control_type_id which is
                //                            not a string; class_name is the
                //                            closest role-bearing string key)
                const PLATFORM_ROLE_KEYS: &[&str] =
                    &["ax_role", "ax_subrole", "atspi_role", "class_name"];
                let matches = PLATFORM_ROLE_KEYS.iter().any(|k| {
                    element
                        .raw
                        .get(*k)
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| s == platform_role)
                });
                if !matches {
                    return false;
                }
            }
        }
    }

    // Check attribute filters. Normalized attribute names dispatch to
    // `ElementData` struct fields; any other name falls back to the
    // platform-specific `raw` map.
    for filter in &simple.filters {
        let attr_value = resolve_attr(element, &filter.attr);

        if !match_op(&filter.op, &filter.value, attr_value.as_deref()) {
            return false;
        }
    }

    true
}

/// Resolve an attribute name for selector matching.
///
/// For normalized attribute names (`role`, `name`, `enabled`, `checked`, …)
/// this reads directly from the corresponding `ElementData` field and formats
/// it as a string. For any other name it falls back to the platform-specific
/// `raw` map, using the same `Value → Option<String>` conversion the old
/// filter loop performed.
///
/// The format produced here is the match contract consumers depend on — it
/// must stay byte-for-byte identical to what a now-removed
/// `populate_attributes`-driven map would have yielded.
fn resolve_attr(element: &ElementData, name: &str) -> Option<String> {
    match name {
        "role" => Some(element.role.to_snake_case().to_string()),
        "name" => element.name.clone(),
        "value" => element.value.clone(),
        "description" => element.description.clone(),
        "bounds" => element.bounds.as_ref().map(|b| {
            serde_json::json!({
                "x": b.x, "y": b.y, "width": b.width, "height": b.height,
            })
            .to_string()
        }),
        "numeric_value" => number_to_string(element.numeric_value),
        "min_value" => number_to_string(element.min_value),
        "max_value" => number_to_string(element.max_value),
        "stable_id" => element.stable_id.clone(),
        "enabled" => Some(element.states.enabled.to_string()),
        "visible" => Some(element.states.visible.to_string()),
        "focused" => Some(element.states.focused.to_string()),
        "active" => Some(element.states.active.to_string()),
        "focusable" => Some(element.states.focusable.to_string()),
        "selected" => Some(element.states.selected.to_string()),
        "editable" => Some(element.states.editable.to_string()),
        "modal" => Some(element.states.modal.to_string()),
        "required" => Some(element.states.required.to_string()),
        "busy" => Some(element.states.busy.to_string()),
        "expanded" => element.states.expanded.map(|b| b.to_string()),
        "checked" => element.states.checked.map(|c| {
            match c {
                Toggled::On => "on",
                Toggled::Off => "off",
                Toggled::Mixed => "mixed",
            }
            .to_string()
        }),
        other => element.raw.get(other).and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Null => None,
            // Arrays/objects: convert to JSON string for matching
            other => Some(other.to_string()),
        }),
    }
}

/// Format an `Option<f64>` the same way `serde_json::Number::from_f64`
/// followed by `Number::to_string()` would have — which is the contract the
/// old `populate_attributes` path established.
fn number_to_string(v: Option<f64>) -> Option<String> {
    v.and_then(serde_json::Number::from_f64)
        .map(|n| n.to_string())
}

/// Test whether `actual` matches `expected` according to the given `MatchOp`.
///
/// Case-insensitive operators lowercase both sides with
/// [`str::to_lowercase`] — ASCII plus the simple Unicode lowercase mapping
/// only, **not** full case folding (e.g. Turkish `ı`/`I` and German `ß`/`SS`
/// do not match). See [`MatchOp`] for details.
pub fn match_op(op: &MatchOp, expected: &str, actual: Option<&str>) -> bool {
    match op {
        MatchOp::Exact => actual == Some(expected),
        MatchOp::Contains => {
            let fl = expected.to_lowercase();
            actual.is_some_and(|v| v.to_lowercase().contains(&fl))
        }
        MatchOp::StartsWith => {
            let fl = expected.to_lowercase();
            actual.is_some_and(|v| v.to_lowercase().starts_with(&fl))
        }
        MatchOp::EndsWith => {
            let fl = expected.to_lowercase();
            actual.is_some_and(|v| v.to_lowercase().ends_with(&fl))
        }
    }
}

// ── find_elements_in_tree ───────────────────────────────────────────────────

/// Default implementation of `find_elements` using `get_children` traversal.
///
/// This walks the tree via the provider's `get_children` method, applies
/// selector matching at each node, and collects results. Providers may
/// override `find_elements` with an optimized implementation that prunes
/// subtrees during traversal.
/// Default implementation of `find_elements` using `get_children` traversal.
///
/// `get_children_fn` is a closure that fetches direct children of an element
/// (or top-level apps if `None`). This avoids the need to pass `&dyn Provider`
/// directly, sidestepping `Sized` constraints in trait default methods.
pub fn find_elements_in_tree(
    get_children_fn: impl Fn(Option<&ElementData>) -> Result<Vec<ElementData>>,
    root: Option<&ElementData>,
    selector: &Selector,
    limit: Option<usize>,
    max_depth: Option<u32>,
) -> Result<Vec<ElementData>> {
    if selector.segments.is_empty() {
        return Ok(vec![]);
    }

    let max_depth = max_depth.unwrap_or(crate::MAX_TREE_DEPTH);

    // Phase 1: Find all matches for the first segment (DFS from root)
    let first = &selector.segments[0].simple;
    let mut candidates = Vec::new();
    // Pass limit to enable early termination when possible
    let phase1_limit = if selector.segments.len() == 1 {
        limit
    } else {
        None
    };
    // Account for :nth — need enough candidates to satisfy it
    let phase1_limit = match (phase1_limit, first.nth) {
        (Some(l), Some(n)) => Some(l.max(n)),
        (_, Some(n)) => Some(n),
        (l, None) => l,
    };

    // Optimization: when searching from system root for applications,
    // only check direct children (apps are always at depth 0 from root).
    let phase1_depth = if root.is_none()
        && matches!(
            first.role,
            Some(RoleMatch::Normalized(crate::role::Role::Application))
        ) {
        0
    } else {
        max_depth
    };

    collect_matching(
        &get_children_fn,
        root,
        first,
        0,
        phase1_depth,
        &mut candidates,
        phase1_limit,
    )?;

    // Apply :nth for the first segment before descending, so later segments
    // narrow against the single selected element rather than every candidate
    // that matched the head. Without this, `button:nth(2) > label` treated
    // `:nth(2)` as a limit on the phase-1 pool and returned labels of *all*
    // buttons up to the second, not labels of the second button.
    apply_nth(&mut candidates, first.nth);

    // Phase 2: For each subsequent segment, narrow candidates
    for segment in &selector.segments[1..] {
        let mut next_candidates = Vec::new();
        for candidate in &candidates {
            match segment.combinator {
                Combinator::Child => {
                    let children = get_children_fn(Some(candidate))?;
                    for child in children {
                        if matches_simple(&child, &segment.simple) {
                            next_candidates.push(child);
                        }
                    }
                }
                Combinator::Descendant => {
                    collect_matching(
                        &get_children_fn,
                        Some(candidate),
                        &segment.simple,
                        0,
                        max_depth,
                        &mut next_candidates,
                        None,
                    )?;
                }
                Combinator::Root => unreachable!(),
            }
        }
        // Deduplicate by handle, preserving order
        let mut seen = HashSet::new();
        next_candidates.retain(|e| seen.insert(e.handle));
        candidates = next_candidates;
        // Apply :nth for this segment before moving on to the next one.
        // This also subsumes the former "apply :nth on the last segment"
        // trailing block — the final segment's :nth runs as part of its
        // own loop iteration.
        apply_nth(&mut candidates, segment.simple.nth);
    }

    // Apply limit
    if let Some(limit) = limit {
        candidates.truncate(limit);
    }

    Ok(candidates)
}

/// Multi-clause variant of [`find_elements_in_tree`].
///
/// Runs each clause in `group` independently, then returns the **union** of
/// matches in **document order**, deduplicated by tree position. A single-
/// clause group is forwarded straight to `find_elements_in_tree` with no
/// extra work.
///
/// Identity for dedup and ordering is the path-from-root (sequence of child
/// indices). This is the only identifier that's stable across multiple
/// `get_children_fn` walks — `ElementData.handle` is *not*, because every
/// real platform backend (Windows/UIA, macOS/AX, Linux/AT-SPI2) allocates a
/// fresh handle on each `cache_element` call, so the same logical node sees
/// disjoint handle values across the per-clause walks. Identifying by path
/// keeps the union correct without requiring providers to stabilise handles.
pub fn find_elements_in_tree_group<F>(
    get_children_fn: F,
    root: Option<&ElementData>,
    group: &SelectorGroup,
    limit: Option<usize>,
    max_depth: Option<u32>,
) -> Result<Vec<ElementData>>
where
    F: Fn(Option<&ElementData>) -> Result<Vec<ElementData>>,
{
    if group.clauses.len() == 1 {
        return find_elements_in_tree(get_children_fn, root, &group.clauses[0], limit, max_depth);
    }

    // Run each clause via the path-tracking walker. The per-clause results
    // are (path, snapshot) pairs where `path` is the sequence of child
    // indices from `root` to the matched node — stable across walks because
    // it's derived purely from `get_children_fn`'s iteration order, not from
    // any platform-allocated identity.
    let f = &get_children_fn;
    let mut by_path: std::collections::BTreeMap<Vec<u32>, ElementData> =
        std::collections::BTreeMap::new();
    for clause in &group.clauses {
        let clause_results = find_elements_in_tree_with_paths(f, root, clause, max_depth)?;
        for (path, data) in clause_results {
            // First clause that matched at this path wins the snapshot —
            // matches `find_elements_in_tree`'s first-write-wins on the
            // single-clause path, and means later clauses' state-drifted
            // re-reads of the same node don't shadow earlier ones.
            by_path.entry(path).or_insert(data);
        }
    }

    // BTreeMap iteration is sorted by key. Comparing `Vec<u32>` paths
    // lexicographically is the same as document order from DFS traversal,
    // so this is the cheapest way to recover the cross-clause merge order.
    let mut out: Vec<ElementData> = by_path.into_values().collect();
    if let Some(l) = limit {
        out.truncate(l);
    }
    Ok(out)
}

/// Path-tracking variant of [`find_elements_in_tree`].
///
/// Mirrors `find_elements_in_tree`'s phase-1 + phase-2 structure exactly,
/// but every result carries the sequence of child indices from `root` to
/// the matched node. The path is the canonical document-order identifier:
/// stable across walks of the same `get_children_fn` and orderable
/// lexicographically.
///
/// Used by [`find_elements_in_tree_group`] to merge per-clause matches
/// without relying on platform-allocated `handle` stability.
pub fn find_elements_in_tree_with_paths<F>(
    get_children_fn: F,
    root: Option<&ElementData>,
    selector: &Selector,
    max_depth: Option<u32>,
) -> Result<Vec<(Vec<u32>, ElementData)>>
where
    F: Fn(Option<&ElementData>) -> Result<Vec<ElementData>>,
{
    if selector.segments.is_empty() {
        return Ok(vec![]);
    }

    let max_depth = max_depth.unwrap_or(crate::MAX_TREE_DEPTH);
    let first = &selector.segments[0].simple;

    // Phase 1: collect all matches for the first segment (DFS from root).
    let phase1_depth = if root.is_none()
        && matches!(
            first.role,
            Some(RoleMatch::Normalized(crate::role::Role::Application))
        ) {
        0
    } else {
        max_depth
    };

    let mut candidates: Vec<(Vec<u32>, ElementData)> = Vec::new();
    let mut path_scratch = Vec::new();
    collect_matching_with_paths(
        &get_children_fn,
        root,
        first,
        0,
        phase1_depth,
        &mut path_scratch,
        &mut candidates,
    )?;
    apply_nth_paths(&mut candidates, first.nth);

    // Phase 2: narrow through remaining segments. Each phase-2 descendant's
    // path extends its phase-1 ancestor's path — so the resulting paths are
    // still rooted at the original `root`.
    for segment in &selector.segments[1..] {
        let mut next: Vec<(Vec<u32>, ElementData)> = Vec::new();
        for (cand_path, candidate) in &candidates {
            match segment.combinator {
                Combinator::Child => {
                    let children = get_children_fn(Some(candidate))?;
                    for (i, child) in children.into_iter().enumerate() {
                        if matches_simple(&child, &segment.simple) {
                            let mut p = cand_path.clone();
                            p.push(i as u32);
                            next.push((p, child));
                        }
                    }
                }
                Combinator::Descendant => {
                    let mut sub: Vec<(Vec<u32>, ElementData)> = Vec::new();
                    let mut sub_path = Vec::new();
                    collect_matching_with_paths(
                        &get_children_fn,
                        Some(candidate),
                        &segment.simple,
                        0,
                        max_depth,
                        &mut sub_path,
                        &mut sub,
                    )?;
                    for (sp, se) in sub {
                        let mut p = cand_path.clone();
                        p.extend(sp);
                        next.push((p, se));
                    }
                }
                Combinator::Root => unreachable!(),
            }
        }
        // Dedup by path, preserving order — same shape as
        // `find_elements_in_tree`'s handle-based dedup but stable across
        // walks because paths are derived from traversal order.
        let mut seen = HashSet::new();
        next.retain(|(p, _)| seen.insert(p.clone()));
        candidates = next;
        apply_nth_paths(&mut candidates, segment.simple.nth);
    }

    Ok(candidates)
}

/// `apply_nth` for path-tagged candidates. Identical 1-based semantics to
/// the plain version, including the "fewer-than-N → empty" rule.
fn apply_nth_paths(candidates: &mut Vec<(Vec<u32>, ElementData)>, nth: Option<usize>) {
    let Some(n) = nth else { return };
    if n <= candidates.len() {
        let kept = candidates.remove(n - 1);
        candidates.clear();
        candidates.push(kept);
    } else {
        candidates.clear();
    }
}

/// DFS variant of [`collect_matching`] that tags each match with its path
/// (sequence of child indices from the original walk root).
///
/// `path` is a scratch buffer pushed/popped at each level so the caller can
/// share one allocation across the whole walk.
fn collect_matching_with_paths(
    get_children_fn: &impl Fn(Option<&ElementData>) -> Result<Vec<ElementData>>,
    root: Option<&ElementData>,
    simple: &SimpleSelector,
    depth: u32,
    max_depth: u32,
    path: &mut Vec<u32>,
    results: &mut Vec<(Vec<u32>, ElementData)>,
) -> Result<()> {
    if depth > max_depth {
        return Ok(());
    }
    let children = get_children_fn(root)?;
    for (i, child) in children.into_iter().enumerate() {
        path.push(i as u32);
        if matches_simple(&child, simple) {
            results.push((path.clone(), child.clone()));
        }
        collect_matching_with_paths(
            get_children_fn,
            Some(&child),
            simple,
            depth + 1,
            max_depth,
            path,
            results,
        )?;
        path.pop();
    }
    Ok(())
}

/// Apply a `:nth(N)` (1-based) filter to a candidate list, collapsing it to
/// the n-th element (or emptying it if fewer than `n` are present).
fn apply_nth(candidates: &mut Vec<ElementData>, nth: Option<usize>) {
    let Some(n) = nth else { return };
    if n <= candidates.len() {
        let kept = candidates.remove(n - 1);
        candidates.clear();
        candidates.push(kept);
    } else {
        candidates.clear();
    }
}

/// DFS collect all elements matching a simple selector under `root`.
fn collect_matching(
    get_children_fn: &impl Fn(Option<&ElementData>) -> Result<Vec<ElementData>>,
    root: Option<&ElementData>,
    simple: &SimpleSelector,
    depth: u32,
    max_depth: u32,
    results: &mut Vec<ElementData>,
    limit: Option<usize>,
) -> Result<()> {
    if depth > max_depth {
        return Ok(());
    }
    if let Some(limit) = limit {
        if results.len() >= limit {
            return Ok(());
        }
    }

    let children = get_children_fn(root)?;
    for child in children {
        if matches_simple(&child, simple) {
            results.push(child.clone());
            if let Some(limit) = limit {
                if results.len() >= limit {
                    return Ok(());
                }
            }
        }
        collect_matching(
            get_children_fn,
            Some(&child),
            simple,
            depth + 1,
            max_depth,
            results,
            limit,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_role_only() {
        let sel = Selector::parse("button").unwrap();
        assert_eq!(sel.segments.len(), 1);
        assert!(matches!(
            sel.segments[0].simple.role,
            Some(RoleMatch::Normalized(Role::Button))
        ));
    }

    #[test]
    fn parse_attr_exact() {
        let sel = Selector::parse(r#"[name="Submit"]"#).unwrap();
        assert_eq!(sel.segments.len(), 1);
        assert!(sel.segments[0].simple.role.is_none());
        assert_eq!(sel.segments[0].simple.filters.len(), 1);
        assert_eq!(sel.segments[0].simple.filters[0].attr, "name");
        assert_eq!(sel.segments[0].simple.filters[0].op, MatchOp::Exact);
        assert_eq!(sel.segments[0].simple.filters[0].value, "Submit");
    }

    #[test]
    fn parse_role_and_attr() {
        let sel = Selector::parse(r#"button[name="Submit"]"#).unwrap();
        assert!(matches!(
            sel.segments[0].simple.role,
            Some(RoleMatch::Normalized(Role::Button))
        ));
        assert_eq!(sel.segments[0].simple.filters[0].value, "Submit");
    }

    #[test]
    fn parse_contains() {
        let sel = Selector::parse(r#"[name*="addr"]"#).unwrap();
        assert_eq!(sel.segments[0].simple.filters[0].op, MatchOp::Contains);
    }

    #[test]
    fn parse_starts_with() {
        let sel = Selector::parse(r#"[name^="addr"]"#).unwrap();
        assert_eq!(sel.segments[0].simple.filters[0].op, MatchOp::StartsWith);
    }

    #[test]
    fn parse_ends_with() {
        let sel = Selector::parse(r#"[name$="bar"]"#).unwrap();
        assert_eq!(sel.segments[0].simple.filters[0].op, MatchOp::EndsWith);
    }

    #[test]
    fn parse_child_combinator() {
        let sel = Selector::parse("toolbar > text_field").unwrap();
        assert_eq!(sel.segments.len(), 2);
        assert!(matches!(
            sel.segments[0].simple.role,
            Some(RoleMatch::Normalized(Role::Toolbar))
        ));
        assert_eq!(sel.segments[1].combinator, Combinator::Child);
        assert!(matches!(
            sel.segments[1].simple.role,
            Some(RoleMatch::Normalized(Role::TextField))
        ));
    }

    #[test]
    fn parse_descendant_combinator() {
        let sel = Selector::parse("toolbar text_field").unwrap();
        assert_eq!(sel.segments.len(), 2);
        assert!(matches!(
            sel.segments[0].simple.role,
            Some(RoleMatch::Normalized(Role::Toolbar))
        ));
        assert_eq!(sel.segments[1].combinator, Combinator::Descendant);
        assert!(matches!(
            sel.segments[1].simple.role,
            Some(RoleMatch::Normalized(Role::TextField))
        ));
    }

    #[test]
    fn parse_nth() {
        let sel = Selector::parse("button:nth(2)").unwrap();
        assert_eq!(sel.segments[0].simple.nth, Some(2));
    }

    #[test]
    fn parse_complex() {
        let sel = Selector::parse(r#"toolbar > text_field[name*="Address"]"#).unwrap();
        assert_eq!(sel.segments.len(), 2);
        assert!(matches!(
            sel.segments[1].simple.role,
            Some(RoleMatch::Normalized(Role::TextField))
        ));
        assert_eq!(sel.segments[1].simple.filters[0].op, MatchOp::Contains);
        assert_eq!(sel.segments[1].simple.filters[0].value, "Address");
    }

    #[test]
    fn parse_empty_error() {
        assert!(Selector::parse("").is_err());
    }

    #[test]
    fn parse_unknown_role_is_platform_role() {
        // Unknown role names are treated as platform role names (e.g., AXButton)
        let sel = Selector::parse("foobar").unwrap();
        assert!(matches!(
            sel.segments[0].simple.role,
            Some(RoleMatch::Platform(ref s)) if s == "foobar"
        ));

        // Platform role with uppercase (typical macOS/Windows)
        let sel = Selector::parse("AXButton").unwrap();
        assert!(matches!(
            sel.segments[0].simple.role,
            Some(RoleMatch::Platform(ref s)) if s == "AXButton"
        ));
    }

    #[test]
    fn parse_nth_zero_error() {
        assert!(Selector::parse("button:nth(0)").is_err());
    }

    #[test]
    fn parse_attr_single_quote() {
        let sel = Selector::parse("[name='Submit']").unwrap();
        assert_eq!(sel.segments[0].simple.filters[0].value, "Submit");
        assert_eq!(sel.segments[0].simple.filters[0].op, MatchOp::Exact);
    }

    #[test]
    fn parse_role_and_attr_single_quote() {
        let sel = Selector::parse("button[name='Submit']").unwrap();
        assert!(matches!(
            sel.segments[0].simple.role,
            Some(RoleMatch::Normalized(Role::Button))
        ));
        assert_eq!(sel.segments[0].simple.filters[0].value, "Submit");
    }

    #[test]
    fn parse_contains_single_quote() {
        let sel = Selector::parse("[name*='addr']").unwrap();
        assert_eq!(sel.segments[0].simple.filters[0].op, MatchOp::Contains);
        assert_eq!(sel.segments[0].simple.filters[0].value, "addr");
    }

    #[test]
    fn nth_on_non_last_segment_filters_during_traversal() {
        // Regression: `:nth(N)` on a non-last segment used to be treated as a
        // pool limit on phase-1 (so up to N candidates were collected and
        // *all* of their children expanded). The expected behaviour is that
        // `:nth(N)` collapses the candidate set to just the N-th match
        // before descending.
        //
        // Tree:
        //   root (application)
        //     ├── toolbar "A" → button "A1", button "A2"
        //     └── toolbar "B" → button "B1", button "B2"
        //
        // `toolbar:nth(2) > button` must return only [B1, B2], not
        // [A1, A2, B1, B2].
        struct Row {
            handle: u64,
            role: Role,
            name: &'static str,
            parent: Option<u64>,
        }
        let tree: Vec<Row> = vec![
            Row {
                handle: 0,
                role: Role::Application,
                name: "root",
                parent: None,
            },
            Row {
                handle: 1,
                role: Role::Toolbar,
                name: "A",
                parent: Some(0),
            },
            Row {
                handle: 2,
                role: Role::Toolbar,
                name: "B",
                parent: Some(0),
            },
            Row {
                handle: 3,
                role: Role::Button,
                name: "A1",
                parent: Some(1),
            },
            Row {
                handle: 4,
                role: Role::Button,
                name: "A2",
                parent: Some(1),
            },
            Row {
                handle: 5,
                role: Role::Button,
                name: "B1",
                parent: Some(2),
            },
            Row {
                handle: 6,
                role: Role::Button,
                name: "B2",
                parent: Some(2),
            },
        ];
        let make_data = |row: &Row| ElementData {
            role: row.role,
            name: Some(row.name.to_string()),
            value: None,
            description: None,
            bounds: None,
            actions: vec![],
            states: crate::element::StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: Some(1),
            raw: std::collections::HashMap::new(),
            handle: row.handle,
        };
        let tree_ref = &tree;
        let get_children = move |parent: Option<&ElementData>| -> Result<Vec<ElementData>> {
            let parent_handle = parent.map(|e| e.handle);
            Ok(tree_ref
                .iter()
                .filter(|row| row.parent == parent_handle)
                .map(make_data)
                .collect())
        };

        let sel = Selector::parse("toolbar:nth(2) > button").unwrap();
        let results = find_elements_in_tree(get_children, None, &sel, None, None).unwrap();
        let names: Vec<_> = results.iter().map(|e| e.name.clone().unwrap()).collect();
        assert_eq!(
            names,
            vec!["B1".to_string(), "B2".to_string()],
            "toolbar:nth(2) > button must return only the children of the 2nd toolbar"
        );
    }

    #[test]
    fn parse_adjacent_nth_is_error() {
        // A second :nth() with no combinator between them would previously
        // parse as Ok but produce a Root-combinator segment in a non-first
        // position, causing an unreachable!() panic in find_elements_in_tree.
        assert!(Selector::parse("button:nth(1):nth(2)").is_err());
    }

    /// Helper: build an ElementData with a given `raw` map for matcher tests.
    fn element_with_raw(raw: crate::element::RawPlatformData) -> ElementData {
        ElementData {
            role: Role::Unknown,
            name: None,
            value: None,
            description: None,
            bounds: None,
            actions: Vec::new(),
            states: crate::element::StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: None,
            raw,
            handle: 0,
        }
    }

    #[test]
    fn platform_role_matches_allowlisted_keys() {
        // ax_role → match
        let mut raw = std::collections::HashMap::new();
        raw.insert(
            "ax_role".into(),
            serde_json::Value::String("AXButton".into()),
        );
        let el = element_with_raw(raw);
        let sel = Selector::parse("AXButton").unwrap();
        assert!(matches_simple(&el, &sel.segments[0].simple));

        // ax_subrole → match
        let mut raw = std::collections::HashMap::new();
        raw.insert(
            "ax_role".into(),
            serde_json::Value::String("AXButton".into()),
        );
        raw.insert(
            "ax_subrole".into(),
            serde_json::Value::String("AXCloseButton".into()),
        );
        let el = element_with_raw(raw);
        let sel = Selector::parse("AXCloseButton").unwrap();
        assert!(matches_simple(&el, &sel.segments[0].simple));

        // atspi_role → match. The role position in a selector is parsed as
        // an identifier, so use an AT-SPI role name without spaces. "pushbutton"
        // is not a known normalized Role, so it becomes a Platform role match.
        let mut raw = std::collections::HashMap::new();
        raw.insert(
            "atspi_role".into(),
            serde_json::Value::String("pushbutton".into()),
        );
        let el = element_with_raw(raw);
        let sel = Selector::parse("pushbutton").unwrap();
        assert!(matches!(
            sel.segments[0].simple.role,
            Some(RoleMatch::Platform(_))
        ));
        assert!(matches_simple(&el, &sel.segments[0].simple));

        // class_name → match
        let mut raw = std::collections::HashMap::new();
        raw.insert(
            "class_name".into(),
            serde_json::Value::String("CustomControl".into()),
        );
        let el = element_with_raw(raw);
        let sel = Selector::parse("CustomControl").unwrap();
        assert!(matches_simple(&el, &sel.segments[0].simple));
    }

    #[test]
    fn platform_role_does_not_match_non_role_string_fields() {
        // An element whose AXTitle happens to equal "AXButton" must NOT
        // match a platform-role selector `AXButton` — previously the
        // matcher scanned every value in the raw map and treated any match
        // as a role hit, which is a bug.
        let mut raw = std::collections::HashMap::new();
        raw.insert(
            "ax_role".into(),
            serde_json::Value::String("AXStaticText".into()),
        );
        raw.insert(
            "AXTitle".into(),
            serde_json::Value::String("AXButton".into()),
        );
        raw.insert(
            "AXDescription".into(),
            serde_json::Value::String("AXButton".into()),
        );
        raw.insert(
            "AXHelp".into(),
            serde_json::Value::String("AXButton".into()),
        );
        raw.insert(
            "AXValue".into(),
            serde_json::Value::String("AXButton".into()),
        );
        raw.insert(
            "ax_identifier".into(),
            serde_json::Value::String("AXButton".into()),
        );
        let el = element_with_raw(raw);

        let sel = Selector::parse("AXButton").unwrap();
        assert!(matches!(
            sel.segments[0].simple.role,
            Some(RoleMatch::Platform(_))
        ));
        assert!(
            !matches_simple(&el, &sel.segments[0].simple),
            "platform role `AXButton` should not match when only AXTitle / AXDescription / \
             AXHelp / AXValue / ax_identifier carry that string",
        );

        // Same element, but with ax_role flipped to AXButton → should match.
        let mut raw = std::collections::HashMap::new();
        raw.insert(
            "ax_role".into(),
            serde_json::Value::String("AXButton".into()),
        );
        raw.insert(
            "AXTitle".into(),
            serde_json::Value::String("Click me".into()),
        );
        let el = element_with_raw(raw);
        assert!(matches_simple(&el, &sel.segments[0].simple));
    }

    // ── resolve_attr / normalized dispatch ──────────────────────────────────

    /// Build a default-ish ElementData; callers tweak the fields they care
    /// about via a closure. Keeps each resolve_attr test focussed on one
    /// normalized key.
    fn element_default() -> ElementData {
        element_with_raw(std::collections::HashMap::new())
    }

    #[test]
    fn resolve_role_reads_struct_field() {
        let mut el = element_default();
        el.role = Role::Button;
        assert_eq!(resolve_attr(&el, "role").as_deref(), Some("button"));
    }

    #[test]
    fn resolve_name_reads_struct_field() {
        let mut el = element_default();
        assert_eq!(resolve_attr(&el, "name"), None);
        el.name = Some("Submit".into());
        assert_eq!(resolve_attr(&el, "name").as_deref(), Some("Submit"));
    }

    #[test]
    fn resolve_value_reads_struct_field() {
        let mut el = element_default();
        assert_eq!(resolve_attr(&el, "value"), None);
        el.value = Some("hello".into());
        assert_eq!(resolve_attr(&el, "value").as_deref(), Some("hello"));
    }

    #[test]
    fn resolve_description_reads_struct_field() {
        let mut el = element_default();
        assert_eq!(resolve_attr(&el, "description"), None);
        el.description = Some("tooltip".into());
        assert_eq!(resolve_attr(&el, "description").as_deref(), Some("tooltip"),);
    }

    #[test]
    fn resolve_bounds_formats_as_json_object() {
        let mut el = element_default();
        assert_eq!(resolve_attr(&el, "bounds"), None);
        el.bounds = Some(crate::element::Rect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        });
        // Contract: same JSON object `populate_attributes` previously wrote.
        let got = resolve_attr(&el, "bounds").expect("bounds set");
        let parsed: serde_json::Value = serde_json::from_str(&got).unwrap();
        assert_eq!(parsed["x"], 1);
        assert_eq!(parsed["y"], 2);
        assert_eq!(parsed["width"], 3);
        assert_eq!(parsed["height"], 4);
    }

    #[test]
    fn resolve_numeric_value_matches_json_number_format() {
        let mut el = element_default();
        assert_eq!(resolve_attr(&el, "numeric_value"), None);
        el.numeric_value = Some(42.0);
        // Mirror serde_json::Number::from_f64(42.0).to_string().
        let expected = serde_json::Number::from_f64(42.0).unwrap().to_string();
        assert_eq!(
            resolve_attr(&el, "numeric_value").as_deref(),
            Some(expected.as_str()),
        );
    }

    #[test]
    fn resolve_min_value_matches_json_number_format() {
        let mut el = element_default();
        el.min_value = Some(0.5);
        let expected = serde_json::Number::from_f64(0.5).unwrap().to_string();
        assert_eq!(
            resolve_attr(&el, "min_value").as_deref(),
            Some(expected.as_str()),
        );
    }

    #[test]
    fn resolve_max_value_matches_json_number_format() {
        let mut el = element_default();
        el.max_value = Some(100.0);
        let expected = serde_json::Number::from_f64(100.0).unwrap().to_string();
        assert_eq!(
            resolve_attr(&el, "max_value").as_deref(),
            Some(expected.as_str()),
        );
    }

    #[test]
    fn resolve_numeric_value_nan_is_none() {
        let mut el = element_default();
        el.numeric_value = Some(f64::NAN);
        // `serde_json::Number::from_f64` rejects NaN, so the old path produced
        // None for this element. Preserve that.
        assert_eq!(resolve_attr(&el, "numeric_value"), None);
    }

    #[test]
    fn resolve_stable_id_reads_struct_field() {
        let mut el = element_default();
        assert_eq!(resolve_attr(&el, "stable_id"), None);
        el.stable_id = Some("abc".into());
        assert_eq!(resolve_attr(&el, "stable_id").as_deref(), Some("abc"));
    }

    #[test]
    fn resolve_bool_states_always_present() {
        let el = element_default();
        // StateSet::default() → enabled=true, visible=true, others=false.
        assert_eq!(resolve_attr(&el, "enabled").as_deref(), Some("true"));
        assert_eq!(resolve_attr(&el, "visible").as_deref(), Some("true"));
        assert_eq!(resolve_attr(&el, "focused").as_deref(), Some("false"));
        assert_eq!(resolve_attr(&el, "active").as_deref(), Some("false"));
        assert_eq!(resolve_attr(&el, "focusable").as_deref(), Some("false"));
        assert_eq!(resolve_attr(&el, "selected").as_deref(), Some("false"));
        assert_eq!(resolve_attr(&el, "editable").as_deref(), Some("false"));
        assert_eq!(resolve_attr(&el, "modal").as_deref(), Some("false"));
        assert_eq!(resolve_attr(&el, "required").as_deref(), Some("false"));
        assert_eq!(resolve_attr(&el, "busy").as_deref(), Some("false"));
    }

    #[test]
    fn resolve_active_reflects_state() {
        let mut el = element_default();
        // Default StateSet has active=false.
        assert_eq!(resolve_attr(&el, "active").as_deref(), Some("false"));
        el.states.active = true;
        assert_eq!(resolve_attr(&el, "active").as_deref(), Some("true"));
    }

    #[test]
    fn resolve_expanded_tri_state() {
        let mut el = element_default();
        // Default: None → not expandable.
        assert_eq!(resolve_attr(&el, "expanded"), None);
        el.states.expanded = Some(true);
        assert_eq!(resolve_attr(&el, "expanded").as_deref(), Some("true"));
        el.states.expanded = Some(false);
        assert_eq!(resolve_attr(&el, "expanded").as_deref(), Some("false"));
    }

    #[test]
    fn resolve_checked_tri_state() {
        let mut el = element_default();
        assert_eq!(resolve_attr(&el, "checked"), None);
        el.states.checked = Some(Toggled::On);
        assert_eq!(resolve_attr(&el, "checked").as_deref(), Some("on"));
        el.states.checked = Some(Toggled::Off);
        assert_eq!(resolve_attr(&el, "checked").as_deref(), Some("off"));
        el.states.checked = Some(Toggled::Mixed);
        assert_eq!(resolve_attr(&el, "checked").as_deref(), Some("mixed"));
    }

    #[test]
    fn resolve_unknown_key_falls_back_to_raw() {
        let mut raw = std::collections::HashMap::new();
        raw.insert(
            "custom_thing".into(),
            serde_json::Value::String("foo".into()),
        );
        raw.insert("a_bool".into(), serde_json::Value::Bool(true));
        raw.insert(
            "a_num".into(),
            serde_json::Value::Number(serde_json::Number::from(7)),
        );
        raw.insert("a_null".into(), serde_json::Value::Null);
        raw.insert("a_list".into(), serde_json::json!(["x", "y"]));
        let el = element_with_raw(raw);

        assert_eq!(resolve_attr(&el, "custom_thing").as_deref(), Some("foo"),);
        assert_eq!(resolve_attr(&el, "a_bool").as_deref(), Some("true"));
        assert_eq!(resolve_attr(&el, "a_num").as_deref(), Some("7"));
        assert_eq!(resolve_attr(&el, "a_null"), None);
        // Arrays serialize to their JSON representation for matching.
        assert_eq!(resolve_attr(&el, "a_list").as_deref(), Some(r#"["x","y"]"#),);
        // Totally absent key → None.
        assert_eq!(resolve_attr(&el, "never_set"), None);
    }

    #[test]
    fn custom_filter_reads_from_raw_end_to_end() {
        // Regression: a filter on a non-normalized key must still match via
        // `raw`, so existing selectors like `[custom=foo]` keep working.
        let mut raw = std::collections::HashMap::new();
        raw.insert("custom".into(), serde_json::Value::String("foo".into()));
        let el = element_with_raw(raw);
        let sel = Selector::parse(r#"[custom="foo"]"#).unwrap();
        assert!(matches_simple(&el, &sel.segments[0].simple));

        let sel = Selector::parse(r#"[custom="bar"]"#).unwrap();
        assert!(!matches_simple(&el, &sel.segments[0].simple));
    }

    #[test]
    fn enabled_filter_reads_struct_field_end_to_end() {
        // Regression: `[enabled="true"]` reads `states.enabled` directly.
        let mut el = element_default();
        el.states.enabled = true;
        let sel = Selector::parse(r#"[enabled="true"]"#).unwrap();
        assert!(matches_simple(&el, &sel.segments[0].simple));

        el.states.enabled = false;
        assert!(!matches_simple(&el, &sel.segments[0].simple));

        let sel_false = Selector::parse(r#"[enabled="false"]"#).unwrap();
        assert!(matches_simple(&el, &sel_false.segments[0].simple));
    }

    #[test]
    fn checked_filter_reads_struct_field_end_to_end() {
        let mut el = element_default();
        el.states.checked = Some(Toggled::On);
        let sel = Selector::parse(r#"[checked="on"]"#).unwrap();
        assert!(matches_simple(&el, &sel.segments[0].simple));

        el.states.checked = Some(Toggled::Off);
        assert!(!matches_simple(&el, &sel.segments[0].simple));

        el.states.checked = None;
        assert!(!matches_simple(&el, &sel.segments[0].simple));
    }

    // ── SelectorGroup / comma alternation ───────────────────────────────────

    #[test]
    fn split_top_level_commas_basic() {
        assert_eq!(split_top_level_commas("a"), vec!["a".to_string()]);
        assert_eq!(
            split_top_level_commas("a,b"),
            vec!["a".to_string(), "b".to_string()],
        );
        assert_eq!(
            split_top_level_commas("a , b"),
            vec!["a ".to_string(), " b".to_string()],
        );
    }

    #[test]
    fn split_top_level_commas_ignores_quoted_commas() {
        // Commas inside quoted attribute values must NOT split the group.
        assert_eq!(
            split_top_level_commas(r#"[name="a,b"]"#),
            vec![r#"[name="a,b"]"#.to_string()],
        );
        assert_eq!(
            split_top_level_commas(r#"[name='a,b'], button"#),
            vec![r#"[name='a,b']"#.to_string(), " button".to_string()],
        );
    }

    #[test]
    fn parse_group_single_clause_matches_selector_parse() {
        // Single-clause groups must produce one clause whose AST equals what
        // `Selector::parse` returns. This is the "existing single-pattern
        // selectors keep working unchanged" guarantee.
        let group = SelectorGroup::parse("button").unwrap();
        assert_eq!(group.clauses.len(), 1);
        assert!(group.is_single());
        assert!(matches!(
            group.clauses[0].segments[0].simple.role,
            Some(RoleMatch::Normalized(Role::Button))
        ));
    }

    #[test]
    fn parse_group_two_clauses() {
        let group = SelectorGroup::parse(r#"button[name="All Clear"], button[name="Clear"]"#)
            .expect("group parses");
        assert_eq!(group.clauses.len(), 2);
        assert!(!group.is_single());
        assert_eq!(
            group.clauses[0].segments[0].simple.filters[0].value,
            "All Clear"
        );
        assert_eq!(
            group.clauses[1].segments[0].simple.filters[0].value,
            "Clear"
        );
    }

    #[test]
    fn parse_group_whitespace_tolerance() {
        // Both `a,b` and `a , b` (and the asymmetric variants) must parse to
        // the same shape.
        for input in &["a,b", "a ,b", "a, b", "a , b", "a  ,  b"] {
            let group = SelectorGroup::parse(input).expect("parses");
            assert_eq!(group.clauses.len(), 2, "input: {input:?}");
            assert!(matches!(
                group.clauses[0].segments[0].simple.role,
                Some(RoleMatch::Platform(ref s)) if s == "a"
            ));
            assert!(matches!(
                group.clauses[1].segments[0].simple.role,
                Some(RoleMatch::Platform(ref s)) if s == "b"
            ));
        }
    }

    #[test]
    fn parse_group_combinator_per_clause() {
        // `window button, dialog button` should parse as two independent
        // selectors, each with its own combinator chain — NOT as one selector
        // containing a stray comma.
        let group = SelectorGroup::parse("window button, dialog button").unwrap();
        assert_eq!(group.clauses.len(), 2);
        assert_eq!(group.clauses[0].segments.len(), 2);
        assert_eq!(group.clauses[1].segments.len(), 2);
        assert!(matches!(
            group.clauses[0].segments[0].simple.role,
            Some(RoleMatch::Normalized(Role::Window))
        ));
        assert!(matches!(
            group.clauses[1].segments[0].simple.role,
            Some(RoleMatch::Normalized(Role::Dialog))
        ));
        assert_eq!(
            group.clauses[0].segments[1].combinator,
            Combinator::Descendant
        );
        assert_eq!(
            group.clauses[1].segments[1].combinator,
            Combinator::Descendant
        );
    }

    #[test]
    fn parse_group_mixed_attribute_clauses() {
        // Different attribute filters per clause must parse independently —
        // makes sure the `[...]` machinery doesn't carry state across commas.
        let group =
            SelectorGroup::parse(r#"button[name="OK"], text_field[name*="search"]"#).unwrap();
        assert_eq!(group.clauses.len(), 2);
        assert_eq!(
            group.clauses[0].segments[0].simple.filters[0].op,
            MatchOp::Exact
        );
        assert_eq!(
            group.clauses[1].segments[0].simple.filters[0].op,
            MatchOp::Contains
        );
    }

    #[test]
    fn parse_group_quoted_comma_kept_in_value() {
        // `[name="a,b"]` must reach the matcher with its comma intact.
        let group = SelectorGroup::parse(r#"[name="a,b"]"#).unwrap();
        assert_eq!(group.clauses.len(), 1);
        assert_eq!(group.clauses[0].segments[0].simple.filters[0].value, "a,b");
    }

    #[test]
    fn parse_group_large_count() {
        // Eight clauses: covers "large group counts" from the issue without
        // turning the test into a stress benchmark.
        let input = (1..=8)
            .map(|i| format!(r#"button[name="b{i}"]"#))
            .collect::<Vec<_>>()
            .join(", ");
        let group = SelectorGroup::parse(&input).unwrap();
        assert_eq!(group.clauses.len(), 8);
        for (i, clause) in group.clauses.iter().enumerate() {
            assert_eq!(
                clause.segments[0].simple.filters[0].value,
                format!("b{}", i + 1)
            );
        }
    }

    #[test]
    fn parse_group_empty_error() {
        // Trailing, leading, double, and whitespace-only commas all surface
        // as "empty clause" rather than silently producing fewer clauses.
        for input in &[",a", "a,", ",,", "a,,b", "  ,  ", " , "] {
            assert!(
                SelectorGroup::parse(input).is_err(),
                "expected parse error for {input:?}",
            );
        }
    }

    #[test]
    fn parse_group_propagates_clause_errors() {
        // A malformed clause must surface as an Err, not silently drop.
        assert!(SelectorGroup::parse("button, :nth(0)").is_err());
        assert!(SelectorGroup::parse("button, foo[unterminated=").is_err());
    }

    #[test]
    fn chain_combinator_distributes_left_side() {
        assert_eq!(chain_combinator("a, b", " ", "c"), "a c, b c");
        assert_eq!(chain_combinator("a, b", " > ", "c"), "a > c, b > c");
        assert_eq!(chain_combinator("a", " ", "c"), "a c");
    }

    #[test]
    fn chain_combinator_cross_products_groups() {
        // Both sides are groups → cross product, keeping `a, b → x, y` in
        // left-major order.
        assert_eq!(chain_combinator("a, b", " ", "x, y"), "a x, a y, b x, b y",);
    }

    #[test]
    fn chain_combinator_ignores_commas_in_quotes() {
        // Quoted commas on either side must not cause a split.
        assert_eq!(
            chain_combinator(r#"[name="a,b"]"#, " ", "c"),
            r#"[name="a,b"] c"#,
        );
        assert_eq!(
            chain_combinator("p", " > ", r#"[name="a,b"]"#),
            r#"p > [name="a,b"]"#,
        );
    }

    // ── find_elements_in_tree_group ────────────────────────────────────────

    /// Mini tree fixture for group-matching tests.
    ///
    /// Shape (DFS order, names in []):
    /// ```text
    /// root (application)
    ///   ├── toolbar [T]
    ///   │   ├── button [Clear]
    ///   │   ├── text_field [Search]
    ///   │   └── button [Save]
    ///   └── dialog [D]
    ///       ├── button [Cancel]
    ///       └── text_field [Password]
    /// ```
    struct GroupRow {
        handle: u64,
        role: Role,
        name: &'static str,
        parent: Option<u64>,
    }

    fn group_fixture() -> Vec<GroupRow> {
        vec![
            GroupRow {
                handle: 0,
                role: Role::Application,
                name: "root",
                parent: None,
            },
            GroupRow {
                handle: 1,
                role: Role::Toolbar,
                name: "T",
                parent: Some(0),
            },
            GroupRow {
                handle: 2,
                role: Role::Button,
                name: "Clear",
                parent: Some(1),
            },
            GroupRow {
                handle: 3,
                role: Role::TextField,
                name: "Search",
                parent: Some(1),
            },
            GroupRow {
                handle: 4,
                role: Role::Button,
                name: "Save",
                parent: Some(1),
            },
            GroupRow {
                handle: 5,
                role: Role::Dialog,
                name: "D",
                parent: Some(0),
            },
            GroupRow {
                handle: 6,
                role: Role::Button,
                name: "Cancel",
                parent: Some(5),
            },
            GroupRow {
                handle: 7,
                role: Role::TextField,
                name: "Password",
                parent: Some(5),
            },
        ]
    }

    fn group_get_children(
        tree: &[GroupRow],
    ) -> impl Fn(Option<&ElementData>) -> Result<Vec<ElementData>> + '_ {
        let make_data = |row: &GroupRow| ElementData {
            role: row.role,
            name: Some(row.name.to_string()),
            value: None,
            description: None,
            bounds: None,
            actions: vec![],
            states: crate::element::StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: Some(1),
            raw: std::collections::HashMap::new(),
            handle: row.handle,
        };
        move |parent: Option<&ElementData>| -> Result<Vec<ElementData>> {
            let parent_handle = parent.map(|e| e.handle);
            Ok(tree
                .iter()
                .filter(|row| row.parent == parent_handle)
                .map(make_data)
                .collect())
        }
    }

    fn names(elements: &[ElementData]) -> Vec<String> {
        elements
            .iter()
            .map(|e| e.name.clone().unwrap_or_default())
            .collect()
    }

    #[test]
    fn group_match_union_in_document_order() {
        let tree = group_fixture();
        let group = SelectorGroup::parse("button, text_field").unwrap();
        let results =
            find_elements_in_tree_group(group_get_children(&tree), None, &group, None, None)
                .unwrap();
        // Document order is DFS: Clear, Search, Save, Cancel, Password.
        // NOT [all buttons, then all text_fields] — that would be the naive
        // concat order this test guards against.
        assert_eq!(
            names(&results),
            vec!["Clear", "Search", "Save", "Cancel", "Password"],
        );
    }

    #[test]
    fn group_match_dedup_overlapping_clauses() {
        // Both clauses match the same element (`Clear`). The union must
        // include it exactly once.
        let tree = group_fixture();
        let group = SelectorGroup::parse(r#"button[name="Clear"], button[name*="lea"]"#).unwrap();
        let results =
            find_elements_in_tree_group(group_get_children(&tree), None, &group, None, None)
                .unwrap();
        assert_eq!(names(&results), vec!["Clear"]);
    }

    #[test]
    fn group_match_combinator_per_clause() {
        // `toolbar button, dialog button` — each clause has its own
        // combinator chain. Result: buttons under toolbar OR under dialog,
        // in document order.
        let tree = group_fixture();
        let group = SelectorGroup::parse("toolbar button, dialog button").unwrap();
        let results =
            find_elements_in_tree_group(group_get_children(&tree), None, &group, None, None)
                .unwrap();
        assert_eq!(names(&results), vec!["Clear", "Save", "Cancel"]);
    }

    #[test]
    fn group_match_limit_truncates_in_document_order() {
        let tree = group_fixture();
        let group = SelectorGroup::parse("button, text_field").unwrap();
        let results =
            find_elements_in_tree_group(group_get_children(&tree), None, &group, Some(2), None)
                .unwrap();
        assert_eq!(names(&results), vec!["Clear", "Search"]);
    }

    #[test]
    fn group_match_single_clause_matches_single_path() {
        // A 1-clause group must produce identical output to find_elements_in_tree.
        let tree = group_fixture();
        let sel_single = Selector::parse("button").unwrap();
        let group_single = SelectorGroup::parse("button").unwrap();

        let via_single =
            find_elements_in_tree(group_get_children(&tree), None, &sel_single, None, None)
                .unwrap();
        let via_group =
            find_elements_in_tree_group(group_get_children(&tree), None, &group_single, None, None)
                .unwrap();
        assert_eq!(names(&via_single), names(&via_group));
    }

    #[test]
    fn group_match_no_matches_returns_empty() {
        // None of the clauses match — result is empty, not an error.
        let tree = group_fixture();
        let group = SelectorGroup::parse(r#"button[name="Nope"], slider"#).unwrap();
        let results =
            find_elements_in_tree_group(group_get_children(&tree), None, &group, None, None)
                .unwrap();
        assert!(results.is_empty());
    }

    /// `get_children` closure that mimics how real platform providers behave:
    /// each call allocates a fresh handle for every returned element, even for
    /// the same logical node. The Windows, macOS, and Linux backends all do
    /// this (each `cache_element` bumps a `NEXT_HANDLE` atomic), so two walks
    /// of the same tree see entirely disjoint handle values for the same
    /// nodes.
    ///
    /// The fixture identifies parents internally by name (stable across calls)
    /// while the returned `ElementData.handle` is freshly minted every time —
    /// this is exactly the shape that surfaced as "comma selector returned 0
    /// results when it should have matched" in real usage.
    fn fresh_handle_get_children(
        tree: &[GroupRow],
    ) -> impl Fn(Option<&ElementData>) -> Result<Vec<ElementData>> + '_ {
        let next = std::cell::RefCell::new(1_000_000u64);
        // Map from freshly-minted handle → stable row name, so subsequent calls
        // can find the parent's row by handle even though row.handle ≠ the
        // freshly-minted handle we previously returned.
        let by_handle: std::cell::RefCell<std::collections::HashMap<u64, &'static str>> =
            std::cell::RefCell::new(std::collections::HashMap::new());

        move |parent: Option<&ElementData>| -> Result<Vec<ElementData>> {
            let parent_row_handle = match parent {
                None => None,
                Some(p) => {
                    let name = by_handle.borrow().get(&p.handle).copied().or_else(|| {
                        tree.iter()
                            .find(|r| Some(r.name) == p.name.as_deref())
                            .map(|r| r.name)
                    });
                    name.and_then(|n| tree.iter().find(|r| r.name == n).map(|r| r.handle))
                }
            };
            let mut out = Vec::new();
            for row in tree.iter().filter(|r| r.parent == parent_row_handle) {
                let fresh = {
                    let mut n = next.borrow_mut();
                    let v = *n;
                    *n += 1;
                    v
                };
                by_handle.borrow_mut().insert(fresh, row.name);
                out.push(ElementData {
                    role: row.role,
                    name: Some(row.name.to_string()),
                    value: None,
                    description: None,
                    bounds: None,
                    actions: vec![],
                    states: crate::element::StateSet::default(),
                    numeric_value: None,
                    min_value: None,
                    max_value: None,
                    stable_id: None,
                    pid: Some(1),
                    raw: std::collections::HashMap::new(),
                    handle: fresh,
                });
            }
            Ok(out)
        }
    }

    #[test]
    fn group_match_works_with_fresh_handles_per_walk() {
        // Regression: the doc-order merge previously identified clause-match
        // results by `ElementData.handle` and re-walked the tree to recover
        // document order, looking up visited nodes by that same handle. On
        // every real platform backend `get_children` allocates a fresh handle
        // for each returned element on every call, so the re-walk's handles
        // never matched the per-clause walks' handles — the lookup missed
        // every time and the function returned 0 results.
        //
        // This test pins the fix: a fresh-handle `get_children` (one new
        // handle per element per call) must still yield the full document-
        // order union.
        let tree = group_fixture();
        let group = SelectorGroup::parse("button, text_field").unwrap();
        let results =
            find_elements_in_tree_group(fresh_handle_get_children(&tree), None, &group, None, None)
                .unwrap();
        assert_eq!(
            names(&results),
            vec!["Clear", "Search", "Save", "Cancel", "Password"],
            "comma selector must work when get_children allocates fresh handles",
        );
    }

    #[test]
    fn group_match_fresh_handles_combinator_per_clause() {
        // Same fresh-handle regression for multi-segment clauses: `toolbar
        // button, dialog button` runs two clauses, each with a Descendant
        // combinator. Pre-fix, this returned 0 results on real providers.
        let tree = group_fixture();
        let group = SelectorGroup::parse("toolbar button, dialog button").unwrap();
        let results =
            find_elements_in_tree_group(fresh_handle_get_children(&tree), None, &group, None, None)
                .unwrap();
        assert_eq!(names(&results), vec!["Clear", "Save", "Cancel"]);
    }

    #[test]
    fn group_match_fresh_handles_overlapping_clauses_dedup() {
        // Fresh-handle dedup: two clauses both match the same node. The
        // result must include it exactly once, even though each clause walk
        // mints a different handle for it.
        let tree = group_fixture();
        let group = SelectorGroup::parse(r#"button[name="Clear"], button[name*="lea"]"#).unwrap();
        let results =
            find_elements_in_tree_group(fresh_handle_get_children(&tree), None, &group, None, None)
                .unwrap();
        assert_eq!(names(&results), vec!["Clear"]);
    }

    #[test]
    fn group_match_fresh_handles_limit_truncates_in_document_order() {
        // Limit must still apply in document order, not in clause order.
        let tree = group_fixture();
        let group = SelectorGroup::parse("button, text_field").unwrap();
        let results = find_elements_in_tree_group(
            fresh_handle_get_children(&tree),
            None,
            &group,
            Some(2),
            None,
        )
        .unwrap();
        assert_eq!(names(&results), vec!["Clear", "Search"]);
    }

    // ── SelectorGroup parsing — additional edge cases ──────────────────────

    #[test]
    fn split_top_level_commas_unicode_around_commas() {
        // Multi-byte chars adjacent to commas must not throw off the byte
        // accounting — `split_top_level_commas` iterates `chars()` so this
        // is just a sanity check that the contract holds for non-ASCII.
        assert_eq!(
            split_top_level_commas("café,naïve"),
            vec!["café".to_string(), "naïve".to_string()],
        );
    }

    #[test]
    fn split_top_level_commas_nested_quotes() {
        // A single-quoted value containing a double quote, followed by a
        // double-quoted value containing a single quote — quotes must close
        // against their own delimiter, not the opposite type.
        assert_eq!(
            split_top_level_commas(r#"[name='a"b,c'], [name="x'y,z"]"#),
            vec![
                r#"[name='a"b,c']"#.to_string(),
                r#" [name="x'y,z"]"#.to_string(),
            ],
        );
    }

    #[test]
    fn parse_group_attribute_only_clauses() {
        // Clauses with no role (attribute filters only) must parse correctly
        // on both sides of the comma.
        let group = SelectorGroup::parse(r#"[name="A"], [name="B"]"#).unwrap();
        assert_eq!(group.clauses.len(), 2);
        assert!(group.clauses[0].segments[0].simple.role.is_none());
        assert!(group.clauses[1].segments[0].simple.role.is_none());
        assert_eq!(group.clauses[0].segments[0].simple.filters[0].value, "A");
        assert_eq!(group.clauses[1].segments[0].simple.filters[0].value, "B");
    }

    #[test]
    fn parse_group_role_only_then_attribute_only() {
        // Mixing role-only and attribute-only clauses: each clause must
        // parse independently.
        let group = SelectorGroup::parse(r#"button, [name="X"]"#).unwrap();
        assert_eq!(group.clauses.len(), 2);
        assert!(matches!(
            group.clauses[0].segments[0].simple.role,
            Some(RoleMatch::Normalized(Role::Button))
        ));
        assert!(group.clauses[1].segments[0].simple.role.is_none());
    }

    #[test]
    fn parse_group_multiple_quoted_commas_in_one_clause() {
        // Two attribute filters in the same clause, both containing commas.
        // The clause must not be split, and both values must survive parsing.
        let group =
            SelectorGroup::parse(r#"button[name="a,b"][description="x,y"], slider"#).unwrap();
        assert_eq!(group.clauses.len(), 2);
        assert_eq!(group.clauses[0].segments[0].simple.filters.len(), 2);
        assert_eq!(group.clauses[0].segments[0].simple.filters[0].value, "a,b");
        assert_eq!(group.clauses[0].segments[0].simple.filters[1].value, "x,y");
        assert!(matches!(
            group.clauses[1].segments[0].simple.role,
            Some(RoleMatch::Normalized(Role::Slider))
        ));
    }

    #[test]
    fn parse_group_nth_per_clause() {
        // `:nth(N)` is per-clause; both clauses must parse with their own
        // nth values.
        let group = SelectorGroup::parse("button:nth(1), text_field:nth(2)").unwrap();
        assert_eq!(group.clauses.len(), 2);
        assert_eq!(group.clauses[0].segments[0].simple.nth, Some(1));
        assert_eq!(group.clauses[1].segments[0].simple.nth, Some(2));
    }

    #[test]
    fn parse_group_unterminated_quote_propagates_error() {
        // An unterminated quoted value inside one clause must surface as a
        // parse error from that clause — the unterminated string makes
        // `split_top_level_commas` swallow the rest of the input as one
        // tail clause, which then fails `Selector::parse`.
        let err = SelectorGroup::parse(r#"button, [name="oops"#);
        assert!(err.is_err(), "unterminated quote must error: {err:?}");
    }

    #[test]
    fn parse_group_trailing_whitespace_is_tolerated() {
        // Surrounding whitespace on the whole input is trimmed; per-clause
        // whitespace also trims. None of this should produce empty clauses.
        let group = SelectorGroup::parse("   button  ,   text_field   ").unwrap();
        assert_eq!(group.clauses.len(), 2);
        assert!(matches!(
            group.clauses[0].segments[0].simple.role,
            Some(RoleMatch::Normalized(Role::Button))
        ));
        assert!(matches!(
            group.clauses[1].segments[0].simple.role,
            Some(RoleMatch::Normalized(Role::TextField))
        ));
    }

    #[test]
    fn chain_combinator_handles_trailing_whitespace() {
        // `chain_combinator` must trim each clause when stitching, so trailing
        // whitespace from prior chaining doesn't leak into the produced
        // selector string and break later parses.
        assert_eq!(chain_combinator("a , b ", " ", " c "), "a c, b c");
    }

    #[test]
    fn chain_combinator_preserves_attribute_filters_in_groups() {
        // Attribute filters survive the round-trip through chain_combinator
        // — important because Locator::descendant / child use this when
        // chaining off a group locator that has filters.
        assert_eq!(
            chain_combinator(r#"button[name="A"], button[name="B"]"#, " ", "label"),
            r#"button[name="A"] label, button[name="B"] label"#,
        );
    }

    #[test]
    fn chain_combinator_round_trips_through_parser() {
        // chain_combinator's docs promise its output re-parses as a
        // SelectorGroup. Test that contract end-to-end so regressions in
        // either side surface immediately.
        let cases = &[
            ("a, b", " ", "c"),
            ("a, b", " > ", "c, d"),
            (r#"[name="a,b"]"#, " ", "c"),
            ("toolbar, group", " > ", r#"button[name="OK"]"#),
        ];
        for (existing, comb, suffix) in cases {
            let stitched = chain_combinator(existing, comb, suffix);
            SelectorGroup::parse(&stitched).unwrap_or_else(|e| {
                panic!("chain_combinator output {stitched:?} must re-parse, got {e:?}")
            });
        }
    }

    // ── find_elements_in_tree_group — additional matching cases ────────────

    #[test]
    fn group_match_three_clauses_doc_order() {
        // Three clauses, each matching a different role. Result must
        // interleave by document position across all three.
        let tree = group_fixture();
        let group = SelectorGroup::parse("toolbar, dialog, button").unwrap();
        let results =
            find_elements_in_tree_group(group_get_children(&tree), None, &group, None, None)
                .unwrap();
        // DFS order: T(toolbar), Clear(button), Save(button), D(dialog), Cancel(button).
        assert_eq!(names(&results), vec!["T", "Clear", "Save", "D", "Cancel"]);
    }

    #[test]
    fn group_match_attribute_only_clauses_against_tree() {
        // Attribute-only clauses (no role) — both clauses must traverse the
        // full tree and match by attribute alone.
        let tree = group_fixture();
        let group = SelectorGroup::parse(r#"[name="Clear"], [name="Password"]"#).unwrap();
        let results =
            find_elements_in_tree_group(group_get_children(&tree), None, &group, None, None)
                .unwrap();
        assert_eq!(names(&results), vec!["Clear", "Password"]);
    }

    #[test]
    fn group_match_with_scoped_root() {
        // Scope the search to the toolbar subtree. Only descendants of the
        // toolbar match — the dialog branch must be excluded entirely, even
        // for clauses that would match nodes there at the root level.
        let tree = group_fixture();
        let toolbar = ElementData {
            role: Role::Toolbar,
            name: Some("T".to_string()),
            value: None,
            description: None,
            bounds: None,
            actions: vec![],
            states: crate::element::StateSet::default(),
            numeric_value: None,
            min_value: None,
            max_value: None,
            stable_id: None,
            pid: Some(1),
            raw: std::collections::HashMap::new(),
            handle: 1,
        };
        let group = SelectorGroup::parse("button, text_field").unwrap();
        let results = find_elements_in_tree_group(
            group_get_children(&tree),
            Some(&toolbar),
            &group,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            names(&results),
            vec!["Clear", "Search", "Save"],
            "scoped search must exclude dialog-subtree matches",
        );
    }

    #[test]
    fn group_match_max_depth_zero_returns_top_level_only() {
        // max_depth=0 means we visit children of root but not grandchildren.
        // From system root, this yields only the application node.
        let tree = group_fixture();
        let group = SelectorGroup::parse("application, toolbar").unwrap();
        let results =
            find_elements_in_tree_group(group_get_children(&tree), None, &group, None, Some(0))
                .unwrap();
        // Only "root" (application) is at depth 0 here; toolbar is deeper.
        assert_eq!(names(&results), vec!["root"]);
    }

    #[test]
    fn group_match_empty_group_clauses_returns_empty() {
        // Defensive: a group whose every clause has zero matches yields an
        // empty Vec, never an error.
        let tree = group_fixture();
        let group =
            SelectorGroup::parse(r#"button[name="ZZZ"], dialog[name="QQQ"], slider"#).unwrap();
        let results =
            find_elements_in_tree_group(group_get_children(&tree), None, &group, None, None)
                .unwrap();
        assert!(results.is_empty());
    }

    // ── find_elements_in_tree_with_paths — path identity contract ──────────

    #[test]
    fn paths_match_dfs_indices_from_root() {
        // Paths are the sole stable identity used to dedup multi-clause
        // groups. Lock the contract: a single-segment clause matching `button`
        // against the fixture must produce paths that correspond to the
        // DFS-position of each match.
        //
        // Tree (DFS):
        //   root(0) → toolbar(1) → Clear(2), Search(3), Save(4)
        //          → dialog(5)  → Cancel(6), Password(7)
        // Buttons live at:
        //   Clear  = root/toolbar[0]/button[0] → path [0, 0, 0]
        //   Save   = root/toolbar[0]/button[2] → path [0, 0, 2]
        //   Cancel = root/dialog[1]/button[0]  → path [0, 1, 0]
        let tree = group_fixture();
        let sel = Selector::parse("button").unwrap();
        let paths_and_data =
            find_elements_in_tree_with_paths(group_get_children(&tree), None, &sel, None).unwrap();
        let just_paths: Vec<Vec<u32>> = paths_and_data.iter().map(|(p, _)| p.clone()).collect();
        let just_names: Vec<String> = paths_and_data
            .iter()
            .map(|(_, e)| e.name.clone().unwrap_or_default())
            .collect();
        assert_eq!(just_names, vec!["Clear", "Save", "Cancel"]);
        assert_eq!(
            just_paths,
            vec![vec![0, 0, 0], vec![0, 0, 2], vec![0, 1, 0]],
        );
    }

    #[test]
    fn paths_are_stable_across_repeated_walks() {
        // Running the same selector twice through `find_elements_in_tree_with_paths`
        // must produce identical paths — this is the invariant the doc-order
        // merge depends on. (The mock here uses stable handles, but the
        // assertion is about path stability, which holds regardless of handle
        // semantics.)
        let tree = group_fixture();
        let sel = Selector::parse("button").unwrap();
        let a =
            find_elements_in_tree_with_paths(group_get_children(&tree), None, &sel, None).unwrap();
        let b =
            find_elements_in_tree_with_paths(group_get_children(&tree), None, &sel, None).unwrap();
        let a_paths: Vec<Vec<u32>> = a.iter().map(|(p, _)| p.clone()).collect();
        let b_paths: Vec<Vec<u32>> = b.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(a_paths, b_paths, "paths must be stable across walks");
        assert_eq!(a_paths.len(), 3, "expected 3 button matches in fixture");
    }

    #[test]
    fn paths_with_combinator_chain_extend_through_phase2() {
        // For multi-segment selectors, the phase-2 path must extend the
        // phase-1 candidate's path with the descendants' offsets. A
        // `toolbar > button` selector should produce paths rooted in the
        // toolbar's path, not in the button's local walk.
        let tree = group_fixture();
        let sel = Selector::parse("toolbar > button").unwrap();
        let results =
            find_elements_in_tree_with_paths(group_get_children(&tree), None, &sel, None).unwrap();
        let paths: Vec<Vec<u32>> = results.iter().map(|(p, _)| p.clone()).collect();
        // Toolbar is at root/[0]/[0] (path [0,0]); its button children at
        // offsets 0 and 2 (Clear, Save). So:
        //   Clear → [0, 0, 0]
        //   Save  → [0, 0, 2]
        assert_eq!(paths, vec![vec![0, 0, 0], vec![0, 0, 2]]);
    }

    #[test]
    fn group_match_clause_with_no_role_and_filter() {
        // A clause that's just `[name*="..."]` (no role) — exercised against
        // the tree to confirm filter-only matching is intact in groups.
        let tree = group_fixture();
        let group = SelectorGroup::parse(r#"[name*="ear"], [name*="ass"]"#).unwrap();
        let results =
            find_elements_in_tree_group(group_get_children(&tree), None, &group, None, None)
                .unwrap();
        // "Clear" (contains "ear"), "Search" (contains "ear"), "Password"
        // (contains "ass"). Doc order: Clear, Search, Password.
        assert_eq!(names(&results), vec!["Clear", "Search", "Password"]);
    }
}
