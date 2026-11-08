//! JS surface for bounded capture (#482).
//!
//! `capture(options?)` on `Element`, `App`, `Locator` and `ShellSurface`
//! takes the options as a trailing object (the options-struct fold) and
//! resolves to a plain [`Capture`] object. Nodes refer to each other by index
//! into `nodes` (`parent`, `children`, and in-capture relation targets).
//! Durations are milliseconds, as everywhere in this binding.

use napi::bindgen_prelude::Either;

use crate::element::Element;
use crate::map_err;

/// Options for `capture()`.
#[napi(object)]
#[derive(Clone, Default)]
pub struct CaptureOptions {
    /// Deepest level read; the root is depth 0. Omit for no depth limit.
    pub max_depth: Option<u32>,
    /// Most nodes in the capture, the root included. Default 2000.
    pub max_nodes: Option<u32>,
    /// Most bytes of text, in total, that `'text'` detail reads. Default
    /// 65536.
    pub max_text_bytes: Option<u32>,
    /// Extra reads per node: `'text'` and/or `'relations'`.
    pub detail: Option<Vec<String>>,
    /// Time budget in milliseconds: when it runs out, the capture resolves
    /// with what was read and `truncated.byTime`.
    pub timeout: Option<u32>,
}

/// Build core options. Runs before any platform call.
pub(crate) fn parse_options(
    options: Option<CaptureOptions>,
) -> napi::Result<xa11y::CaptureOptions> {
    let o = options.unwrap_or_default();
    let mut out = xa11y::CaptureOptions::new();
    if let Some(depth) = o.max_depth {
        out = out.max_depth(depth as usize);
    }
    if let Some(nodes) = o.max_nodes {
        out = out.max_nodes(nodes as usize);
    }
    if let Some(bytes) = o.max_text_bytes {
        out = out.max_text_bytes(bytes as usize);
    }
    for name in o.detail.unwrap_or_default() {
        out = match name.as_str() {
            "text" => out.text(true),
            "relations" => out.relations(true),
            other => {
                return Err(map_err(xa11y::Error::InvalidActionData {
                    message: format!(
                        "unknown capture detail {other:?}; expected 'text' or 'relations'"
                    ),
                }))
            }
        };
    }
    if let Some(ms) = o.timeout {
        out = out.timeout(std::time::Duration::from_millis(u64::from(ms)));
    }
    Ok(out)
}

/// A bounded read of a subtree: every node read, pre-order, and which
/// limits cut it.
#[napi(object, object_from_js = false)]
pub struct Capture {
    /// Every node read, pre-order. The root is index 0.
    pub nodes: Vec<CapturedNode>,
    /// Which limits dropped something.
    pub truncated: Truncation,
    /// Nodes that disappeared while the capture read them, and were dropped.
    pub vanished: u32,
    /// When the walk began, in milliseconds since the epoch.
    pub started: f64,
    /// When the walk ended, in milliseconds since the epoch.
    pub finished: f64,
}

/// Which limits dropped something.
#[napi(object)]
#[derive(Clone)]
pub struct Truncation {
    /// Children below `maxDepth` were left out.
    pub by_depth: bool,
    /// Nodes past `maxNodes` were left out.
    pub by_nodes: bool,
    /// Text past `maxTextBytes` was left out.
    pub by_text: bool,
    /// The time budget ran out before the walk finished.
    pub by_time: bool,
}

/// One node of a capture.
#[napi(object, object_from_js = false)]
pub struct CapturedNode {
    /// Index in `nodes`, which is pre-order. 0-based.
    pub index: u32,
    /// The parent's index; `null` on the root.
    pub parent: Option<u32>,
    /// Depth below the root, which is 0.
    pub depth: u32,
    /// The element, which names exactly this node: actions on it reach it
    /// or reject with `ElementStaleError`.
    pub element: Element,
    /// Indices of the children in the capture, in platform order.
    pub children: Vec<u32>,
    /// How many children the node has, including ones the capture left out;
    /// `null` when they were not read.
    pub children_count: Option<u32>,
    /// Whether children of this node are missing from the capture.
    pub children_truncated: bool,
    /// Reads of this node that failed.
    pub errors: Vec<CaptureError>,
    /// The node's text, with `'text'` detail, when it has a text interface.
    pub text: Option<CapturedText>,
    /// With `'relations'` detail: each kind the platform reports, with its
    /// targets (possibly none).
    pub relations: Vec<CapturedRelation>,
}

