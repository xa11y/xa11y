use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::node::{NodeData, NodeIndex};
use crate::selector::Selector;

/// A snapshot of an application's accessibility tree.
///
/// The tree is a flattened snapshot — nodes are stored in DFS order and
/// reference each other by internal indices.
///
/// **This type is internal to provider implementations.** End users should
/// use [`Node`](crate::Node) (returned by `xa11y::app()`, etc.) which wraps
/// a `Tree` with navigation methods.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tree {
    /// Application name
    pub app_name: String,

    /// Process ID. `None` for multi-app queries.
    pub pid: Option<u32>,

    /// Screen dimensions at capture time (width, height)
    pub screen_size: (u32, u32),

    /// All nodes in DFS order (access through methods)
    nodes: Vec<NodeData>,
}

impl Tree {
    /// Create a new Tree from a list of nodes.
    pub fn new(
        app_name: String,
        pid: Option<u32>,
        screen_size: (u32, u32),
        nodes: Vec<NodeData>,
    ) -> Self {
        Self {
            app_name,
            pid,
            screen_size,
            nodes,
        }
    }

    /// Get a node's data by its internal index.
    pub fn get_data(&self, index: u32) -> Option<&NodeData> {
        self.nodes.get(index as usize)
    }

    /// Get the root node data.
    pub fn root_data(&self) -> &NodeData {
        &self.nodes[0]
    }

    /// Get the parent of a node.
    pub fn parent_data(&self, node: &NodeData) -> Option<&NodeData> {
        node.parent_index
            .and_then(|idx| self.nodes.get(idx as usize))
    }

    /// Get direct children of a node.
    pub fn children_data(&self, node: &NodeData) -> Vec<&NodeData> {
        node.children_indices
            .iter()
            .filter_map(|&idx| self.nodes.get(idx as usize))
            .collect()
    }

    /// Get indices of the subtree rooted at a node (including the node itself).
    pub fn subtree_indices(&self, index: u32) -> Vec<u32> {
        let mut result = Vec::new();
        self.collect_subtree_indices(index, &mut result);
        result
    }

    fn collect_subtree_indices(&self, index: NodeIndex, result: &mut Vec<u32>) {
        if let Some(node) = self.nodes.get(index as usize) {
            result.push(index);
            for &child_idx in &node.children_indices {
                self.collect_subtree_indices(child_idx, result);
            }
        }
    }

    /// Iterate all node data.
    pub fn iter(&self) -> impl Iterator<Item = &NodeData> {
        self.nodes.iter()
    }

    /// Query node indices matching a CSS-like selector string.
    pub fn query_indices(&self, selector_str: &str) -> Result<Vec<u32>> {
        let selector = Selector::parse(selector_str)?;
        Ok(selector.match_nodes(self).iter().map(|n| n.index).collect())
    }

    /// Query nodes matching a CSS-like selector string (returns NodeData refs).
    pub fn query(&self, selector_str: &str) -> Result<Vec<&NodeData>> {
        let selector = Selector::parse(selector_str)?;
        Ok(selector.match_nodes(self))
    }

    /// Render the tree as an indented text representation for debugging.
    pub fn dump(&self) -> String {
        // Compute depth from parent_index so NodeData doesn't need a depth field.
        let mut depths = vec![0u32; self.nodes.len()];
        for node in &self.nodes {
            let d = depths[node.index as usize];
            for &child_idx in &node.children_indices {
                if let Some(cd) = depths.get_mut(child_idx as usize) {
                    *cd = d + 1;
                }
            }
        }

        let mut output = String::new();
        for node in &self.nodes {
            let depth = depths.get(node.index as usize).copied().unwrap_or(0);
            let indent = "  ".repeat(depth as usize);
            let name_part = node
                .name
                .as_ref()
                .map(|n| format!(" \"{}\"", n))
                .unwrap_or_default();
            let value_part = node
                .value
                .as_ref()
                .map(|v| format!(" value=\"{}\"", v))
                .unwrap_or_default();
            output.push_str(&format!(
                "{}[{}] {}{}{}\n",
                indent,
                node.index,
                node.role.to_snake_case(),
                name_part,
                value_part,
            ));
        }
        output
    }

    /// Get the number of nodes in the tree.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Check if the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Get the internal node index for a node. Used by platform providers
    /// to look up cached element handles.
    #[doc(hidden)]
    pub fn node_index(&self, node: &NodeData) -> NodeIndex {
        node.index
    }
}
