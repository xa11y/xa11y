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
/// This is a public type representing an accessibility tree snapshot.
/// End users typically interact with it through [`Element`](crate::Element)
/// (returned by `xa11y::app()`, etc.) which wraps a `Tree` with
/// navigation methods.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tree {
    /// Application name
    pub app_name: String,

    /// Process ID. `None` for multi-app queries.
    pub pid: Option<u32>,

    /// Screen dimensions at capture time (width, height)
    pub screen_size: (u32, u32),

    /// All elements in DFS order (access through methods)
    elements: Vec<ElementData>,
}

impl Tree {
    /// Create a new Tree from a list of elements.
    pub fn new(
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
    pub fn get_data(&self, index: u32) -> Option<&ElementData> {
        self.elements.get(index as usize)
    }

    /// Get the root element data.
    pub fn root_data(&self) -> &ElementData {
        &self.elements[0]
    }

    /// Get the parent of an element.
    pub fn parent_data(&self, element: &ElementData) -> Option<&ElementData> {
        element
            .parent_index
            .and_then(|idx| self.elements.get(idx as usize))
    }

    /// Get direct children of an element.
    pub fn children_data(&self, element: &ElementData) -> Vec<&ElementData> {
        element
            .children_indices
            .iter()
            .filter_map(|&idx| self.elements.get(idx as usize))
            .collect()
    }

    /// Get indices of the subtree rooted at an element (including the element itself).
    pub fn subtree_indices(&self, index: u32) -> Vec<u32> {
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
    pub fn iter(&self) -> impl Iterator<Item = &ElementData> {
        self.elements.iter()
    }

    /// Query element indices matching a CSS-like selector string.
    pub fn query_indices(&self, selector_str: &str) -> Result<Vec<u32>> {
        let selector = Selector::parse(selector_str)?;
        Ok(selector
            .match_elements(self)
            .iter()
            .map(|e| e.index)
            .collect())
    }

    /// Query elements matching a CSS-like selector string (returns ElementData refs).
    pub fn query(&self, selector_str: &str) -> Result<Vec<&ElementData>> {
        let selector = Selector::parse(selector_str)?;
        Ok(selector.match_elements(self))
    }

    /// Get the number of elements in the tree.
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    /// Check if the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Get the internal element index for an element. Used by platform providers
    /// to look up cached element handles.
    #[doc(hidden)]
    pub fn element_index(&self, element: &ElementData) -> ElementIndex {
        element.index
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
