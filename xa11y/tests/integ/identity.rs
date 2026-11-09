//! Element identity and staleness (#481) against the AccessKit app, on every
//! platform cell.
//!
//! The app's "Add Item" / "Remove Item" buttons create and destroy a list
//! item at runtime, which is the one thing a stale-element test needs: a
//! node that existed when it was captured and does not now.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use std::time::Duration;
    use xa11y::*;

    const WAIT: Duration = Duration::from_secs(5);
    const ITEM: &str = r#"list_item[name="Item 1"]"#;

    /// Leave the dynamic list empty, so the test starts from a known state
    /// whatever ran before it.
    fn empty_dynamic_list(app: &App) {
        while app
            .locator(r#"list[name="Dynamic List"] list_item"#)
            .count()
            .expect("count")
            > 0
        {
            app.locator(r#"button[name="Remove Item"]"#)
                .press()
                .expect("press Remove Item");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    #[ignore]
    fn a_captured_element_is_one_node_and_fails_stale_once_removed() {
        let app = h::app_root();
        empty_dynamic_list(&app);
        app.locator(r#"button[name="Add Item"]"#)
            .press()
            .expect("press Add Item");
        let item = app
            .locator(ITEM)
            .wait_attached(WAIT)
            .expect("Item 1 appears");

        // Identity: a second find of the same node is the same node; a
        // different node is not.
        let again = app.locator(ITEM).element().expect("find Item 1 again");
        assert!(
            item.same_as(&again).expect("identity"),
            "two finds of one node"
        );
        let add = app
            .locator(r#"button[name="Add Item"]"#)
            .element()
            .expect("Add Item");
        assert!(!item.same_as(&add).expect("identity"), "a different node");
        item.check_alive().expect("alive before removal");

        app.locator(r#"button[name="Remove Item"]"#)
            .press()
            .expect("press Remove Item");
        app.locator(ITEM)
            .wait_detached(WAIT)
            .expect("Item 1 disappears");

        // The captured element now names nothing: every path refuses with a
        // typed reason instead of acting on whatever the id names now.
        for (what, result) in [("check_alive", item.check_alive()), ("focus", item.focus())] {
            match result {
                Err(Error::ElementStale {
                    reason: StaleReason::Removed,
                    element,
                }) => assert!(element.contains("Item 1"), "{what}: {element}"),
                other => {
                    panic!("{what} on a removed node: expected stale (removed), got {other:?}")
                }
            }
        }

        // Not asserted, by design: after "Add Item" again, AccessKit gives the
        // new item the same NodeId, which on Linux (object path) and Windows
        // (RuntimeId) is the same platform id with the same role. That is the
        // native-id reuse `xa11y_core::identity` documents; the role guard
        // cannot see it.
    }
}
