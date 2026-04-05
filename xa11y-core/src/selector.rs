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

use crate::element::ElementData;
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
/// it is looked up in the element's `attributes` map at match time.
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
                // Check raw platform role fields: ax_role (macOS), atspi_role (Linux),
                // or control_type_id (Windows).
                let matches = element
                    .raw
                    .values()
                    .any(|v| v.as_str().is_some_and(|s| s == platform_role));
                if !matches {
                    return false;
                }
            }
        }
    }

    // Check attribute filters — look up each attribute in the element's attributes map.
    for filter in &simple.filters {
        let attr_value: Option<String> = element.attributes.get(&filter.attr).and_then(|v| {
            match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Bool(b) => Some(b.to_string()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                serde_json::Value::Null => None,
                // Arrays/objects: convert to JSON string for matching
                other => Some(other.to_string()),
            }
        });

        if !match_op(&filter.op, &filter.value, attr_value.as_deref()) {
            return false;
        }
    }

    true
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
    }

    // Apply :nth on the last segment
    if let Some(nth) = selector.segments.last().and_then(|s| s.simple.nth) {
        if nth <= candidates.len() {
            candidates = vec![candidates.remove(nth - 1)];
        } else {
            candidates.clear();
        }
    }

    // Apply limit
    if let Some(limit) = limit {
        candidates.truncate(limit);
    }

    Ok(candidates)
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
    fn parse_adjacent_nth_is_error() {
        // A second :nth() with no combinator between them would previously
        // parse as Ok but produce a Root-combinator segment in a non-first
        // position, causing an unreachable!() panic in find_elements_in_tree.
        assert!(Selector::parse("button:nth(1):nth(2)").is_err());
    }
}
