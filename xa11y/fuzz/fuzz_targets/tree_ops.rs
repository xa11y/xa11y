//! Fuzz target for the xa11y public API surface.
//!
//! Exercises public entry points only — the mock Provider is test
//! infrastructure, but the fuzz body never calls provider methods directly:
//!
//! - Selector::parse (arbitrary strings)
//! - App::by_name_with, App::by_pid_with, App::list_with
//! - App::locator, App::children
//! - Locator: exists, count, element, elements, nth, first, child, descendant
//! - Element: children, parent, Display
//! - ElementData serde round-trip (arbitrary bytes → from_slice)
#![no_main]

#[path = "mock.rs"]
mod mock;

use std::sync::Arc;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mock::{build_provider, FuzzElement};
use xa11y::{App, ElementData, Locator, Provider, Selector};

// ── Fuzz-only types (tree_ops-specific) ───────────────────────────────────────

/// A sequence of Locator API calls to exercise on a resolved locator.
#[derive(Arbitrary, Debug)]
enum LocatorOp {
    Exists,
    Count,
    Element,
    Elements,
    NthThenExists(usize),
    FirstThenExists,
    /// Call `.child(sel)` then `.exists()` — exercises selector concatenation.
    ChildThenExists(String),
    /// Call `.descendant(sel)` then `.exists()` — exercises selector concatenation.
    DescendantThenExists(String),
}

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    fuzz_elements: Vec<FuzzElement>,
    /// Passed to Selector::parse and App::locator.
    selector: String,
    /// Passed to App::by_name_with.
    app_name: String,
    /// Passed to App::by_pid_with.
    pid: u32,
    /// Sequence of Locator API calls to exercise on each found app.
    locator_ops: Vec<LocatorOp>,
    /// Arbitrary bytes tried as ElementData JSON for serde fuzzing.
    json_bytes: Vec<u8>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn apply_locator_op(loc: Locator, op: &LocatorOp) {
    match op {
        LocatorOp::Exists => {
            let _ = loc.exists();
        }
        LocatorOp::Count => {
            let _ = loc.count();
        }
        LocatorOp::Element => {
            let _ = loc.element();
        }
        LocatorOp::Elements => {
            let _ = loc.elements();
        }
        LocatorOp::NthThenExists(n) => {
            let n = (*n).max(1); // nth is 1-based, clamp to at least 1
            let _ = loc.nth(n).exists();
        }
        LocatorOp::FirstThenExists => {
            let _ = loc.first().exists();
        }
        LocatorOp::ChildThenExists(sel) => {
            let _ = loc.child(sel).exists();
        }
        LocatorOp::DescendantThenExists(sel) => {
            let _ = loc.descendant(sel).exists();
        }
    }
}

// ── Fuzz target ───────────────────────────────────────────────────────────────

fuzz_target!(|input: FuzzInput| {
    // ── 1. Selector::parse — pure parser, no provider needed ─────────────────
    let _ = Selector::parse(&input.selector);

    // ── 2. ElementData serde — try to deserialize arbitrary bytes ────────────
    let _ = serde_json::from_slice::<ElementData>(&input.json_bytes);

    // ── 3. Build the mock provider ────────────────────────────────────────────
    let Some(provider) = build_provider(&input.fuzz_elements) else {
        return;
    };
    let provider: Arc<dyn Provider> = provider;

    // ── 4. App::by_name_with — exercises name-embedding in selector string ────
    //    Names containing '"' produce invalid selector strings; the method must
    //    return Err, not panic.
    let _ = App::by_name_with(Arc::clone(&provider), &input.app_name);

    // ── 5. App::by_pid_with ───────────────────────────────────────────────────
    let _ = App::by_pid_with(Arc::clone(&provider), input.pid);

    // ── 6. App::list_with → children, locator, serde ─────────────────────────
    let Ok(apps) = App::list_with(Arc::clone(&provider)) else {
        return;
    };

    for app in &apps {
        // Display
        let _ = app.to_string();

        // App::children → Element::children, parent, Display
        if let Ok(children) = app.children() {
            for child in children.iter().take(8) {
                let _ = child.to_string();
                let _ = child.children();
                let _ = child.parent();
            }
        }

        // App::locator with the fuzz selector string, then all LocatorOps
        let loc = app.locator(&input.selector);
        for op in input.locator_ops.iter().take(16) {
            apply_locator_op(loc.clone(), op);
        }

        // Serde round-trip on a real ElementData produced by the provider
        if let Ok(json) = serde_json::to_string(&app.data) {
            let _ = serde_json::from_str::<ElementData>(&json);
        }
    }
});
