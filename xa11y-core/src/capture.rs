//! Bounded capture of an accessibility subtree (#482).
//!
//! [`Element::capture`](crate::Element::capture) (and the `App`, `Locator`
//! and `ShellSurface` forms) reads a subtree into a flat, pre-order list of
//! [`CapturedNode`]s under explicit budgets, and says which budget cut it.
//! Every tree surface that has to stay bounded (the MCP `tree` tool, the CLI's
//! `xa11y tree`) is built on it.
//!
//! # Budgets
//!
//! [`CaptureOptions`] sets a depth limit, a node limit (default
//! [`DEFAULT_MAX_NODES`]), a total text budget for [`text`](CaptureOptions::text)
//! detail, and an optional time budget. Each limit that drops something sets
//! its flag in [`Truncation`], and the node it cut at says so
//! ([`CapturedNode::children_truncated`]). Running out of time returns what
//! was read, with `truncated.by_time`, rather than failing.
//!
//! # Order
//!
//! Read breadth-first, returned pre-order. When the node limit cuts, the top
//! of the tree is complete rather than one deep branch.
//!
//! # Consistency
//!
//! A capture is not an atomic snapshot on any platform: each node is read
//! once, at some instant between [`Capture::started`] and
//! [`Capture::finished`]. A node that disappears while its children are read
//! is dropped and counted in [`Capture::vanished`]. A read that fails for
//! another reason is recorded on the node ([`CapturedNode::errors`]) and the
//! walk continues. A cycle (a node that is its own ancestor, which broken web
//! trees have) is detected by [`NodeIdentity`] where the platform reports
//! one, and cut with an error on its parent. A node listed under two parents
//! is not a cycle and is captured under each.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime};

use crate::element::Element;
use crate::error::{Error, Result};
use crate::identity::NodeIdentity;
use crate::semantics::RelationKind;

/// The node limit every capture surface uses unless told otherwise.
pub const DEFAULT_MAX_NODES: usize = 2000;

/// The total text budget of a capture with text detail: 64 KiB.
pub const DEFAULT_MAX_TEXT_BYTES: usize = 64 * 1024;

/// What a capture reads and how far. Build with [`CaptureOptions::new`] and
/// the chained setters.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CaptureOptions {
    /// Deepest level read; the root is depth 0. `None` is no depth limit.
    pub max_depth: Option<usize>,
    /// Most nodes in the capture, the root included. At least 1.
    pub max_nodes: usize,
    /// Most bytes of text, in total, that [`text`](Self::text) detail reads.
    pub max_text_bytes: usize,
    /// Read the full text of each element with a text interface, within
    /// `max_text_bytes`.
    pub text: bool,
    /// Read each node's relations, resolved to nodes of the capture where
    /// the target is inside it.
    pub relations: bool,
    /// Stop reading after this long and return what was read.
    pub timeout: Option<Duration>,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            max_depth: None,
            max_nodes: DEFAULT_MAX_NODES,
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
            text: false,
            relations: false,
            timeout: None,
        }
    }
}

impl CaptureOptions {
    /// The defaults: no depth limit, [`DEFAULT_MAX_NODES`] nodes, structure
    /// only, no time budget.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the deepest level read (the root is 0).
    pub fn max_depth(mut self, depth: usize) -> Self {
        self.max_depth = Some(depth);
        self
    }

    /// Set the node limit, the root included.
    pub fn max_nodes(mut self, nodes: usize) -> Self {
        self.max_nodes = nodes;
        self
    }

    /// Set the total text budget, in bytes.
    pub fn max_text_bytes(mut self, bytes: usize) -> Self {
        self.max_text_bytes = bytes;
        self
    }

    /// Read element text.
    pub fn text(mut self, on: bool) -> Self {
        self.text = on;
        self
    }

    /// Read relations.
    pub fn relations(mut self, on: bool) -> Self {
        self.relations = on;
        self
    }

    /// Set the time budget.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    fn validate(&self) -> Result<()> {
        if self.max_nodes == 0 {
            return Err(Error::InvalidActionData {
                message: "max_nodes must be at least 1: the root is a node".into(),
            });
        }
        Ok(())
    }
}

