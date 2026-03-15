use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::node::{Node, NodeId};
use crate::provider::QueryOptions;
use crate::role::Role;
use crate::selector::Selector;

/// A snapshot of an application's accessibility tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tree {
    /// Application name
    pub app_name: String,

    /// Process ID (0 for multi-app queries)
    pub pid: u32,

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
    /// Create a new Tree from nodes and metadata.
    pub fn new(
        app_name: String,
        pid: u32,
        screen_size: (u32, u32),
        nodes: Vec<Node>,
        query: QueryOptions,
    ) -> Self {
        let node_index = nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();
        Self {
            app_name,
            pid,
            screen_size,
            nodes,
            node_index,
            query,
        }
    }

    /// Rebuild the index after deserialization.
    pub fn rebuild_index(&mut self) {
        self.node_index = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id, i))
            .collect();
    }

    /// Get a node by ID.
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.node_index.get(&id).and_then(|&i| self.nodes.get(i))
    }

    /// Get the root node.
    pub fn root(&self) -> Option<&Node> {
        self.nodes.first()
    }

    /// Iterate all nodes.
    pub fn iter(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter()
    }

    /// Query nodes matching a CSS-like selector string.
    pub fn query(&self, selector_str: &str) -> Result<Vec<&Node>> {
        let selector = Selector::parse(selector_str)?;
        Ok(self.nodes.iter().filter(|n| selector.matches(n)).collect())
    }

    /// Get children of a node.
    pub fn children(&self, id: NodeId) -> Vec<&Node> {
        self.get(id)
            .map(|node| {
                node.children
                    .iter()
                    .filter_map(|&cid| self.get(cid))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get the subtree rooted at a node (including the node itself).
    pub fn subtree(&self, id: NodeId) -> Vec<&Node> {
        let mut result = Vec::new();
        let mut stack = vec![id];
        while let Some(current) = stack.pop() {
            if let Some(node) = self.get(current) {
                result.push(node);
                // Push children in reverse so we visit them in order
                for &child_id in node.children.iter().rev() {
                    stack.push(child_id);
                }
            }
        }
        result
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

    /// Returns the total number of nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns true if the tree has no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}
