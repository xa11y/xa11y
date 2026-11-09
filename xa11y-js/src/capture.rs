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
use crate::types::{checked_count, checked_millis};

/// Options for `capture()`.
#[napi(object, object_to_js = false)]
#[derive(Clone, Default)]
pub struct CaptureOptions {
    /// Deepest level read; the root is depth 0. Omit for no depth limit.
    pub max_depth: Option<f64>,
    /// Most nodes in the capture, the root included. Default 2000.
    pub max_nodes: Option<f64>,
    /// Most bytes of text, in total, that `'text'` detail reads. Default
    /// 65536.
    pub max_text_bytes: Option<f64>,
    /// Extra reads per node: `'text'`, `'formatting'` (text plus its
    /// attribute runs) and/or `'relations'`.
    pub detail: Option<Vec<String>>,
    /// Time budget in milliseconds: when it runs out, the capture resolves
    /// with what was read and `truncated.byTime`.
    pub timeout: Option<f64>,
    /// Most attribute runs, in total, that `'formatting'` detail reads.
    /// Default 4096.
    pub max_runs: Option<f64>,
    /// A token whose `cancel()` stops the walk between nodes; the capture
    /// resolves with what was read and `truncated.byCancel`.
    #[napi(ts_type = "CancelToken")]
    pub cancel: Option<CancelToken>,
}

/// Stops a capture that is in progress. Pass it as `capture({ cancel })`
/// and call `cancel()` from anywhere: the walk checks it between nodes.
#[napi]
#[derive(Clone, Default)]
pub struct CancelToken {
    inner: xa11y::CancelToken,
}

// A token arrives inside the options object, which napi converts by value.
// The JS object is the class instance; this clones the shared flag out of it,
// so `cancel()` on the JS side still reaches the walk.
impl napi::bindgen_prelude::FromNapiValue for CancelToken {
    unsafe fn from_napi_value(
        env: napi::sys::napi_env,
        value: napi::sys::napi_value,
    ) -> napi::Result<Self> {
        // SAFETY: forwarded unchanged from napi's own by-value conversion.
        let token: &CancelToken = unsafe {
            <CancelToken as napi::bindgen_prelude::FromNapiRef>::from_napi_ref(env, value)?
        };
        Ok(token.clone())
    }
}

#[napi]
impl CancelToken {
    /// A token that has not been cancelled.
    #[napi(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask every capture holding this token to stop. Idempotent.
    #[napi]
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// Whether `cancel()` has been called.
    #[napi]
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}

