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
//!
//! # Cancellation
//!
//! [`CaptureOptions::cancel`] takes a [`CancelToken`]; cancelling it from
//! another thread stops the walk at the next node, and the capture returns
//! what it read with `truncated.by_cancel`, like a time budget running out.
//!
//! # Provenance and cost
//!
//! [`Capture::backend`] names the source that read the tree (`"atspi"`,
//! `"ax"`, `"uia"`, ...), so two captures from different sources are never
//! mistaken for one another. [`Capture::stats`] counts the reads the walk
//! made, by kind, so the cost of a detail level can be measured rather than
//! assumed: a structure-only capture makes no text, formatting or relation
//! reads at all.
//!
//! Each backend already batches the reads behind one children call: UIA
//! fetches every child's properties in one `FindAllBuildCache` request, AX in
//! one `AXUIElementCopyMultipleAttributeValues` call per child, and AT-SPI
//! builds the children in parallel. A capture is therefore one children read
//! per node read, plus one read per node and detail requested.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::element::Element;
use crate::error::{Error, Result};
use crate::identity::NodeIdentity;
use crate::semantics::RelationKind;
use crate::text_api::{TextRange, TextRun};

/// The node limit every capture surface uses unless told otherwise.
pub const DEFAULT_MAX_NODES: usize = 2000;

/// The total text budget of a capture with text detail: 64 KiB.
pub const DEFAULT_MAX_TEXT_BYTES: usize = 64 * 1024;

/// The total formatting-run budget of a capture with formatting detail.
pub const DEFAULT_MAX_RUNS: usize = 4096;

/// Stops a capture from another thread. Clones share one flag.
///
/// ```
/// let token = xa11y_core::CancelToken::new();
/// let other = token.clone();
/// other.cancel();
/// assert!(token.is_cancelled());
/// ```
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel every capture holding this token (or a clone of it). A capture
    /// already finished is unaffected; one not yet started stops at once.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether [`cancel`](Self::cancel) was called.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl PartialEq for CancelToken {
    /// Tokens are equal when they share a flag.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CancelToken {}

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
    /// Read the formatting runs of the text each node's text detail read.
    /// Turns text detail on: runs are ranges in that text.
    pub formatting: bool,
    /// Most formatting runs, in total, that [`formatting`](Self::formatting)
    /// detail reads.
    pub max_runs: usize,
    /// Stop reading after this long and return what was read.
    pub timeout: Option<Duration>,
    /// Stop reading when this token is cancelled and return what was read.
    pub cancel: Option<CancelToken>,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            max_depth: None,
            max_nodes: DEFAULT_MAX_NODES,
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
            text: false,
            relations: false,
            formatting: false,
            max_runs: DEFAULT_MAX_RUNS,
            timeout: None,
            cancel: None,
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

    /// Read formatting runs (and so text).
    pub fn formatting(mut self, on: bool) -> Self {
        self.formatting = on;
        self
    }

    /// Set the total formatting-run budget.
    pub fn max_runs(mut self, runs: usize) -> Self {
        self.max_runs = runs;
        self
    }

    /// Set the time budget.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Stop when `token` is cancelled.
    pub fn cancel(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    fn validate(&self) -> Result<()> {
        if self.max_nodes == 0 {
            return Err(Error::invalid_action_data(
                "max_nodes must be at least 1: the root is a node",
            ));
        }
        Ok(())
    }
}

/// Which limits dropped something. A depth limit that dropped nothing is not
/// reported. The node limit is reported once the capture is full while nodes
/// whose children were never read remain: those children are not read just
/// to learn whether there are any, so a tree of exactly `max_nodes` nodes
/// can report it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Truncation {
    /// Children below `max_depth` were left out.
    pub by_depth: bool,
    /// The capture filled `max_nodes` with children left out or unread.
    pub by_nodes: bool,
    /// Text past `max_text_bytes` was left out.
    pub by_text: bool,
    /// The time budget ran out before the walk finished.
    pub by_time: bool,
    /// Formatting runs past `max_runs` were left out.
    pub by_formatting: bool,
    /// The capture's [`CancelToken`] was cancelled before the walk finished.
    pub by_cancel: bool,
}

impl Truncation {
    /// Whether any limit dropped something.
    pub fn any(&self) -> bool {
        self.by_depth
            || self.by_nodes
            || self.by_text
            || self.by_time
            || self.by_formatting
            || self.by_cancel
    }
}

