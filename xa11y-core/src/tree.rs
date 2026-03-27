use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::node::{NodeIndex, RawNode};
use crate::selector::Selector;

/// Internal snapshot of an application's accessibility tree.
///
/// Nodes are stored in DFS order and reference each other by internal indices.
/// This type is used internally by providers. The public API exposes [`Node`](crate::node::Node)
/// cursors instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tree {
    /// Application name
    pub app_name: String,

    /// Process ID. `None` for multi-app queries.
    pub pid: Option<u32>,

    /// Screen dimensions at capture time (width, height)
    pub screen_size: (u32, u32),

    /// All nodes in DFS order
    pub nodes: Vec<RawNode>,
}

impl Tree {
    /// Create a new Tree from a list of raw nodes.
    pub fn new(
        app_name: String,
        pid: Option<u32>,
        screen_size: (u32, u32),
        nodes: Vec<RawNode>,
    ) -> Self {
        Self {
            app_name,
            pid,
            screen_size,
            nodes,
        }
    }

    /// Get a node by its internal index.
    pub fn get(&self, index: u32) -> Option<&RawNode> {
        self.nodes.get(index as usize)
    }

    /// Get the root node.
    pub fn root(&self) -> &RawNode {
        &self.nodes[0]
    }

    /// Get the parent of a node.
    pub fn parent(&self, node: &RawNode) -> Option<&RawNode> {
        node.parent_index
            .and_then(|idx| self.nodes.get(idx as usize))
    }

    /// Get direct children of a node.
    pub fn children(&self, node: &RawNode) -> Vec<&RawNode> {
        node.children_indices
            .iter()
            .filter_map(|&idx| self.nodes.get(idx as usize))
            .collect()
    }

    /// Get the subtree rooted at a node (including the node itself).
    pub fn subtree(&self, node: &RawNode) -> Vec<&RawNode> {
        let mut result = Vec::new();
        self.collect_subtree(node.index, &mut result);
        result
    }

    fn collect_subtree<'a>(&'a self, index: NodeIndex, result: &mut Vec<&'a RawNode>) {
        if let Some(node) = self.nodes.get(index as usize) {
            result.push(node);
            for &child_idx in &node.children_indices {
                self.collect_subtree(child_idx, result);
            }
        }
    }

    /// Iterate all nodes.
    pub fn iter(&self) -> impl Iterator<Item = &RawNode> {
        self.nodes.iter()
    }

    /// Query nodes matching a CSS-like selector string.
    pub fn query(&self, selector_str: &str) -> Result<Vec<&RawNode>> {
        let selector = Selector::parse(selector_str)?;
        let indices = selector.match_raw_nodes(&self.nodes);
        Ok(indices
            .into_iter()
            .filter_map(|idx| self.nodes.get(idx as usize))
            .collect())
    }

    /// Render the tree as an indented text representation for debugging.
    pub fn dump(&self) -> String {
        // Compute depth from parent_index so RawNode doesn't need a depth field.
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
    pub fn node_index(&self, node: &RawNode) -> NodeIndex {
        node.index
    }
}