/// Which limits dropped something. A limit that was reached without dropping
/// anything (a tree of exactly `max_nodes` nodes) is not reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Truncation {
    /// Children below `max_depth` were left out.
    pub by_depth: bool,
    /// Nodes past `max_nodes` were left out.
    pub by_nodes: bool,
    /// Text past `max_text_bytes` was left out.
    pub by_text: bool,
    /// The time budget ran out before the walk finished.
    pub by_time: bool,
}

impl Truncation {
    /// Whether any limit dropped something.
    pub fn any(&self) -> bool {
        self.by_depth || self.by_nodes || self.by_text || self.by_time
    }
}

/// Which read of a node failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CaptureStep {
    /// Reading the node's children. Its subtree is missing.
    Children,
    /// Reading the node's platform identity, which cycle detection and
    /// relation resolution use.
    Identity,
    /// Reading the node's text.
    Text,
    /// Reading the node's relations.
    Relations,
    /// A child was a node already in the capture (a cycle in the platform
    /// tree), and was left out.
    Cycle,
}

impl CaptureStep {
    /// `"children"`, `"identity"`, `"text"`, `"relations"` or `"cycle"`.
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Self::Children => "children",
            Self::Identity => "identity",
            Self::Text => "text",
            Self::Relations => "relations",
            Self::Cycle => "cycle",
        }
    }
}

impl std::fmt::Display for CaptureStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_snake_case())
    }
}

/// A read that failed on one node. The walk continued past it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CaptureError {
    /// What was being read.
    pub step: CaptureStep,
    /// Why it failed.
    pub error: Error,
}

/// An element's text, as far as the capture's text budget reached.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CapturedText {
    /// The text read, from offset 0.
    pub content: String,
    /// The element's full text length, in UTF-16 code units.
    pub length: u32,
    /// Whether `content` stops short of `length`.
    pub truncated: bool,
}

/// The target of a captured relation.
#[allow(
    clippy::exhaustive_enums,
    reason = "Closed domain: a target is either inside the capture or outside it."
)]
#[derive(Debug, Clone)]
pub enum RelationTarget {
    /// The node at this index of [`Capture::nodes`].
    Node(usize),
    /// An element the capture did not reach. Boxed: an `Element` is large
    /// and most targets are inside.
    Outside(Box<Element>),
}

/// One node of a [`Capture`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CapturedNode {
    /// Index in [`Capture::nodes`], which is pre-order. 0-based.
    pub index: usize,
    /// The parent's index; `None` on the root.
    pub parent: Option<usize>,
    /// Depth below the root, which is 0.
    pub depth: usize,
    /// The element, which names exactly this node: actions on it reach it
    /// or fail with [`Error::ElementStale`]. Derefs to its `ElementData`.
    pub element: Element,
    /// Indices of the children in the capture, in platform order.
    pub children: Vec<usize>,
    /// How many children the node has, including ones the capture left
    /// out. `None` when they were not read (a failed read, or the time
    /// budget ran out first).
    pub children_count: Option<usize>,
    /// Whether children of this node are missing from the capture: cut by a
    /// limit, or not read at all.
    pub children_truncated: bool,
    /// Reads of this node that failed.
    pub errors: Vec<CaptureError>,
    /// The node's text, with text detail, when it has a text interface.
    pub text: Option<CapturedText>,
    /// With relations detail: each kind the platform reports, in
    /// [`RelationKind::ALL`] order, with its targets (empty when none).
    pub relations: Vec<(RelationKind, Vec<RelationTarget>)>,
}

impl std::ops::Deref for CapturedNode {
    type Target = Element;
    fn deref(&self) -> &Element {
        &self.element
    }
}

/// A bounded read of a subtree. See the [module docs](self).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Capture {
    /// Every node read, pre-order. The root is index 0.
    pub nodes: Vec<CapturedNode>,
    /// Which limits dropped something.
    pub truncated: Truncation,
    /// Nodes that disappeared while the capture read them, and were dropped.
    pub vanished: usize,
    /// When the walk began.
    pub started: SystemTime,
    /// When the walk ended.
    pub finished: SystemTime,
    /// The options it ran with.
    pub options: CaptureOptions,
}

