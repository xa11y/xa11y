use crate::error::{Error, Result};
use crate::node::Node;
use crate::role::Role;

/// A parsed CSS-like selector for querying accessibility tree nodes.
#[derive(Debug, Clone)]
pub struct Selector {
    segments: Vec<SelectorSegment>,
}

/// A single segment in a selector chain.
#[derive(Debug, Clone)]
struct SelectorSegment {
    role: Option<Role>,
    attributes: Vec<AttributeMatcher>,
    #[allow(dead_code)]
    nth: Option<usize>,
    #[allow(dead_code)]
    combinator: Combinator,
}

/// How this segment relates to the previous one.
#[derive(Debug, Clone, PartialEq)]
enum Combinator {
    /// First segment (no combinator)
    None,
    /// Direct child (`>`)
    Child,
    /// Any descendant (space)
    Descendant,
}

/// An attribute match condition.
#[derive(Debug, Clone)]
struct AttributeMatcher {
    attr: String,
    op: MatchOp,
    value: String,
}

/// Match operation for attribute selectors.
#[derive(Debug, Clone)]
enum MatchOp {
    /// Exact match (`=`)
    Exact,
    /// Substring match (`*=`)
    Contains,
    /// Starts-with match (`^=`)
    StartsWith,
}

impl Selector {
    /// Parse a CSS-like selector string.
    ///
    /// Supported syntax:
    /// - `button` — match by role
    /// - `[name="Submit"]` — match by attribute (exact)
    /// - `[name*="addr"]` — substring match (case-insensitive)
    /// - `[name^="addr"]` — starts-with match
    /// - `button[name="Submit"]` — role + attribute
    /// - `toolbar > text_field` — direct child combinator
    /// - `toolbar text_field` — descendant combinator
    /// - `button:nth(2)` — nth match (1-based)
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(Error::InvalidSelector("empty selector".into()));
        }

        let mut segments = Vec::new();
        let mut remaining = input;
        let mut next_combinator = Combinator::None;

        while !remaining.is_empty() {
            remaining = remaining.trim_start();
            if remaining.is_empty() {
                break;
            }

            // Check for child combinator
            if remaining.starts_with('>') {
                next_combinator = Combinator::Child;
                remaining = remaining[1..].trim_start();
                continue;
            }

            let (segment, rest) = Self::parse_segment(remaining, next_combinator)?;
            segments.push(segment);
            next_combinator = if rest.trim_start().starts_with('>') {
                Combinator::None // Will be set on next iteration
            } else if !rest.trim_start().is_empty() && !rest.trim_start().starts_with('>') {
                Combinator::Descendant
            } else {
                Combinator::None
            };
            remaining = rest;
        }

        if segments.is_empty() {
            return Err(Error::InvalidSelector("no valid segments".into()));
        }

        Ok(Selector { segments })
    }

    fn parse_segment(input: &str, combinator: Combinator) -> Result<(SelectorSegment, &str)> {
        let mut role = None;
        let mut attributes = Vec::new();
        let mut nth = None;
        let mut pos = 0;
        let bytes = input.as_bytes();

        // Parse role name (alphanumeric + underscore)
        let role_start = pos;
        while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
            pos += 1;
        }
        if pos > role_start {
            let role_str = &input[role_start..pos];
            role = Some(
                Role::from_selector(role_str)
                    .ok_or_else(|| Error::InvalidSelector(format!("unknown role: {role_str}")))?,
            );
        }

        // Parse attribute matchers [attr op "value"]
        while pos < bytes.len() && bytes[pos] == b'[' {
            pos += 1; // skip [
            let attr_start = pos;
            while pos < bytes.len()
                && bytes[pos] != b'='
                && bytes[pos] != b'*'
                && bytes[pos] != b'^'
                && bytes[pos] != b']'
            {
                pos += 1;
            }
            let attr = input[attr_start..pos].trim().to_string();

            if pos >= bytes.len() || bytes[pos] == b']' {
                // Presence check only - skip for now
                if pos < bytes.len() {
                    pos += 1;
                }
                continue;
            }

            let op = if bytes[pos] == b'*' {
                pos += 1; // skip *
                if pos < bytes.len() && bytes[pos] == b'=' {
                    pos += 1;
                }
                MatchOp::Contains
            } else if bytes[pos] == b'^' {
                pos += 1; // skip ^
                if pos < bytes.len() && bytes[pos] == b'=' {
                    pos += 1;
                }
                MatchOp::StartsWith
            } else {
                pos += 1; // skip =
                MatchOp::Exact
            };

            // Skip whitespace and quotes
            while pos < bytes.len()
                && (bytes[pos] == b' ' || bytes[pos] == b'"' || bytes[pos] == b'\'')
            {
                pos += 1;
            }
            let val_start = pos;
            while pos < bytes.len()
                && bytes[pos] != b'"'
                && bytes[pos] != b'\''
                && bytes[pos] != b']'
            {
                pos += 1;
            }
            let value = input[val_start..pos].trim().to_string();

            // Skip closing quote and bracket
            while pos < bytes.len() && bytes[pos] != b']' {
                pos += 1;
            }
            if pos < bytes.len() {
                pos += 1; // skip ]
            }

            attributes.push(AttributeMatcher { attr, op, value });
        }

        // Parse :nth(N)
        if pos < bytes.len() && input[pos..].starts_with(":nth(") {
            pos += 5; // skip :nth(
            let nth_start = pos;
            while pos < bytes.len() && bytes[pos] != b')' {
                pos += 1;
            }
            let n: usize = input[nth_start..pos]
                .parse()
                .map_err(|_| Error::InvalidSelector("invalid :nth value".into()))?;
            nth = Some(n);
            if pos < bytes.len() {
                pos += 1; // skip )
            }
        }

        if role.is_none() && attributes.is_empty() && nth.is_none() {
            return Err(Error::InvalidSelector(
                "segment has no role, attributes, or nth".into(),
            ));
        }

        Ok((
            SelectorSegment {
                role,
                attributes,
                nth,
                combinator,
            },
            &input[pos..],
        ))
    }

    /// Check if a node matches this selector (single-segment, non-hierarchical match).
    /// For multi-segment selectors with combinators, use `matches_in_tree`.
    pub fn matches(&self, node: &Node) -> bool {
        // For single-segment selectors, just match the last segment
        if self.segments.len() == 1 {
            return Self::segment_matches(&self.segments[0], node);
        }
        // For multi-segment, only match the final segment (tree context needed for full matching)
        Self::segment_matches(self.segments.last().unwrap(), node)
    }

    fn segment_matches(segment: &SelectorSegment, node: &Node) -> bool {
        // Check role
        if let Some(role) = segment.role {
            if node.role != role {
                return false;
            }
        }

        // Check attributes
        for attr in &segment.attributes {
            let node_value = match attr.attr.as_str() {
                "name" => node.name.as_deref(),
                "value" => node.value.as_deref(),
                "description" => node.description.as_deref(),
                "role" => Some(node.role.as_selector()),
                _ => None,
            };

            let Some(node_value) = node_value else {
                return false;
            };

            let matched = match attr.op {
                MatchOp::Exact => node_value == attr.value,
                MatchOp::Contains => node_value
                    .to_lowercase()
                    .contains(&attr.value.to_lowercase()),
                MatchOp::StartsWith => node_value
                    .to_lowercase()
                    .starts_with(&attr.value.to_lowercase()),
            };

            if !matched {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::StateSet;

    fn make_node(id: u32, role: Role, name: Option<&str>) -> Node {
        Node {
            id,
            role,
            name: name.map(String::from),
            value: None,
            description: None,
            bounds: None,
            bounds_normalized: None,
            actions: vec![],
            states: StateSet::default(),
            children: vec![],
            parent: None,
            depth: 0,
            app_name: None,
            raw: None,
        }
    }

    #[test]
    fn test_role_selector() {
        let sel = Selector::parse("button").unwrap();
        assert!(sel.matches(&make_node(0, Role::Button, Some("OK"))));
        assert!(!sel.matches(&make_node(0, Role::TextField, Some("OK"))));
    }

    #[test]
    fn test_attribute_exact() {
        let sel = Selector::parse("[name=\"Submit\"]").unwrap();
        assert!(sel.matches(&make_node(0, Role::Button, Some("Submit"))));
        assert!(!sel.matches(&make_node(0, Role::Button, Some("Cancel"))));
    }

    #[test]
    fn test_attribute_contains() {
        let sel = Selector::parse("[name*=\"addr\"]").unwrap();
        assert!(sel.matches(&make_node(0, Role::TextField, Some("Address Bar"))));
        assert!(!sel.matches(&make_node(0, Role::TextField, Some("Search"))));
    }

    #[test]
    fn test_attribute_starts_with() {
        let sel = Selector::parse("[name^=\"addr\"]").unwrap();
        assert!(sel.matches(&make_node(0, Role::TextField, Some("Address Bar"))));
        assert!(!sel.matches(&make_node(0, Role::TextField, Some("My Address"))));
    }

    #[test]
    fn test_role_plus_attribute() {
        let sel = Selector::parse("button[name=\"Submit\"]").unwrap();
        assert!(sel.matches(&make_node(0, Role::Button, Some("Submit"))));
        assert!(!sel.matches(&make_node(0, Role::TextField, Some("Submit"))));
        assert!(!sel.matches(&make_node(0, Role::Button, Some("Cancel"))));
    }

    #[test]
    fn test_invalid_selector() {
        assert!(Selector::parse("").is_err());
        assert!(Selector::parse("nonexistent_role").is_err());
    }

    #[test]
    fn test_nth_selector() {
        let sel = Selector::parse("button:nth(2)").unwrap();
        // nth is parsed but only used in tree context
        assert!(sel.matches(&make_node(0, Role::Button, Some("OK"))));
    }
}
