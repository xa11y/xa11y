//! CSS-like selector parser and matcher for accessibility tree queries.
//!
//! Grammar:
//! ```text
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

#[derive(Debug, Clone, PartialEq)]
pub enum MatchOp {
    /// Exact match (case-sensitive)
    Exact,
    /// Substring match (case-insensitive)
    Contains,
    /// Starts-with match (case-insensitive)
    StartsWith,
    /// Ends-with match (case-insensitive)
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
        assert_eq!(resolve_attr(&el, "focusable").as_deref(), Some("false"));
        assert_eq!(resolve_attr(&el, "selected").as_deref(), Some("false"));
        assert_eq!(resolve_attr(&el, "editable").as_deref(), Some("false"));
        assert_eq!(resolve_attr(&el, "modal").as_deref(), Some("false"));
        assert_eq!(resolve_attr(&el, "required").as_deref(), Some("false"));
        assert_eq!(resolve_attr(&el, "busy").as_deref(), Some("false"));
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
}