/// The reads a capture made, by kind. A read is one call into the backend
/// for one node; how many platform round trips it costs depends on the
/// backend (see the [module docs](self)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CaptureStats {
    /// Children reads: one per node whose children were read.
    pub children_reads: usize,
    /// Identity reads, for cycle detection and relation resolution.
    pub identity_reads: usize,
    /// Text reads (the length, then the content), with text detail.
    pub text_reads: usize,
    /// Formatting-run reads, with formatting detail: one per run.
    pub formatting_reads: usize,
    /// Relation reads, with relations detail: one per node.
    pub relation_reads: usize,
    /// Bytes of text in the capture, as UTF-8.
    pub text_bytes: usize,
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
    /// Reading the formatting runs of the node's text.
    Formatting,
    /// A child was a node already in the capture (a cycle in the platform
    /// tree), and was left out.
    Cycle,
}

impl CaptureStep {
    /// `"children"`, `"identity"`, `"text"`, `"relations"`, `"formatting"`
    /// or `"cycle"`.
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Self::Children => "children",
            Self::Identity => "identity",
            Self::Text => "text",
            Self::Relations => "relations",
            Self::Formatting => "formatting",
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
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CapturedText {
    /// The text read, from offset 0.
    pub content: String,
    /// The element's full text length, in UTF-16 code units.
    pub length: u32,
    /// Whether `content` stops short of `length`.
    pub truncated: bool,
    /// With formatting detail, the formatting runs covering `content`, in
    /// order, each clipped to it. `None` when they were not read (no
    /// formatting detail, a failed read, or a budget ran out first).
    pub runs: Option<Vec<TextRun>>,
    /// Whether `runs` stops short of the end of `content`: the run budget
    /// ran out.
    pub runs_truncated: bool,
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
    /// out. `None` when they were not read (a failed read, or the time or
    /// node budget ran out first).
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
    /// The source that read the tree, from the root's
    /// [`NodeIdentity::backend`]: `"atspi"`, `"ax"`, `"uia"`, `"ia2"`.
    /// `None` when the root reported no identity.
    pub backend: Option<&'static str>,
    /// The reads the walk made.
    pub stats: CaptureStats,
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

/// Why a walk stopped early.
#[derive(Clone, Copy)]
enum Stop {
    Time,
    Cancel,
}

impl Stop {
    fn mark(self, truncated: &mut Truncation) {
        match self {
            Stop::Time => truncated.by_time = true,
            Stop::Cancel => truncated.by_cancel = true,
        }
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
    // The time budget bounds the reads too (issue #531): one cut short by it
    // is recorded as that node's failure, with the read named as pending,
    // and the walk then stops on time like any other.
    match options.timeout {
        Some(t) => crate::deadline::within(t, || walk(root, root_children, options)),
        None => walk(root, root_children, options),
    }
}

/// The body of [`capture_from`], run inside the capture's deadline.
fn walk(
    root: Element,
    root_children: impl FnOnce() -> Result<Vec<Element>>,
    options: &CaptureOptions,
) -> Result<Capture> {
    let started = SystemTime::now();
    // A timeout too large for an `Instant` is no deadline, as in
    // `Deadline::after`.
    let deadline = options.timeout.and_then(|t| Instant::now().checked_add(t));
    let cancel = options.cancel.clone();
    let stop = move || -> Option<Stop> {
        if cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
            Some(Stop::Cancel)
        } else if deadline.is_some_and(|d| Instant::now() >= d) {
            Some(Stop::Time)
        } else {
            None
        }
    };
    let mut truncated = Truncation::default();
    let mut stats = CaptureStats::default();
    let mut vanished = 0usize;

    // Cycle detection needs identities; a custom provider without them
    // walks without it.
    let mut identities = true;
    let (_, root) = note_identity(&[], Raw::new(root, None, 0), &mut identities, &mut stats);
    let backend = root.identity.as_ref().map(NodeIdentity::backend);
    let mut raw = vec![root];

    let mut root_children = Some(root_children);
    let mut queue = VecDeque::from([0usize]);
    while let Some(i) = queue.pop_front() {
        if let Some(why) = stop() {
            why.mark(&mut truncated);
            for j in std::iter::once(i).chain(queue.drain(..)) {
                raw[j].children_truncated = true;
            }
            break;
        }
        // A full capture reads no more children: a read lists every child a
        // node has, so reading past the budget would cost the platform calls
        // the budget exists to bound (a 100k-row table under the last node
        // the capture kept). The unread nodes say so, as the time budget's do.
        if raw.len() >= options.max_nodes {
            truncated.by_nodes = true;
            for j in std::iter::once(i).chain(queue.drain(..)) {
                raw[j].children_truncated = true;
            }
            break;
        }
        stats.children_reads += 1;
        let read = match root_children.take() {
            Some(read_root) => read_root(),
            None => raw[i].element.children(),
        };
        // A read the time budget cut short is this node's failure, even
        // when the backend returned what it had (issue #531).
        let read = crate::deadline::distrust(read);
        let kids = match read {
            Ok(kids) => kids,
            // The node went away between being listed and being read. An
            // eviction is not that: the node may still exist, and the walk's
            // own reads pushed its handle out, so it is a failed read.
            Err(Error::ElementStale { reason, .. })
                if i != 0 && reason != crate::error::StaleReason::Evicted =>
            {
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
            let (fresh, node) = note_identity(
                &raw,
                Raw::new(kid, Some(i), depth),
                &mut identities,
                &mut stats,
            );
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

    if options.text || options.formatting {
        let budget = TextBudget {
            bytes: options.max_text_bytes,
            runs: options.formatting.then_some(options.max_runs),
        };
        read_texts(&mut raw, budget, &mut truncated, &mut stats, &stop);
    }
    if options.relations {
        read_relations(&mut raw, &mut truncated, &mut stats, &stop);
    }

    let nodes = into_pre_order(raw);
    Ok(Capture {
        nodes,
        truncated,
        vanished,
        started,
        finished: SystemTime::now(),
        options: options.clone(),
        backend,
        stats,
    })
}

/// Record `node`'s identity, and whether it is new on its path: `false`
/// when it is one of its own ancestors (a cycle). A node reachable under two
/// parents is not a cycle, and is captured under each.
fn note_identity(
    raw: &[Raw],
    mut node: Raw,
    identities: &mut bool,
    stats: &mut CaptureStats,
) -> (bool, Raw) {
    if !*identities {
        return (true, node);
    }
    stats.identity_reads += 1;
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

/// What text detail may still read: bytes of text, and formatting runs
/// when formatting detail is on.
#[derive(Clone, Copy)]
struct TextBudget {
    bytes: usize,
    runs: Option<usize>,
}

fn read_texts(
    raw: &mut [Raw],
    mut left: TextBudget,
    truncated: &mut Truncation,
    stats: &mut CaptureStats,
    stop: &impl Fn() -> Option<Stop>,
) {
    for node in raw.iter_mut().filter(|n| !n.vanished) {
        if let Some(why) = stop() {
            why.mark(truncated);
            return;
        }
        // The length read proves the interface exists and is the length
        // `read_text` needs: one read, which on GTK fetches the whole text.
        let length = match node.element.provider().text_length(node.element.data()) {
            Ok(length) => length,
            // No text interface: nothing to read, not a failure.
            Err(Error::ActionNotSupported { .. }) => continue,
            Err(e) => {
                node.fail(CaptureStep::Text, e);
                continue;
            }
        };
        let text = crate::text_api::Text::for_fresh_element(node.element.clone());
        stats.text_reads += 1;
        let mut captured = match read_text(&text, length, left.bytes) {
            Ok(captured) => captured,
            Err(e) => {
                node.fail(CaptureStep::Text, e);
                continue;
            }
        };
        left.bytes -= captured.content.len();
        stats.text_bytes += captured.content.len();
        if captured.truncated {
            truncated.by_text = true;
        }
        if let Some(runs_left) = left.runs.as_mut() {
            match read_runs(&text, &captured.content, *runs_left, stats) {
                Ok((runs, cut)) => {
                    *runs_left -= runs.len();
                    if cut {
                        truncated.by_formatting = true;
                    }
                    captured.runs = Some(runs);
                    captured.runs_truncated = cut;
                }
                Err(e) => node.fail(CaptureStep::Formatting, e),
            }
        }
        node.text = Some(captured);
    }
}

/// Read `text` from offset 0, in at most `max_bytes` bytes of UTF-8.
fn read_text(text: &crate::text_api::Text, length: u32, max_bytes: usize) -> Result<CapturedText> {
    if length == 0 {
        return Ok(CapturedText {
            content: String::new(),
            length,
            truncated: false,
            runs: None,
            runs_truncated: false,
        });
    }
    // A UTF-16 unit is at least one byte of UTF-8, so this many units can
    // only overshoot the budget, and the cut below brings it back.
    let units = length.min(u32::try_from(max_bytes).unwrap_or(u32::MAX));
    let mut content = if units == 0 {
        String::new()
    } else {
        match text.read(TextRange::new(0, units)?) {
            // A cut short of the end can split a surrogate pair, which the
            // backend refuses; one unit shorter cannot split it (as in
            // `find_in_chunks`). A refusal of the whole text is real.
            Err(Error::InvalidTextRange { .. }) if units < length => {
                text.read(TextRange::new(0, units - 1)?)?
            }
            other => other?,
        }
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
        runs: None,
        runs_truncated: false,
    })
}

/// The formatting runs over `content` (the text read from offset 0), at
/// most `max` of them, and whether `max` cut them short.
fn read_runs(
    text: &crate::text_api::Text,
    content: &str,
    max: usize,
    stats: &mut CaptureStats,
) -> Result<(Vec<TextRun>, bool)> {
    let units = u32::try_from(content.encode_utf16().count()).unwrap_or(u32::MAX);
    if units == 0 {
        return Ok((Vec::new(), false));
    }
    if max == 0 {
        return Ok((Vec::new(), true));
    }
    let page = text.runs(TextRange::new(0, units)?, max)?;
    stats.formatting_reads += page.runs.len();
    Ok((page.runs, page.truncated))
}

fn read_relations(
    raw: &mut [Raw],
    truncated: &mut Truncation,
    stats: &mut CaptureStats,
    stop: &impl Fn() -> Option<Stop>,
) {
    for node in raw.iter_mut().filter(|n| !n.vanished) {
        if let Some(why) = stop() {
            why.mark(truncated);
            return;
        }
        stats.relation_reads += 1;
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
        // The cut nodes say so: a node read before the capture filled keeps
        // its real child count, and one never read has none.
        let cut: Vec<&CapturedNode> = cap.nodes.iter().filter(|n| n.children_truncated).collect();
        assert!(!cut.is_empty());
        for node in cut {
            match node.children_count {
                Some(count) => assert!(count > node.children.len()),
                None => assert!(node.children.is_empty()),
            }
        }
    }

    #[test]
    fn a_full_capture_reads_no_more_children() {
        // Each read lists a node's every child, so the budget bounds reads,
        // not only what the capture keeps.
        for max in 1..=4 {
            let cap = app(build_provider())
                .capture(&CaptureOptions::new().max_nodes(max))
                .expect("capture");
            assert!(cap.nodes.len() <= max);
            assert!(
                cap.stats.children_reads <= max,
                "{} reads for a budget of {max}",
                cap.stats.children_reads
            );
        }
    }

    #[test]
    fn a_capture_with_room_to_spare_is_not_truncated() {
        let full = app(build_provider())
            .capture(&CaptureOptions::new())
            .expect("capture");
        let roomy = app(build_provider())
            .capture(&CaptureOptions::new().max_nodes(full.nodes.len() + 1))
            .expect("capture");
        assert!(!roomy.truncated.by_nodes);
        assert_eq!(roomy.nodes.len(), full.nodes.len());
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
    fn an_evicted_handle_is_a_failed_read_not_a_vanished_node() {
        let provider = build_provider();
        provider.fail_children(
            "Navigation",
            Error::stale(crate::StaleReason::Evicted, "toolbar"),
        );
        let cap = app(provider)
            .capture(&CaptureOptions::new())
            .expect("capture");
        assert_eq!(cap.vanished, 0);
        let nav = cap
            .nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("Navigation"))
            .expect("an evicted node stays in the capture");
        assert_eq!(nav.errors.len(), 1);
        assert_eq!(nav.errors[0].step, CaptureStep::Children);
        assert!(nav.children_truncated);
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

    #[test]
    fn an_unrepresentable_timeout_is_no_deadline() {
        // `Instant::now() + Duration::MAX` overflows; the walk must not panic.
        let cap = app(build_provider())
            .capture(&CaptureOptions::new().timeout(Duration::MAX))
            .expect("capture");
        assert!(!cap.truncated.any(), "{:?}", cap.truncated);
    }

    #[test]
    fn a_text_budget_that_splits_a_surrogate_pair_reads_one_unit_less() {
        // "Hello 😀 …": a 7-byte budget reads 7 units, ending inside the
        // emoji's surrogate pair, which the backend refuses.
        let cap = table_app()
            .capture(&CaptureOptions::new().text(true).max_text_bytes(7))
            .expect("capture");
        let notes = cap
            .nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("Notes"))
            .expect("Notes");
        assert!(notes.errors.is_empty(), "{:?}", notes.errors);
        let text = notes.text.as_ref().expect("Notes keeps its text");
        assert_eq!(text.content, "Hello ");
        assert!(text.truncated);
    }
    fn table_app() -> App {
        let provider: Arc<dyn Provider> = crate::mock::build_table_provider();
        App::list_with(provider)
            .expect("list")
            .into_iter()
            .next()
            .expect("one app")
    }

    #[test]
    fn formatting_detail_reads_runs_over_the_captured_text() {
        let app = table_app();
        let notes = |cap: &Capture| {
            cap.nodes
                .iter()
                .find(|n| n.name.as_deref() == Some("Notes"))
                .and_then(|n| n.text.clone())
                .expect("Notes has text")
        };
        // Formatting turns text on: runs are ranges in it.
        let cap = app
            .capture(&CaptureOptions::new().formatting(true))
            .expect("capture");
        let text = notes(&cap);
        let runs = text.runs.expect("formatting detail reads runs");
        assert!(!text.runs_truncated);
        assert!(!cap.truncated.by_formatting);
        // The mock gives one run per word, alternating bold and regular.
        assert_eq!(
            runs[0].range,
            TextRange::new(0, 6).expect("range"),
            "\"Hello \""
        );
        assert_eq!(runs[0].attributes.font_weight, Some(700));
        assert_eq!(runs.last().map(|r| r.range.end), Some(text.length));
        for pair in runs.windows(2) {
            assert_eq!(pair[0].range.end, pair[1].range.start, "{runs:?}");
        }
        let all_runs: usize = cap
            .nodes
            .iter()
            .filter_map(|n| n.text.as_ref()?.runs.as_ref())
            .map(Vec::len)
            .sum();
        assert_eq!(cap.stats.formatting_reads, all_runs);

        // Text detail alone reads no runs.
        let plain = app
            .capture(&CaptureOptions::new().text(true))
            .expect("capture");
        assert_eq!(notes(&plain).runs, None);
        assert_eq!(plain.stats.formatting_reads, 0);

        // The run budget cuts, and says so.
        let cut = app
            .capture(&CaptureOptions::new().formatting(true).max_runs(2))
            .expect("capture");
        let short = notes(&cut);
        assert_eq!(short.runs.as_ref().map(Vec::len), Some(2));
        assert!(short.runs_truncated);
        assert!(cut.truncated.by_formatting);
        assert!(cut.truncated.any());
    }

    #[test]
    fn stats_count_only_the_reads_a_detail_level_asks_for() {
        let app = table_app();
        let plain = app.capture(&CaptureOptions::new()).expect("capture");
        assert!(!plain.truncated.any());
        // Structure only: one children read per node, one identity read per
        // node, and nothing else.
        assert_eq!(plain.stats.children_reads, plain.nodes.len());
        assert_eq!(plain.stats.identity_reads, plain.nodes.len());
        assert_eq!(plain.stats.text_reads, 0);
        assert_eq!(plain.stats.formatting_reads, 0);
        assert_eq!(plain.stats.relation_reads, 0);
        assert_eq!(plain.stats.text_bytes, 0);
        assert_eq!(plain.backend, Some("mock"));

        let rich = app
            .capture(&CaptureOptions::new().text(true).relations(true))
            .expect("capture");
        assert_eq!(rich.stats.children_reads, plain.stats.children_reads);
        assert_eq!(rich.stats.relation_reads, rich.nodes.len());
        let with_text = rich.nodes.iter().filter(|n| n.text.is_some()).count();
        assert!(with_text > 0);
        assert_eq!(rich.stats.text_reads, with_text);
        let bytes: usize = rich
            .nodes
            .iter()
            .filter_map(|n| n.text.as_ref())
            .map(|t| t.content.len())
            .sum();
        assert_eq!(rich.stats.text_bytes, bytes);
    }

    #[test]
    fn a_cancelled_token_stops_the_walk_and_says_so() {
        let app = table_app();
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
        let options = CaptureOptions::new().text(true).cancel(token.clone());
        let full = app.capture(&options).expect("capture");
        assert!(!full.truncated.by_cancel);
        assert!(full.nodes.len() > 1);

        token.cancel();
        let cap = app
            .capture(&options)
            .expect("a cancelled capture still returns");
        assert_eq!(cap.nodes.len(), 1, "only the root, read before the walk");
        assert!(cap.root().children_truncated);
        assert!(cap.truncated.by_cancel);
        assert!(!cap.truncated.by_time);
        assert_eq!(cap.stats.children_reads, 0);
        assert_eq!(cap.stats.text_reads, 0, "text detail stops too");
        // Clones share the flag; a fresh token does not.
        assert_eq!(options.cancel, Some(token.clone()));
        assert_ne!(Some(CancelToken::new()), options.cancel);
    }
}