/// Build core options. Runs before any platform call, so a bad number
/// (negative, fractional, `NaN`) throws before the walk starts.
pub(crate) fn parse_options(
    options: Option<CaptureOptions>,
) -> napi::Result<xa11y::CaptureOptions> {
    let o = options.unwrap_or_default();
    let mut out = xa11y::CaptureOptions::new();
    let count = |value: f64, what: &str| checked_count(value, what).map_err(map_err);
    if let Some(depth) = o.max_depth {
        out = out.max_depth(count(depth, "maxDepth")?);
    }
    if let Some(nodes) = o.max_nodes {
        out = out.max_nodes(count(nodes, "maxNodes")?);
    }
    if let Some(bytes) = o.max_text_bytes {
        out = out.max_text_bytes(count(bytes, "maxTextBytes")?);
    }
    for name in o.detail.unwrap_or_default() {
        out = match name.as_str() {
            "text" => out.text(true),
            "formatting" => out.formatting(true),
            "relations" => out.relations(true),
            other => {
                return Err(map_err(xa11y::Error::invalid_action_data(format!(
                "unknown capture detail {other:?}; expected 'text', 'formatting' or 'relations'"
            ))))
            }
        };
    }
    if let Some(ms) = o.timeout {
        out = out.timeout(checked_millis(ms, "timeout").map_err(map_err)?);
    }
    if let Some(runs) = o.max_runs {
        out = out.max_runs(count(runs, "maxRuns")?);
    }
    if let Some(token) = o.cancel {
        out = out.cancel(token.inner);
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
    /// Which platform API read the nodes: `'atspi'`, `'ax'`, `'uia'`, `'ia2'`
    /// or `'mock'`.
    pub backend: Option<String>,
    /// How many platform reads the capture made, by kind.
    pub stats: CaptureStats,
}

/// How many platform reads a capture made, by kind. A structure-only
/// capture makes only children and identity reads.
#[napi(object, object_from_js = false)]
#[derive(Clone)]
pub struct CaptureStats {
    /// Child-list reads, one per node whose children were read.
    pub children_reads: u32,
    /// Identity reads, one per node.
    pub identity_reads: u32,
    /// Text reads (`'text'` detail).
    pub text_reads: u32,
    /// Attribute runs read (`'formatting'` detail).
    pub formatting_reads: u32,
    /// Relation reads (`'relations'` detail).
    pub relation_reads: u32,
    /// Bytes of text read.
    pub text_bytes: u32,
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
    /// Attribute runs past `maxRuns` were left out.
    pub by_formatting: bool,
    /// The `cancel` token was cancelled before the walk finished.
    pub by_cancel: bool,
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

/// A read of one node that failed. The capture continued past it.
#[napi(object, object_from_js = false)]
#[derive(Clone)]
pub struct CaptureError {
    /// `'children'`, `'identity'`, `'text'`, `'formatting'`, `'relations'`
    /// or `'cycle'`.
    pub step: String,
    /// The error code the call would have rejected with (`'XA11Y_PLATFORM'`).
    pub code: String,
    /// The error message.
    pub message: String,
    /// The error the read raised, as the typed `XA11yError` subclass a
    /// rejected call would carry, with its structured fields (`delivery`,
    /// `pending`, `selector`, `reason`, ...). Same as Python's
    /// `CaptureError.error`.
    // Crosses as `map_err`'s encoded reason; `index.js` turns it into the
    // typed error before the capture reaches the caller.
    #[napi(ts_type = "XA11yError")]
    pub error: String,
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
    /// With `'formatting'` detail: the attribute runs over `content`.
    pub runs: Option<Vec<crate::types::TextRun>>,
    /// Whether runs past `maxRuns` were left out of `runs`.
    pub runs_truncated: bool,
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

pub(crate) fn epoch_ms(t: std::time::SystemTime) -> f64 {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64() * 1000.0,
        Err(e) => -e.duration().as_secs_f64() * 1000.0,
    }
}

/// One failed read, as JS sees it: `code` is the `XA11Y_*` tag `map_err`
/// gives the error, and `error` its whole encoded reason (tag, message and
/// structured payload), which `index.js` turns into the typed error.
fn capture_error(step: &str, e: &xa11y::Error) -> CaptureError {
    let reason = map_err(e.clone()).reason;
    let code = reason
        .split_once(':')
        .map_or(reason.as_str(), |(code, _)| code)
        .to_string();
    CaptureError {
        step: step.to_string(),
        code,
        message: e.to_string(),
        error: reason,
    }
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
                    .map(|e| capture_error(e.step.to_snake_case(), &e.error))
                    .collect(),
                text: node.text.as_ref().map(|t| CapturedText {
                    content: t.content.clone(),
                    length: t.length,
                    truncated: t.truncated,
                    runs: t
                        .runs
                        .clone()
                        .map(|runs| runs.into_iter().map(Into::into).collect()),
                    runs_truncated: t.runs_truncated,
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
                by_formatting: cap.truncated.by_formatting,
                by_cancel: cap.truncated.by_cancel,
            },
            vanished: index(cap.vanished),
            started: epoch_ms(cap.started),
            finished: epoch_ms(cap.finished),
            backend: cap.backend.map(str::to_string),
            stats: CaptureStats {
                children_reads: index(cap.stats.children_reads),
                identity_reads: index(cap.stats.identity_reads),
                text_reads: index(cap.stats.text_reads),
                formatting_reads: index(cap.stats.formatting_reads),
                relation_reads: index(cap.stats.relation_reads),
                text_bytes: index(cap.stats.text_bytes),
            },
        }
    }
}
