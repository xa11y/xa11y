//! Selector CPU time on a snapshot and traversal cost with a synthetic provider.
use std::cell::Cell;
use std::hint::black_box;
use std::time::Instant;
use xa11y_core::{
    element::ElementData,
    selector::{find_elements_in_tree_group, matches_simple},
    Role, Selector, SelectorGroup,
};

fn measure(label: &str, batch: u32, mut run: impl FnMut()) {
    let mut samples = Vec::new();
    for trial in 0..12 {
        let start = Instant::now();
        for _ in 0..batch {
            run();
        }
        if trial > 0 {
            samples.push(start.elapsed().as_micros() / u128::from(batch));
        }
    }
    samples.sort_unstable();
    println!(
        "- {label}: median {} us/query, samples {samples:?}",
        samples[5]
    );
}

fn main() {
    let nodes: Vec<_> = (0..10_000)
        .map(|index| {
            let mut node = ElementData::for_role(Role::Button);
            node.handle = 101 + index;
            node.name = Some(format!("Record {index}"));
            node
        })
        .collect();
    for (label, query, expected) in [
        ("exact name", r#"button[name="Record 9999"]"#, 1),
        ("substring", r#"button[name*="CORD 99"]"#, 111),
        ("role only", "button", 10_000),
    ] {
        let selector = Selector::parse(query).unwrap();
        measure(&format!("10k snapshot {label}"), 32, || {
            let count = black_box(&nodes)
                .iter()
                .filter(|node| matches_simple(node, &selector.segments[0].simple))
                .count();
            assert_eq!(count, expected);
        });
    }

    // 100 groups with 100 buttons each. Return owned child snapshots just
    // like Provider::get_children, counting requests separately from time.
    let groups: Vec<_> = (1..=100)
        .map(|index| {
            let mut node = ElementData::for_role(Role::Group);
            node.handle = index;
            node
        })
        .collect();
    for (label, query, limit, expected) in [
        ("first", "button", Some(1), 1),
        ("all", "button", None, 10_000),
        ("miss", r#"button[name="Missing"]"#, None, 0),
        ("child", r#"group:nth(100) > button"#, None, 100),
        (
            "union",
            r#"button[name="Record 0"], button[name="Record 9999"]"#,
            None,
            2,
        ),
    ] {
        let selector = SelectorGroup::parse(query).unwrap();
        let reads = Cell::new(0);
        measure(&format!("10k traversal {label}"), 1, || {
            reads.set(0);
            let found = find_elements_in_tree_group(
                |parent| {
                    reads.set(reads.get() + 1);
                    Ok(match parent.map(|node| node.handle) {
                        None => groups.clone(),
                        Some(handle @ 1..=100) => {
                            let offset = (handle as usize - 1) * 100;
                            nodes[offset..offset + 100].to_vec()
                        }
                        _ => Vec::new(),
                    })
                },
                None,
                &selector,
                limit,
                None,
            )
            .unwrap();
            assert_eq!(found.len(), expected);
        });
        println!("  - {} child reads/query", reads.get());
    }
}
