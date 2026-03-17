use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::node::{Node, NodeId};
use crate::provider::QueryOptions;
use crate::role::Role;
use crate::selector::Selector;

/// A snapshot of an application's accessibility tree.
///
/// The tree is a flattened snapshot — nodes reference each other by `NodeId`
/// rather than holding direct pointers. This is critical for:
/// - Serialization (JSON, msgpack) for FFI
/// - Deterministic re-traversal for action dispatch (same DFS order → same IDs)
/// - Thread safety without lifetimes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tree {
    /// Opaque snapshot identifier, assigned by the Provider.
    /// Used by `perform_action` to look up the correct handle cache.
    /// Not meaningful across Provider instances or serialization boundaries.
    #[serde(skip)]
    #[allow(dead_code)]
    pub(crate) tree_id: u64,

    /// Application name
    pub app_name: String,

    /// Process ID. `None` for multi-app queries.
    pub pid: Option<u32>,

    /// Screen dimensions at capture time (width, height)
    pub screen_size: (u32, u32),

    /// All nodes in DFS order
    pub nodes: Vec<Node>,

    /// Index from NodeId -> position in nodes vec (for O(1) lookup)
    #[serde(skip)]
    node_index: HashMap<NodeId, usize>,

    /// Query options used to produce this snapshot
    pub query: QueryOptions,
}

impl Tree {
    /// Create a new Tree from a list of nodes.
    pub fn new(
        tree_id: u64,
        app_name: String,
        pid: Option<u32>,
        screen_size: (u32, u32),
        nodes: Vec<Node>,
        query: QueryOptions,
    ) -> Self {
        let node_index = nodes
            .iter()
            .enumerate()
            .map(|(i, node)| (node.id, i))
            .collect();
        Self {
            tree_id,
            app_name,
            pid,
            screen_size,
            nodes,
            node_index,
            query,
        }
    }

    /// Rebuild the node index (needed after deserialization).
    pub fn rebuild_index(&mut self) {
        self.node_index = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, node)| (node.id, i))
            .collect();
    }

    /// Get a node by ID.
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.node_index
            .get(&id)
            .and_then(|&idx| self.nodes.get(idx))
    }

    /// Get the root node.
    pub fn root(&self) -> &Node {
        &self.nodes[0]
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

    /// Get direct children of a node.
    pub fn children(&self, id: NodeId) -> Vec<&Node> {
        self.get(id)
            .map(|node| {
                node.children
                    .iter()
                    .filter_map(|child_id| self.get(*child_id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get the subtree rooted at a node (including the node itself).
    pub fn subtree(&self, id: NodeId) -> Vec<&Node> {
        let mut result = Vec::new();
        self.collect_subtree(id, &mut result);
        result
    }

    fn collect_subtree<'a>(&'a self, id: NodeId, result: &mut Vec<&'a Node>) {
        if let Some(node) = self.get(id) {
            result.push(node);
            for child_id in &node.children {
                self.collect_subtree(*child_id, result);
            }
        }
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

    /// Render the tree as an indented text representation for debugging.
    pub fn dump(&self) -> String {
        let mut output = String::new();
        for node in &self.nodes {
            let indent = "  ".repeat(node.depth as usize);
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
                node.id,
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
}
