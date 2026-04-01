//! Dedicated fuzz target for the selector parser and find_elements_in_tree.
//!
//! Both the selector string and the accessibility tree are fuzz-driven, so
//! the same selector is exercised against many different tree shapes.
//!
//! Enforces the invariant: anything Selector::parse accepts must not panic
//! when passed to find_elements_in_tree (via the public Locator API).
#![no_main]

#[path = "mock.rs"]
mod mock;

use std::sync::Arc;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mock::{build_provider, FuzzElement};
use xa11y::{App, Provider, Selector};

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    /// Random accessibility tree to search against.
    elements: Vec<FuzzElement>,
    /// Selector string — the primary fuzzing surface.
    selector: String,
}

fuzz_target!(|input: FuzzInput| {
    // Exercise Selector::parse on all inputs (valid or not).
    let _ = Selector::parse(&input.selector);

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

    // Chaining: exercises selector concatenation with the same fuzz string.
    let _ = app.locator(&input.selector).child(&input.selector).exists();
    let _ = app
        .locator(&input.selector)
        .descendant(&input.selector)
        .exists();
});
