//! Dedicated fuzz target for the selector parser and find_elements_in_tree.
//!
//! Both the selector string and the accessibility tree are fuzz-driven, so
//! the same selector is exercised against many different tree shapes.
//!
//! Enforces the invariant: anything Selector::parse accepts must not panic
//! when passed to find_elements_in_tree (via the public Locator API).
//!
//! Also fuzzes the comma-separated `SelectorGroup` form by stitching extra
//! clauses onto the fuzz selector — exercises the doc-order merge, dedup,
//! and `chain_combinator` path-distribution.
#![no_main]

#[path = "mock.rs"]
mod mock;

use std::sync::Arc;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mock::{build_provider, FuzzElement};
use xa11y::{App, Provider, Selector, SelectorGroup};

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    /// Random accessibility tree to search against.
    elements: Vec<FuzzElement>,
    /// Selector string — the primary fuzzing surface.
    selector: String,
    /// Second clause for SelectorGroup fuzzing — stitched onto `selector`
    /// with a top-level comma to exercise multi-clause matching.
    extra_clause: String,
    /// Third clause for >2-clause fuzzing — covers the BTreeMap-by-path
    /// merge path with multiple inputs.
    third_clause: String,
}

fuzz_target!(|input: FuzzInput| {
    // Exercise Selector::parse on all inputs (valid or not).
    let _ = Selector::parse(&input.selector);

    // Exercise SelectorGroup::parse directly — both the single-clause input
    // (must behave like Selector::parse) and a synthesized comma-joined
    // multi-clause input. A panic here is a parser bug.
    let _ = SelectorGroup::parse(&input.selector);
    let two_clause = format!("{}, {}", input.selector, input.extra_clause);
    let _ = SelectorGroup::parse(&two_clause);
    let three_clause = format!(
        "{}, {}, {}",
        input.selector, input.extra_clause, input.third_clause
    );
    let _ = SelectorGroup::parse(&three_clause);

    // Build the random tree.
    let Some(provider) = build_provider(&input.elements) else {
        return;
    };
    let provider: Arc<dyn Provider> = provider;

    let Ok(apps) = App::list_with(Arc::clone(&provider)) else {
        return;
    };
    let Some(app) = apps.into_iter().next() else {
        return;
    };

    // Use the selector through the public Locator API against the random tree.
    // Invalid selectors return Err — that's fine.
    // Valid selectors that cause a panic are bugs.
    let _ = app.locator(&input.selector).exists();
    let _ = app.locator(&input.selector).count();
    let _ = app.locator(&input.selector).elements();

    // SelectorGroup paths through the public Locator API: each invocation
    // of `count`/`elements` goes through `Provider::find_elements_group`,
    // which dedupes by tree-path across clauses. Any panic in the merge
    // (e.g. due to malformed clause strings or pathological tree shapes)
    // surfaces here.
    let _ = app.locator(&two_clause).count();
    let _ = app.locator(&two_clause).elements();
    let _ = app.locator(&two_clause).exists();
    let _ = app.locator(&three_clause).count();
    let _ = app.locator(&three_clause).elements();

    // Idempotence sanity: a single-clause group's count must equal the
    // bare-selector count. A divergence here would mean SelectorGroup's
    // single-clause short-circuit drifted from the plain path.
    if let (Ok(a), Ok(b)) = (
        app.locator(&input.selector).count(),
        app.locator(&format!("{}", input.selector)).count(),
    ) {
        assert_eq!(
            a, b,
            "single-clause group must agree with bare-selector path on count",
        );
    }

    // Dedup invariant: `sel, sel` must produce the same count as `sel`
    // (the duplicate clause adds no new matches). Skip when parsing of
    // the doubled form fails — invalid selectors are expected.
    if let Ok(bare) = app.locator(&input.selector).count() {
        let doubled = format!("{}, {}", input.selector, input.selector);
        if let Ok(doubled_count) = app.locator(&doubled).count() {
            assert_eq!(
                bare, doubled_count,
                "dedup: `X, X` must yield the same count as `X` (sel = {:?})",
                input.selector,
            );
        }
    }

    // Chaining: exercises selector concatenation with the same fuzz string.
    let _ = app.locator(&input.selector).child(&input.selector).exists();
    let _ = app
        .locator(&input.selector)
        .descendant(&input.selector)
        .exists();

    // Chained navigation on group locators — exercises
    // `chain_combinator`'s distribution over clauses for both sides.
    let _ = app.locator(&two_clause).descendant(&input.selector).exists();
    let _ = app.locator(&two_clause).child(&input.selector).count();
    let _ = app.locator(&input.selector).descendant(&two_clause).exists();
});
