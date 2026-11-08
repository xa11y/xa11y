//! Bounded capture (#482) against the AccessKit app.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use xa11y::*;

    #[test]
    #[ignore]
    fn a_capture_holds_the_whole_app_in_pre_order() {
        let app = h::app_root();
        let cap = app.capture(&CaptureOptions::new()).expect("capture");
        assert!(
            !cap.truncated.any(),
            "the fixture fits in 2000 nodes: {:?}",
            cap.truncated
        );
        assert_eq!(cap.root().role, Role::Application);
        for node in &cap.nodes {
            for &child in &node.children {
                assert_eq!(cap.nodes[child].parent, Some(node.index));
                assert!(child > node.index);
            }
        }
        // Every button a locator finds is in the capture, as the same node.
        let buttons = app.locator("button").elements().expect("buttons");
        assert!(!buttons.is_empty());
        for button in &buttons {
            assert!(
                cap.nodes
                    .iter()
                    .any(|n| n.element.same_as(button).unwrap_or(false)),
                "{button:?} is missing from the capture"
            );
        }
    }

    #[test]
    #[ignore]
    fn the_node_limit_keeps_the_top_of_the_tree() {
        let app = h::app_root();
        let cap = app
            .capture(&CaptureOptions::new().max_nodes(10))
            .expect("capture");
        assert_eq!(cap.nodes.len(), 10);
        assert!(cap.truncated.by_nodes);
        let full = app.capture(&CaptureOptions::new()).expect("capture");
        let deepest = cap.nodes.iter().map(|n| n.depth).max().unwrap_or(0);
        let above = |c: &Capture| c.nodes.iter().filter(|n| n.depth < deepest).count();
        assert_eq!(
            above(&cap),
            above(&full),
            "breadth-first: every shallower node is kept"
        );
    }

    #[test]
    #[ignore]
    fn a_captured_element_acts_on_the_node_read() {
        let app = h::app_root();
        let cap = app.capture(&CaptureOptions::new()).expect("capture");
        let submit = cap
            .nodes
            .iter()
            .find(|n| n.role == Role::Button && n.name.as_deref() == Some("Submit"))
            .expect("the Submit button");
        submit.element.check_alive().expect("still the same node");
        submit
            .element
            .press()
            .expect("press through the captured element");
    }

    #[test]
    #[ignore]
    fn relations_resolve_inside_the_capture() {
        let app = h::app_root();
        let cap = app
            .capture(&CaptureOptions::new().relations(true))
            .expect("capture");
        let button = cap
            .nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("Toggle Details"))
            .expect("the controller");
        // macOS reports aria-controls as `linked` (see semantics.rs).
        let kind = if cfg!(target_os = "macos") {
            RelationKind::Linked
        } else {
            RelationKind::Controls
        };
        let (_, targets) = button
            .relations
            .iter()
            .find(|(k, _)| *k == kind)
            .expect("the kind is reported");
        match targets.as_slice() {
            [RelationTarget::Node(i)] => {
                assert_eq!(cap.nodes[*i].name.as_deref(), Some("Details Text"))
            }
            other => panic!("expected the controlled node inside the capture: {other:?}"),
        }
    }
}
