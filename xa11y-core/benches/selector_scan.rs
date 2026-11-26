//! Measure filtering a materialized accessibility snapshot, excluding its acquisition.
use std::hint::black_box;
use std::time::Instant;
use xa11y_core::{element::ElementData, selector::matches_simple, Role, Selector};

fn main() {
    let nodes: Vec<_> = (0..10_000)
        .map(|index| {
            let mut node = ElementData::for_role(Role::Button);
            node.name = Some(format!("Record {index}"));
            node
        })
        .collect();
    let selector = Selector::parse(r#"button[name="Record 9999"]"#).unwrap();
    let mut samples = Vec::new();
    for trial in 0..12 {
        let start = Instant::now();
        for _ in 0..32 {
            let matches = black_box(&nodes)
                .iter()
                .filter(|node| matches_simple(node, &selector.segments[0].simple))
                .count();
            assert_eq!(matches, 1);
        }
        if trial > 0 {
            samples.push(start.elapsed().as_micros() / 32);
        }
    }
    samples.sort_unstable();
    println!(
        "10k snapshot selector median {} us/scan, samples {samples:?}",
        samples[5]
    );
}