impl Capture {
    /// The root node.
    pub fn root(&self) -> &CapturedNode {
        &self.nodes[0]
    }

    /// The indices of `index`'s subtree, itself first, in pre-order. Empty
    /// when `index` is out of range.
    pub fn subtree(&self, index: usize) -> Vec<usize> {
        let Some(node) = self.nodes.get(index) else {
            return Vec::new();
        };
        // Pre-order: a subtree is the run of nodes after it that are deeper.
        let mut out = vec![index];
        out.extend(
            self.nodes[index + 1..]
                .iter()
                .take_while(|n| n.depth > node.depth)
                .map(|n| n.index),
        );
        out
    }
}

/// A node while the walk runs, in read (breadth-first) order.
struct Raw {
    element: Element,
    parent: Option<usize>,
    depth: usize,
    children: Vec<usize>,
    children_count: Option<usize>,
    children_truncated: bool,
    errors: Vec<CaptureError>,
    identity: Option<NodeIdentity>,
    vanished: bool,
    text: Option<CapturedText>,
    relations: Vec<(RelationKind, Vec<Element>)>,
}

impl Raw {
    fn new(element: Element, parent: Option<usize>, depth: usize) -> Self {
        Self {
            element,
            parent,
            depth,
            children: Vec::new(),
            children_count: None,
            children_truncated: false,
            errors: Vec::new(),
            identity: None,
            vanished: false,
            text: None,
            relations: Vec::new(),
        }
    }

    fn fail(&mut self, step: CaptureStep, error: Error) {
        self.errors.push(CaptureError { step, error });
    }
}

/// Capture the subtree under `root`, whose children `root_children` reads
/// (an `App` root's children are its windows across every registration, not
/// its element's children).
pub(crate) fn capture_from(
    root: Element,
    root_children: impl FnOnce() -> Result<Vec<Element>>,
    options: &CaptureOptions,
) -> Result<Capture> {
    options.validate()?;
    let started = SystemTime::now();
    let deadline = options.timeout.map(|t| Instant::now() + t);
    let out_of_time = || deadline.is_some_and(|d| Instant::now() >= d);
    let mut truncated = Truncation::default();
    let mut vanished = 0usize;

    let mut raw = vec![Raw::new(root, None, 0)];
    // Cycle detection needs identities; a platform without them (the IA2
    // source) walks without it.
    let mut identities = true;
    let root = raw.pop().expect("the root was just pushed");
    let (_, root) = note_identity(&raw, root, &mut identities);
    raw.push(root);

    let mut root_children = Some(root_children);
    let mut queue = VecDeque::from([0usize]);
    while let Some(i) = queue.pop_front() {
        if out_of_time() {
            truncated.by_time = true;
            for j in std::iter::once(i).chain(queue.drain(..)) {
                raw[j].children_truncated = true;
            }
            break;
        }
        let read = match root_children.take() {
            Some(read_root) => read_root(),
            None => raw[i].element.children(),
        };
        let kids = match read {
            Ok(kids) => kids,
            // The node went away between being listed and being read.
            Err(Error::ElementStale { .. }) if i != 0 => {
                raw[i].vanished = true;
                vanished += 1;
                continue;
            }
            Err(e) => {
                raw[i].fail(CaptureStep::Children, e);
                raw[i].children_truncated = true;
                continue;
            }
        };
        raw[i].children_count = Some(kids.len());
        if kids.is_empty() {
            continue;
        }
        if options.max_depth.is_some_and(|max| raw[i].depth >= max) {
            raw[i].children_truncated = true;
            truncated.by_depth = true;
            continue;
        }
        let depth = raw[i].depth + 1;
        for kid in kids {
            if raw.len() >= options.max_nodes {
                raw[i].children_truncated = true;
                truncated.by_nodes = true;
                break;
            }
            let (fresh, node) = note_identity(&raw, Raw::new(kid, Some(i), depth), &mut identities);
            if !fresh {
                let subject = crate::error::stale_subject(&node.element);
                raw[i].fail(
                    CaptureStep::Cycle,
                    Error::Platform {
                        code: 0,
                        message: format!(
                            "child {subject} is one of its own ancestors; the platform \
                             tree has a cycle here, so it was left out"
                        ),
                    },
                );
                raw[i].children_truncated = true;
                continue;
            }
            let j = raw.len();
            raw.push(node);
            raw[i].children.push(j);
            queue.push_back(j);
        }
    }

    if options.text {
        read_texts(
            &mut raw,
            options.max_text_bytes,
            &mut truncated,
            &out_of_time,
        );
    }
    if options.relations {
        read_relations(&mut raw, &mut truncated, &out_of_time);
    }

    let nodes = into_pre_order(raw);
    Ok(Capture {
        nodes,
        truncated,
        vanished,
        started,
        finished: SystemTime::now(),
        options: options.clone(),
    })
}