/// A read of one node that failed.
#[napi(object)]
#[derive(Clone)]
pub struct CaptureError {
    /// `'children'`, `'identity'`, `'text'`, `'relations'` or `'cycle'`.
    pub step: String,
    /// The error code the call would have rejected with (`'XA11Y_PLATFORM'`).
    pub code: String,
    /// The error message.
    pub message: String,
}

/// An element's text, as far as the capture's text budget reached.
#[napi(object)]
#[derive(Clone)]
pub struct CapturedText {
    /// The text read, from offset 0.
    pub content: String,
    /// The element's full text length, in UTF-16 code units.
    pub length: u32,
    /// Whether `content` stops short of `length`.
    pub truncated: bool,
}

/// One relation kind of a captured node.
#[napi(object, object_from_js = false)]
pub struct CapturedRelation {
    /// The kind: `'labelled_by'`, `'controls'`, ...
    pub kind: String,
    /// Each target: a `number` index into `nodes` when inside the capture,
    /// an `Element` when outside it.
    #[napi(ts_type = "Array<number | Element>")]
    pub targets: Vec<Either<u32, Element>>,
}

fn epoch_ms(t: std::time::SystemTime) -> f64 {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64() * 1000.0,
        Err(e) => -e.duration().as_secs_f64() * 1000.0,
    }
}

/// The `XA11Y_*` code `map_err` gives an error: the tag before its message.
fn error_code(e: &xa11y::Error) -> String {
    let reason = map_err(e.clone()).reason;
    reason
        .split_once(':')
        .map_or(reason.clone(), |(code, _)| code.to_string())
}

fn index(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

impl From<xa11y::Capture> for Capture {
    fn from(cap: xa11y::Capture) -> Self {
        let nodes = cap
            .nodes
            .into_iter()
            .map(|node| CapturedNode {
                index: index(node.index),
                parent: node.parent.map(index),
                depth: index(node.depth),
                children: node.children.iter().copied().map(index).collect(),
                children_count: node.children_count.map(index),
                children_truncated: node.children_truncated,
                errors: node
                    .errors
                    .iter()
                    .map(|e| CaptureError {
                        step: e.step.to_snake_case().to_string(),
                        code: error_code(&e.error),
                        message: e.error.to_string(),
                    })
                    .collect(),
                text: node.text.as_ref().map(|t| CapturedText {
                    content: t.content.clone(),
                    length: t.length,
                    truncated: t.truncated,
                }),
                relations: node
                    .relations
                    .into_iter()
                    .map(|(kind, targets)| CapturedRelation {
                        kind: kind.to_snake_case().to_string(),
                        targets: targets
                            .into_iter()
                            .map(|t| match t {
                                xa11y::RelationTarget::Node(i) => Either::A(index(i)),
                                xa11y::RelationTarget::Outside(el) => {
                                    Either::B(Element::from_core(*el))
                                }
                            })
                            .collect(),
                    })
                    .collect(),
                element: Element::from_core(node.element),
            })
            .collect();
        Capture {
            nodes,
            truncated: Truncation {
                by_depth: cap.truncated.by_depth,
                by_nodes: cap.truncated.by_nodes,
                by_text: cap.truncated.by_text,
                by_time: cap.truncated.by_time,
            },
            vanished: index(cap.vanished),
            started: epoch_ms(cap.started),
            finished: epoch_ms(cap.finished),
        }
    }
}
