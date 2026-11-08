//! Element refs for the MCP tools (#481).
//!
//! MCP is the one surface where a reference must outlive a call: a model
//! reads a tree, then acts on a node it saw. A `ref` (`c3:17`, node 17 of
//! capture 3) names exactly that [`Element`], so `action`, `text`, `table`
//! and `relations` reach that node or fail with `element_stale`. A ref is never
//! re-resolved by name or selector.
//!
//! Bounded twice: the table keeps the refs of the last [`MAX_CAPTURES`]
//! `tree` / `find` / `relations` results (an older ref fails with `ref_expired`), and each
//! capture is itself bounded by its tool's node or match cap. The CLI has no
//! refs: each invocation is a new process with nothing to resolve against.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::cli::{CliError, CliResult};
use crate::Element;

/// How many captures' refs the server holds at once.
pub(crate) const MAX_CAPTURES: usize = 8;

/// The refs of the most recent captures.
#[derive(Default)]
pub(crate) struct RefTable {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    next: u64,
    captures: VecDeque<(u64, Vec<Element>)>,
}

/// One `tree` / `find` result's refs, filled while the result is built and
/// kept by [`RefTable::keep`].
pub(crate) struct Capture {
    id: u64,
    elements: Vec<Element>,
}

impl Capture {
    /// Hold `element` and return its ref.
    pub(crate) fn add(&mut self, element: &Element) -> String {
        self.elements.push(element.clone());
        format!("c{}:{}", self.id, self.elements.len() - 1)
    }
}

impl RefTable {
    /// Start a capture; its refs resolve once it is [`kept`](Self::keep).
    pub(crate) fn begin(&self) -> Capture {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.next += 1;
        Capture {
            id: state.next,
            elements: Vec::new(),
        }
    }

    /// Hold `capture`'s refs, dropping the oldest capture past the bound.
    pub(crate) fn keep(&self, capture: Capture) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.captures.push_back((capture.id, capture.elements));
        while state.captures.len() > MAX_CAPTURES {
            state.captures.pop_front();
        }
    }

    /// The element `reference` names. A malformed ref is a usage error; one
    /// whose capture is no longer held is [`CliError::RefExpired`].
    pub(crate) fn resolve(&self, reference: &str) -> CliResult<Element> {
        let parsed = reference
            .strip_prefix('c')
            .and_then(|rest| rest.split_once(':'))
            .and_then(|(capture, index)| {
                Some((capture.parse::<u64>().ok()?, index.parse::<usize>().ok()?))
            });
        let Some((capture, index)) = parsed else {
            return Err(CliError::Usage(format!(
                "\"ref\" {reference:?} is not a ref: refs look like \"c3:17\", as `tree` and \
                 `find` return them"
            )));
        };
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let held = state
            .captures
            .iter()
            .find(|(id, _)| *id == capture)
            .and_then(|(_, elements)| elements.get(index));
        match held {
            Some(element) => Ok(element.clone()),
            None => Err(CliError::RefExpired {
                reference: reference.to_string(),
                kept: state
                    .captures
                    .iter()
                    .map(|(id, _)| format!("c{id}"))
                    .collect(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn element() -> Element {
        let provider: Arc<dyn crate::Provider> = xa11y_core::mock::build_provider();
        let root = provider.list_apps().expect("apps").remove(0);
        Element::new(root, provider)
    }

    #[test]
    fn refs_resolve_until_their_capture_ages_out() {
        let table = RefTable::default();
        let mut first = table.begin();
        let r = first.add(&element());
        assert_eq!(r, "c1:0");
        assert!(
            matches!(table.resolve(&r), Err(CliError::RefExpired { .. })),
            "not kept yet"
        );
        table.keep(first);
        assert!(table.resolve(&r).is_ok());
        for _ in 0..MAX_CAPTURES {
            let c = table.begin();
            table.keep(c);
        }
        match table.resolve(&r) {
            Err(CliError::RefExpired { reference, kept }) => {
                assert_eq!(reference, "c1:0");
                assert_eq!(kept.len(), MAX_CAPTURES);
                assert_eq!(kept[0], "c2");
            }
            other => panic!("expected RefExpired, got {:?}", other.err()),
        }
    }

    #[test]
    fn malformed_refs_are_usage_errors() {
        let table = RefTable::default();
        for bad in ["", "3:1", "c3", "c3:x", "cx:1", "c3:-1"] {
            assert!(
                matches!(table.resolve(bad), Err(CliError::Usage(_))),
                "{bad}"
            );
        }
        assert!(matches!(
            table.resolve("c99:0"),
            Err(CliError::RefExpired { .. })
        ));
    }
}