/// Record `node`'s identity, and whether it is new on its path: `false`
/// when it is one of its own ancestors (a cycle). A node reachable under two
/// parents is not a cycle, and is captured under each.
fn note_identity(raw: &[Raw], mut node: Raw, identities: &mut bool) -> (bool, Raw) {
    if !*identities {
        return (true, node);
    }
    match node.element.identity() {
        Ok(id) => {
            let mut ancestor = node.parent;
            while let Some(a) = ancestor {
                if raw[a].identity.as_ref() == Some(&id) {
                    return (false, node);
                }
                ancestor = raw[a].parent;
            }
            node.identity = Some(id);
        }
        // This platform has no identities: no cycle detection, and
        // relations resolve to elements outside the capture.
        Err(Error::Unsupported { .. }) => *identities = false,
        Err(e) => node.fail(CaptureStep::Identity, e),
    }
    (true, node)
}

fn read_texts(
    raw: &mut [Raw],
    max_bytes: usize,
    truncated: &mut Truncation,
    out_of_time: &impl Fn() -> bool,
) {
    let mut left = max_bytes;
    for node in raw.iter_mut().filter(|n| !n.vanished) {
        if out_of_time() {
            truncated.by_time = true;
            return;
        }
        let text = match node.element.text() {
            Ok(text) => text,
            // No text interface: nothing to read, not a failure.
            Err(Error::ActionNotSupported { .. }) => continue,
            Err(e) => {
                node.fail(CaptureStep::Text, e);
                continue;
            }
        };
        match read_text(&text, left) {
            Ok(captured) => {
                left -= captured.content.len();
                if captured.truncated {
                    truncated.by_text = true;
                }
                node.text = Some(captured);
            }
            Err(e) => node.fail(CaptureStep::Text, e),
        }
    }
}

/// Read `text` from offset 0, in at most `max_bytes` bytes of UTF-8.
fn read_text(text: &crate::text_api::Text, max_bytes: usize) -> Result<CapturedText> {
    let length = text.length()?;
    if length == 0 {
        return Ok(CapturedText {
            content: String::new(),
            length,
            truncated: false,
        });
    }
    // A UTF-16 unit is at least one byte of UTF-8, so this many units can
    // only overshoot the budget, and the cut below brings it back.
    let units = length.min(u32::try_from(max_bytes).unwrap_or(u32::MAX));
    let mut content = if units == 0 {
        String::new()
    } else {
        text.read(crate::text_api::TextRange::new(0, units)?)?
    };
    let mut cut = content.len().min(max_bytes);
    while !content.is_char_boundary(cut) {
        cut -= 1;
    }
    content.truncate(cut);
    let read_units = content.encode_utf16().count();
    Ok(CapturedText {
        truncated: (read_units as u64) < u64::from(length),
        content,
        length,
    })
}

fn read_relations(raw: &mut [Raw], truncated: &mut Truncation, out_of_time: &impl Fn() -> bool) {
    for node in raw.iter_mut().filter(|n| !n.vanished) {
        if out_of_time() {
            truncated.by_time = true;
            return;
        }
        match node.element.relations(&[]) {
            Ok(found) => node.relations = found,
            Err(e) => node.fail(CaptureStep::Relations, e),
        }
    }
}

