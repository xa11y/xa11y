use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionData};
use crate::error::{Error, Result};
use crate::node::{Node, NodeIndex};
use crate::provider::{Provider, QueryOptions};
use crate::role::Role;
use crate::selector::Selector;

/// A snapshot of an application's accessibility tree.
///
/// The tree is a flattened snapshot — nodes are stored in DFS order and
/// reference each other by internal indices. Navigation is done through
/// `Tree` methods that accept `&Node` references.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tree {
    /// Application name
    pub app_name: String,

    /// Process ID. `None` for multi-app queries.
    pub pid: Option<u32>,

    /// Screen dimensions at capture time (width, height)
    pub screen_size: (u32, u32),

    /// All nodes in DFS order (access through methods)
    nodes: Vec<Node>,

    /// Query options used to produce this snapshot
    pub query: QueryOptions,
}

impl Tree {
    /// Create a new Tree from a list of nodes.
    pub fn new(
        app_name: String,
        pid: Option<u32>,
        screen_size: (u32, u32),
        nodes: Vec<Node>,
        query: QueryOptions,
    ) -> Self {
        Self {
            app_name,
            pid,
            screen_size,
            nodes,
            query,
        }
    }

    /// Rebuild internal state after deserialization.
    /// (No-op now that the HashMap is removed, but kept for API compatibility
    /// with FFI consumers that deserialize trees.)
    pub fn rebuild_index(&mut self) {
        // Node index == array position, no rebuild needed.
    }

    /// Get a node by its internal index. Primarily for FFI consumers.
    pub fn get(&self, index: u32) -> Option<&Node> {
        self.nodes.get(index as usize)
    }

    /// Get the root node.
    pub fn root(&self) -> &Node {
        &self.nodes[0]
    }

    /// Get the parent of a node.
    pub fn parent(&self, node: &Node) -> Option<&Node> {
        node.parent_index
            .and_then(|idx| self.nodes.get(idx as usize))
    }

    /// Get direct children of a node.
    pub fn children(&self, node: &Node) -> Vec<&Node> {
        node.children_indices
            .iter()
            .filter_map(|&idx| self.nodes.get(idx as usize))
            .collect()
    }

    /// Get the subtree rooted at a node (including the node itself).
    pub fn subtree(&self, node: &Node) -> Vec<&Node> {
        let mut result = Vec::new();
        self.collect_subtree(node.index, &mut result);
        result
    }

    fn collect_subtree<'a>(&'a self, index: NodeIndex, result: &mut Vec<&'a Node>) {
        if let Some(node) = self.nodes.get(index as usize) {
            result.push(node);
            for &child_idx in &node.children_indices {
                self.collect_subtree(child_idx, result);
            }
        }
    }

    /// Iterate all nodes.
    pub fn iter(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter()
    }

    /// Query nodes matching a CSS-like selector string.
    pub fn query(&self, selector_str: &str) -> Result<Vec<&Node>> {
        let selector = Selector::parse(selector_str)?;
        Ok(selector.match_nodes(self))
    }

    /// Find nodes by role.
    pub fn find_by_role(&self, role: Role) -> Vec<&Node> {
        self.nodes.iter().filter(|n| n.role == role).collect()
    }

    /// Find nodes by name (substring, case-insensitive).
    pub fn find_by_name(&self, pattern: &str) -> Vec<&Node> {
        let pattern_lower = pattern.to_lowercase();
        self.nodes
            .iter()
            .filter(|n| {
                n.name
                    .as_ref()
                    .is_some_and(|name| name.to_lowercase().contains(&pattern_lower))
            })
            .collect()
    }

    /// Perform an action on the first element matching a selector.
    pub fn perform(
        &self,
        provider: &dyn Provider,
        selector: &str,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        let results = self.query(selector)?;
        let node = results.first().ok_or_else(|| Error::SelectorNotMatched {
            selector: selector.to_string(),
        })?;
        provider.perform_action(self, node, action, data)
    }

    /// Render the tree as an indented text representation for debugging.
    pub fn dump(&self) -> String {
        // Compute depth from parent_index so Node doesn't need a depth field.
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
    pub fn node_index(&self, node: &Node) -> NodeIndex {
        node.index
    }
}
