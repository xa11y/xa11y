use std::fmt;

use serde::{Deserialize, Serialize};

use crate::element::{ElementData, ElementIndex};
use crate::error::Result;
use crate::selector::Selector;

/// A snapshot of an application's accessibility tree.
///
/// The tree is a flattened snapshot — elements are stored in DFS order and
/// reference each other by internal indices.
///
/// Internal to xa11y-core. Consumers interact through [`Element`](crate::Element).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Tree {
    /// Application name
    pub(crate) app_name: String,

    /// Process ID. `None` for multi-app queries.
    pub(crate) pid: Option<u32>,

    /// Screen dimensions at capture time (width, height)
    pub(crate) screen_size: (u32, u32),

    /// All elements in DFS order (access through methods)
    elements: Vec<ElementData>,
}

impl Tree {
    /// Create a new Tree from a list of elements.
    pub(crate) fn new(
        app_name: String,
        pid: Option<u32>,
        screen_size: (u32, u32),
        elements: Vec<ElementData>,
    ) -> Self {
        Self {
            app_name,
            pid,
            screen_size,
            elements,
        }
    }

    /// Get an element's data by its internal index.
    pub(crate) fn get_data(&self, index: u32) -> Option<&ElementData> {
        self.elements.get(index as usize)
    }

    /// Get direct children of an element.
    pub(crate) fn children_data(&self, element: &ElementData) -> Vec<&ElementData> {
        element
            .children_indices
            .iter()
            .filter_map(|&idx| self.elements.get(idx as usize))
            .collect()
    }

    /// Get indices of the subtree rooted at an element (including the element itself).
    pub(crate) fn subtree_indices(&self, index: u32) -> Vec<u32> {
        let mut result = Vec::new();
        self.collect_subtree_indices(index, &mut result);
        result
    }

    fn collect_subtree_indices(&self, index: ElementIndex, result: &mut Vec<u32>) {
        if let Some(element) = self.elements.get(index as usize) {
            result.push(index);
            for &child_idx in &element.children_indices {
                self.collect_subtree_indices(child_idx, result);
            }
        }
    }

    /// Iterate all element data.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &ElementData> {
        self.elements.iter()
    }

    /// Query element indices matching a CSS-like selector string.
    pub(crate) fn query_indices(&self, selector_str: &str) -> Result<Vec<u32>> {
        let selector = Selector::parse(selector_str)?;
        Ok(selector
            .match_elements(self)
            .iter()
            .map(|e| e.index)
            .collect())
    }

    /// Query elements matching a CSS-like selector string (returns ElementData refs).
    pub(crate) fn query(&self, selector_str: &str) -> Result<Vec<&ElementData>> {
        let selector = Selector::parse(selector_str)?;
        Ok(selector.match_elements(self))
    }
}

impl fmt::Display for Tree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Compute depth from parent_index so ElementData doesn't need a depth field.
        let mut depths = vec![0u32; self.elements.len()];
        for element in &self.elements {
            let d = depths[element.index as usize];
            for &child_idx in &element.children_indices {
                if let Some(cd) = depths.get_mut(child_idx as usize) {
                    *cd = d + 1;
                }
            }
        }

        for element in &self.elements {
            let depth = depths.get(element.index as usize).copied().unwrap_or(0);
            let indent = "  ".repeat(depth as usize);
            let name_part = element
                .name
                .as_ref()
                .map(|n| format!(" \"{}\"", n))
                .unwrap_or_default();
            let value_part = element
                .value
                .as_ref()
                .map(|v| format!(" value=\"{}\"", v))
                .unwrap_or_default();
            writeln!(
                f,
                "{}[{}] {}{}{}",
                indent,
                element.index,
                element.role.to_snake_case(),
                name_part,
                value_part,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{RawPlatformData, Rect, StateSet};
    use crate::role::Role;

    fn sample_tree() -> Tree {
        let elements = vec![
            ElementData {
                role: Role::Window,
                name: Some("My App".to_string()),
                value: None,
                description: None,
                bounds: Some(Rect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                }),
                actions: vec![],
                states: StateSet::default(),
                pid: None,
                stable_id: None,
                numeric_value: None,
                min_value: None,
                max_value: None,
                raw: RawPlatformData::Synthetic,
                index: 0,
                children_indices: vec![1],
                parent_index: None,
            },
            ElementData {
                role: Role::Button,
                name: Some("Submit".to_string()),
                value: None,
                description: None,
                bounds: None,
                actions: vec![],
                states: StateSet::default(),
                pid: None,
                stable_id: None,
                numeric_value: None,
                min_value: None,
                max_value: None,
                raw: RawPlatformData::Synthetic,
                index: 1,
                children_indices: vec![],
                parent_index: Some(0),
            },
        ];
        Tree::new("My App".to_string(), Some(1234), (1920, 1080), elements)
    }

    /// MAX_TREE_DEPTH prevents stack overflow from cyclic accessibility trees.
    ///
    /// Some toolkits (notably Qt/PySide6) expose the application node as its own
    /// child, creating infinite recursion in the AX tree. All providers (macOS,
    /// Linux, Windows) check `depth > MAX_TREE_DEPTH` before recursing into
    /// children, capping traversal at a safe depth. The value of 50 is well above
    /// any real UI nesting depth but low enough to keep traversal fast even when
    /// cycles exist.
    #[test]
    fn max_tree_depth_is_reasonable() {
        // Providers rely on this constant; changing it affects cycle protection.
        assert_eq!(crate::MAX_TREE_DEPTH, 50);
        // Must be high enough for deeply nested UIs (dialogs in tabs in panels)
        assert!(crate::MAX_TREE_DEPTH >= 30);
        // Must be low enough to terminate quickly on cyclic trees
        assert!(crate::MAX_TREE_DEPTH <= 100);
    }

    #[test]
    fn tree_json_roundtrip() {
        let tree = sample_tree();
        let json = serde_json::to_string(&tree).unwrap();
        let deserialized: Tree = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.app_name, "My App");
        assert_eq!(deserialized.pid, Some(1234));
        assert_eq!(deserialized.screen_size, (1920, 1080));
        assert_eq!(deserialized.elements.len(), 2);

        let root = deserialized.get_data(0).unwrap();
        assert_eq!(root.role, Role::Window);

        let buttons = deserialized.query("button").unwrap();
        assert_eq!(buttons.len(), 1);
    }
}