/// Drop vanished nodes and lay the rest out pre-order, renumbering every
/// index, and resolve relation targets to nodes where they are inside.
fn into_pre_order(raw: Vec<Raw>) -> Vec<CapturedNode> {
    // Pre-order walk over the read (BFS) indices.
    let mut order = Vec::with_capacity(raw.len());
    let mut stack = vec![0usize];
    while let Some(i) = stack.pop() {
        if raw[i].vanished {
            continue;
        }
        order.push(i);
        stack.extend(raw[i].children.iter().rev().copied());
    }
    let new_index: HashMap<usize, usize> = order
        .iter()
        .enumerate()
        .map(|(new, &old)| (old, new))
        .collect();
    // A node captured under two parents resolves to its first (pre-order)
    // appearance.
    let mut by_identity: HashMap<NodeIdentity, usize> = HashMap::new();
    for &old in &order {
        if let Some(id) = raw[old].identity.clone() {
            by_identity.entry(id).or_insert(new_index[&old]);
        }
    }
    let resolve = |target: Element| -> RelationTarget {
        match target.identity() {
            Ok(id) => match by_identity.get(&id) {
                Some(&index) => RelationTarget::Node(index),
                None => RelationTarget::Outside(Box::new(target)),
            },
            // Without an identity the target cannot be matched to a node;
            // it is still a correct element, only not cross-referenced.
            Err(_) => RelationTarget::Outside(Box::new(target)),
        }
    };

    let mut slots: Vec<Option<Raw>> = raw.into_iter().map(Some).collect();
    let mut nodes = Vec::with_capacity(order.len());
    for (index, &old) in order.iter().enumerate() {
        let Some(node) = slots[old].take() else {
            continue;
        };
        let children: Vec<usize> = node
            .children
            .iter()
            .filter_map(|c| new_index.get(c).copied())
            .collect();
        let relations = node
            .relations
            .into_iter()
            .map(|(kind, targets)| (kind, targets.into_iter().map(&resolve).collect()))
            .collect();
        nodes.push(CapturedNode {
            index,
            parent: node.parent.and_then(|p| new_index.get(&p).copied()),
            depth: node.depth,
            element: node.element,
            children,
            children_count: node.children_count,
            children_truncated: node.children_truncated,
            errors: node.errors,
            text: node.text,
            relations,
        });
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{build_document_provider, build_provider, MockProvider};
    use crate::{App, Provider, Role};
    use std::sync::Arc;

    fn app(provider: Arc<MockProvider>) -> App {
        let provider: Arc<dyn Provider> = provider;
        App::list_with(provider)
            .expect("the mock lists its app")
            .into_iter()
            .next()
            .expect("one app")
    }

    fn names(cap: &Capture) -> Vec<String> {
        cap.nodes
            .iter()
            .map(|n| n.name.clone().unwrap_or_default())
            .collect()
    }

    #[test]
    fn an_unbounded_capture_is_the_whole_tree_in_pre_order() {
        let cap = app(build_provider())
            .capture(&CaptureOptions::new())
            .expect("capture");
        assert!(!cap.truncated.any(), "{:?}", cap.truncated);
        assert_eq!(cap.vanished, 0);
        assert_eq!(cap.root().role, Role::Application);
        for node in &cap.nodes {
            assert!(node.errors.is_empty(), "{:?}", node.errors);
            assert!(!node.children_truncated);
            assert_eq!(node.children_count, Some(node.children.len()));
            for &child in &node.children {
                assert_eq!(cap.nodes[child].parent, Some(node.index));
                assert_eq!(cap.nodes[child].depth, node.depth + 1);
                assert!(child > node.index, "pre-order puts children after parents");
            }
        }
        // Pre-order: Back and Forward (in Navigation) come before Content's children.
        let order = names(&cap);
        let pos = |n: &str| order.iter().position(|x| x == n).expect(n);
        assert!(pos("Navigation") < pos("Back"));
        assert!(pos("Back") < pos("Forward"));
        assert!(pos("Forward") < pos("Content"));
        // A subtree is the node and its descendants.
        let nav = pos("Navigation");
        let sub: Vec<String> = cap.subtree(nav).iter().map(|&i| order[i].clone()).collect();
        assert_eq!(sub, ["Navigation", "Back", "Forward"]);
    }

    #[test]
    fn the_node_limit_keeps_the_top_of_the_tree_and_says_where_it_cut() {
        let full = app(build_provider())
            .capture(&CaptureOptions::new())
            .expect("capture");
        let cap = app(build_provider())
            .capture(&CaptureOptions::new().max_nodes(4))
            .expect("capture");
        assert_eq!(cap.nodes.len(), 4);
        assert!(cap.truncated.by_nodes);
        assert!(!cap.truncated.by_depth);
        // Breadth-first: every node of depth <= 1 is present before any deeper.
        let max_depth = cap.nodes.iter().map(|n| n.depth).max().unwrap_or(0);
        let shallower_in_full = full.nodes.iter().filter(|n| n.depth < max_depth).count();
        let shallower_here = cap.nodes.iter().filter(|n| n.depth < max_depth).count();
        assert_eq!(shallower_here, shallower_in_full);
        // The cut nodes say so and keep their real child count.
        let cut: Vec<&CapturedNode> = cap.nodes.iter().filter(|n| n.children_truncated).collect();
        assert!(!cut.is_empty());
        for node in cut {
            assert!(node.children_count.expect("counted") > node.children.len());
        }
    }

    #[test]
    fn a_limit_that_drops_nothing_is_not_reported() {
        let full = app(build_provider())
            .capture(&CaptureOptions::new())
            .expect("capture");
        let exact = app(build_provider())
            .capture(&CaptureOptions::new().max_nodes(full.nodes.len()))
            .expect("capture");
        assert!(!exact.truncated.by_nodes);
        assert_eq!(exact.nodes.len(), full.nodes.len());
    }

    #[test]
    fn the_depth_limit_cuts_with_the_child_count() {
        let cap = app(build_provider())
            .capture(&CaptureOptions::new().max_depth(1))
            .expect("capture");
        assert!(cap.truncated.by_depth);
        assert!(cap.nodes.iter().all(|n| n.depth <= 1));
        let window = cap.nodes.iter().find(|n| n.depth == 1).expect("a window");
        assert!(window.children.is_empty());
        assert!(window.children_truncated);
        assert!(window.children_count.expect("counted") > 0);
    }

    #[test]
    fn zero_nodes_is_refused() {
        let err = app(build_provider())
            .capture(&CaptureOptions::new().max_nodes(0))
            .expect_err("refused");
        assert!(matches!(err, Error::InvalidActionData { .. }), "{err}");
    }

    #[test]
    fn a_vanished_node_is_dropped_and_counted_and_a_failed_read_is_recorded() {
        let provider = build_provider();
        provider.fail_children(
            "Navigation",
            Error::stale(crate::StaleReason::Removed, "toolbar"),
        );
        provider.fail_children(
            "Content",
            Error::Platform {
                code: 7,
                message: "broken subtree".into(),
            },
        );
        let cap = app(provider)
            .capture(&CaptureOptions::new())
            .expect("capture");
        assert_eq!(cap.vanished, 1);
        let order = names(&cap);
        assert!(!order.contains(&"Navigation".to_string()), "{order:?}");
        assert!(!order.contains(&"Back".to_string()));
        let content = cap
            .nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("Content"))
            .expect("Content stays");
        assert_eq!(content.errors.len(), 1);
        assert_eq!(content.errors[0].step, CaptureStep::Children);
        assert!(content.children_truncated);
        assert_eq!(content.children_count, None);
        // Indices stay consistent after the drop.
        for node in &cap.nodes {
            for &child in &node.children {
                assert_eq!(cap.nodes[child].parent, Some(node.index));
            }
        }
    }

    #[test]
    fn a_shared_node_is_captured_under_each_parent_and_a_cycle_is_cut() {
        // Shared: Back listed under Content too. Not a cut.
        let shared = build_provider();
        shared.link_child("Content", "Back");
        let cap = app(shared)
            .capture(&CaptureOptions::new())
            .expect("capture");
        assert!(!cap.truncated.any(), "{:?}", cap.truncated);
        assert_eq!(names(&cap).iter().filter(|n| *n == "Back").count(), 2);
        assert!(cap.nodes.iter().all(|n| !n.children_truncated));

        // Cycle: Navigation listed under its own child Back.
        let cyclic = build_provider();
        cyclic.link_child("Back", "Navigation");
        let cap = app(cyclic)
            .capture(&CaptureOptions::new())
            .expect("capture");
        let back = cap
            .nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("Back"))
            .expect("Back");
        assert_eq!(back.errors.len(), 1, "{:?}", back.errors);
        assert_eq!(back.errors[0].step, CaptureStep::Cycle);
        assert!(back.children_truncated);
        assert_eq!(names(&cap).iter().filter(|n| *n == "Navigation").count(), 1);
    }

    #[test]
    fn an_exhausted_time_budget_returns_what_was_read() {
        let cap = app(build_provider())
            .capture(&CaptureOptions::new().timeout(Duration::ZERO))
            .expect("a partial capture, not a timeout");
        assert!(cap.truncated.by_time);
        assert_eq!(cap.nodes.len(), 1, "only the root was listed");
        assert!(cap.root().children_truncated);
        assert_eq!(cap.root().children_count, None);
    }

    #[test]
    fn relations_resolve_to_nodes_inside_the_capture() {
        let cap = app(build_document_provider())
            .capture(&CaptureOptions::new().relations(true))
            .expect("capture");
        let field = cap
            .nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("Email"))
            .expect("the field");
        let (_, labels) = field
            .relations
            .iter()
            .find(|(k, _)| *k == RelationKind::LabelledBy)
            .expect("labelled_by is reported");
        match labels.as_slice() {
            [RelationTarget::Node(i)] => {
                assert_eq!(cap.nodes[*i].name.as_deref(), Some("Email address"))
            }
            other => panic!("expected one in-capture label, got {other:?}"),
        }
        // Every other kind this platform reports is listed, empty.
        let others: Vec<_> = field
            .relations
            .iter()
            .filter(|(k, _)| !matches!(k, RelationKind::LabelledBy | RelationKind::DescribedBy))
            .collect();
        assert!(!others.is_empty(), "the platform reports more kinds");
        assert!(others.iter().all(|(_, t)| t.is_empty()), "{others:?}");
        // Outside the capture: a capture rooted at the field itself.
        let alone = field
            .element
            .capture(&CaptureOptions::new().relations(true))
            .expect("capture");
        let (_, labels) = alone
            .root()
            .relations
            .iter()
            .find(|(k, _)| *k == RelationKind::LabelledBy)
            .expect("labelled_by");
        match labels.as_slice() {
            [RelationTarget::Outside(label)] => {
                assert_eq!(label.name.as_deref(), Some("Email address"))
            }
            other => panic!("expected one outside label, got {other:?}"),
        }
    }

    #[test]
    fn text_detail_reads_within_the_budget() {
        let provider: Arc<dyn Provider> = crate::mock::build_table_provider();
        let app = App::list_with(provider)
            .expect("list")
            .into_iter()
            .next()
            .expect("one app");
        let notes = |cap: &Capture| {
            cap.nodes
                .iter()
                .find(|n| n.name.as_deref() == Some("Notes"))
                .and_then(|n| n.text.clone())
                .expect("Notes has text")
        };
        let cap = app
            .capture(&CaptureOptions::new().text(true))
            .expect("capture");
        let full = notes(&cap);
        assert_eq!(full.content, "Hello 😀 world\nsecond line");
        assert!(!full.truncated);
        assert_eq!(full.length, 26, "UTF-16 units: the emoji is two");
        assert!(!cap.truncated.by_text);
        // Without text detail nothing is read.
        let plain = app.capture(&CaptureOptions::new()).expect("capture");
        assert!(plain.nodes.iter().all(|n| n.text.is_none()));
        // A budget of 8 bytes stops before the emoji, on a char boundary.
        let cut = app
            .capture(&CaptureOptions::new().text(true).max_text_bytes(8))
            .expect("capture");
        let short = notes(&cut);
        assert!(short.truncated);
        assert!(cut.truncated.by_text);
        assert!(short.content.len() <= 8, "{:?}", short.content);
        assert!(short.content.starts_with("Hello"));
    }
}
