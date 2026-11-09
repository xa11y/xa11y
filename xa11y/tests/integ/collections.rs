//! Collection positions (`CollectionInfo`) against the AccessKit test app.
//!
//! Split out of `integ::rich` so it runs wherever the AccessKit app does,
//! including the Wayland job, which skips `integ::rich` because its GTK 3
//! fixture is not in that image. GTK 3 sets no `posinset` / `setsize`, so
//! AccessKit's "Fruit List" is the Linux fixture for this. Depth is not
//! asserted: `accesskit_atspi_common` exports no `level`. Chromium's tree
//! depth is covered by `tests/suites/python/test_web_rich.py`.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use xa11y::*;

    #[test]
    #[ignore]
    fn list_items_report_position_and_set_size() {
        let app = h::app_root();
        let items = app
            .locator(r#"list[name="Fruit List"] > list_item"#)
            .elements()
            .expect("fruit list items");
        let names: Vec<_> = items.iter().map(|i| i.name.as_deref()).collect();
        assert_eq!(names, [Some("Apple"), Some("Banana"), Some("Cherry")]);
        for (i, item) in items.iter().enumerate() {
            let c = item
                .collection
                .unwrap_or_else(|| panic!("{:?} has no collection info", item.name));
            assert_eq!(c.index, Some(i as u32), "{:?}", item.name);
            assert_eq!(c.size, Some(Count::Known(3)), "{:?}", item.name);
            // accesskit_atspi_common exports no `level`.
            assert_eq!(c.depth, None, "{:?}", item.name);
        }
        let banana = h::one(&app, r#"list_item[index="1"][size="3"]"#);
        assert_eq!(banana.name.as_deref(), Some("Banana"));
    }
}
