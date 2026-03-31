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
//! value         := '"' [^"]* '"'
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

#[derive(Debug, Clone)]
pub struct SimpleSelector {
    pub role: Option<Role>,
    pub filters: Vec<AttrFilter>,
    pub nth: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct AttrFilter {
    pub attr: AttrName,
    pub op: MatchOp,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrName {
    Name,
    Value,
    Description,
    Role,
}

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

        // Try to parse role name (sequence of [a-z_])
        let start = pos;
        while pos < chars.len() && (chars[pos].is_ascii_lowercase() || chars[pos] == '_') {
            pos += 1;
        }
        if pos > start {
            let role_str: String = chars[start..pos].iter().collect();
            match Role::from_snake_case(&role_str) {
                Some(r) => role = Some(r),
                None => {
                    return Err(Error::InvalidSelector {
                        selector: input.to_string(),
                        message: format!("unknown role '{}'", role_str),
                    });
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

        // Parse attribute name
        let attr_start = pos;
        while pos < chars.len() && chars[pos].is_ascii_alphabetic() {
            pos += 1;
        }
        let attr_str: String = chars[attr_start..pos].iter().collect();
        let attr = match attr_str.as_str() {
            "name" => AttrName::Name,
            "value" => AttrName::Value,
            "description" => AttrName::Description,
            "role" => AttrName::Role,
            _ => {
                return Err(Error::InvalidSelector {
                    selector: input.to_string(),
                    message: format!("unknown attribute '{}'", attr_str),
                });
            }
        };

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

        // Parse quoted value
        if pos >= chars.len() || chars[pos] != '"' {
            return Err(Error::InvalidSelector {
                selector: input.to_string(),
                message: "expected '\"' to start attribute value".to_string(),
            });
        }
        pos += 1; // skip opening quote
        let val_start = pos;
        while pos < chars.len() && chars[pos] != '"' {
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
    if let Some(role) = simple.role {
        if element.role != role {
            return false;
        }
    }

    // Check attribute filters
    for filter in &simple.filters {
        let attr_value = match filter.attr {
            AttrName::Name => element.name.as_deref(),
            AttrName::Value => element.value.as_deref(),
            AttrName::Description => element.description.as_deref(),
            AttrName::Role => Some(element.role.to_snake_case()),
        };

        let matches = match &filter.op {
            MatchOp::Exact => attr_value == Some(&filter.value),
            MatchOp::Contains => {
                let filter_lower = filter.value.to_lowercase();
                attr_value.is_some_and(|v| v.to_lowercase().contains(&filter_lower))
            }
            MatchOp::StartsWith => {
                let filter_lower = filter.value.to_lowercase();
                attr_value.is_some_and(|v| v.to_lowercase().starts_with(&filter_lower))
            }
            MatchOp::EndsWith => {
                let filter_lower = filter.value.to_lowercase();
                attr_value.is_some_and(|v| v.to_lowercase().ends_with(&filter_lower))
            }
        };

        if !matches {
            return false;
        }
    }

    true
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
    let phase1_depth = if root.is_none() && first.role == Some(crate::role::Role::Application) {
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
        assert_eq!(sel.segments[0].simple.role, Some(Role::Button));
    }

    #[test]
    fn parse_attr_exact() {
        let sel = Selector::parse(r#"[name="Submit"]"#).unwrap();
        assert_eq!(sel.segments.len(), 1);
        assert!(sel.segments[0].simple.role.is_none());
        assert_eq!(sel.segments[0].simple.filters.len(), 1);
        assert_eq!(sel.segments[0].simple.filters[0].attr, AttrName::Name);
        assert_eq!(sel.segments[0].simple.filters[0].op, MatchOp::Exact);
        assert_eq!(sel.segments[0].simple.filters[0].value, "Submit");
    }

    #[test]
    fn parse_role_and_attr() {
        let sel = Selector::parse(r#"button[name="Submit"]"#).unwrap();
        assert_eq!(sel.segments[0].simple.role, Some(Role::Button));
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
        assert_eq!(sel.segments[0].simple.role, Some(Role::Toolbar));
        assert_eq!(sel.segments[1].combinator, Combinator::Child);
        assert_eq!(sel.segments[1].simple.role, Some(Role::TextField));
    }

    #[test]
    fn parse_descendant_combinator() {
        let sel = Selector::parse("toolbar text_field").unwrap();
        assert_eq!(sel.segments.len(), 2);
        assert_eq!(sel.segments[0].simple.role, Some(Role::Toolbar));
        assert_eq!(sel.segments[1].combinator, Combinator::Descendant);
        assert_eq!(sel.segments[1].simple.role, Some(Role::TextField));
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
        assert_eq!(sel.segments[1].simple.role, Some(Role::TextField));
        assert_eq!(sel.segments[1].simple.filters[0].op, MatchOp::Contains);
        assert_eq!(sel.segments[1].simple.filters[0].value, "Address");
    }

    #[test]
    fn parse_empty_error() {
        assert!(Selector::parse("").is_err());
    }

    #[test]
    fn parse_unknown_role_error() {
        assert!(Selector::parse("foobar").is_err());
    }

    #[test]
    fn parse_nth_zero_error() {
        assert!(Selector::parse("button:nth(0)").is_err());
    }
}
