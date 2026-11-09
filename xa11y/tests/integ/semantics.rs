//! Document semantics (#476) against the AccessKit app, on every platform
//! cell: landmarks, a link's URL, a live region and the controls relation.
//!
//! What each assertion expects is what AccessKit's platform adapter
//! publishes, which is narrower than what each platform can report:
//!
//! - Linux (`accesskit_unix`): landmarks as `ROLE_LANDMARK` without
//!   `xml-roles`, so their kind is `unknown`; link URIs through `Hyperlink`;
//!   `controls` as `CONTROLLER_FOR`. No `level`, `live` or other relation.
//! - macOS (`accesskit_macos`): the `AXLandmark*` subroles, `AXURL`, and
//!   `controls` targets in `AXLinkedUIElements` (read as `linked`). No
//!   `AXARIALive`, and no `AXValue` on a heading.
//! - Windows (`accesskit_windows`): landmarks through `AriaRole` only, no
//!   `LandmarkType` or `HeadingLevel`; a link's URL as its value;
//!   `LiveSetting`; `ControllerFor`.
//!
//! The heading's level is set in the fixture and reported by none of the
//! three adapters, so it is not asserted here; the mock and the backend unit
//! tests cover reading it.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use xa11y::*;

    const DOC_LINK_URL: &str = "https://example.com/docs";

    fn one(app: &App, selector: &str) -> Element {
        let mut found = app.locator(selector).elements().expect("query");
        assert_eq!(
            found.len(),
            1,
            "{selector} must match one element, matched {}. App: {app}",
            found.len()
        );
        found.remove(0)
    }

    #[test]
    #[ignore]
    fn landmarks_are_one_role_with_a_kind() {
        let app = h::app_root();
        let main = one(&app, r#"landmark[name="Document Main"]"#);
        let nav = one(&app, r#"landmark[name="Document Nav"]"#);
        if cfg!(target_os = "linux") {
            // accesskit_unix sets no `xml-roles`, so the kind is unknown
            // rather than absent: it is a landmark, of an unreported kind.
            assert_eq!(main.landmark_type, Some(Landmark::Unknown));
            assert_eq!(nav.landmark_type, Some(Landmark::Unknown));
        } else {
            assert_eq!(main.landmark_type, Some(Landmark::Main));
            assert_eq!(nav.landmark_type, Some(Landmark::Navigation));
            let by_kind = one(&app, r#"landmark[landmark_type="navigation"]"#);
            assert!(by_kind.same_as(&nav).expect("compare"));
        }
        // The landmark's own subtree is intact under the new role.
        one(
            &app,
            r#"landmark[name="Document Main"] heading[name="Document Heading"]"#,
        );
        one(
            &app,
            r#"landmark[name="Document Nav"] link[name="Docs Link"]"#,
        );
        // Only landmarks carry a kind.
        let heading = one(&app, r#"heading[name="Document Heading"]"#);
        assert_eq!(heading.landmark_type, None);
    }

    #[test]
    #[ignore]
    fn the_retired_navigation_role_fails_with_its_replacement() {
        let app = h::app_root();
        match app.locator("navigation").elements() {
            Err(Error::InvalidSelector { message, .. }) => assert!(
                message.contains(r#"landmark[landmark_type="navigation"]"#),
                "the error must name the replacement: {message}"
            ),
            other => panic!("expected InvalidSelector, got {other:?}"),
        }
    }

    #[test]
    #[ignore]
    fn a_link_reports_its_url() {
        let app = h::app_root();
        let link = one(&app, r#"link[name="Docs Link"]"#);
        assert_eq!(link.link_url.as_deref(), Some(DOC_LINK_URL));
        let by_url = one(&app, &format!(r#"link[url="{DOC_LINK_URL}"]"#));
        assert!(by_url.same_as(&link).expect("compare"));
        // A link without a URL reports none.
        let plain = one(&app, r#"link[name="Visit Example"]"#);
        assert_eq!(plain.link_url, None);
    }

    #[test]
    #[ignore]
    fn a_live_region_reports_its_politeness_where_the_adapter_publishes_it() {
        let app = h::app_root();
        // Found by its politeness rather than its name: AT-SPI names a
        // Label from its value, which starts empty.
        let polite = app.locator(r#"[live="polite"]"#).elements().expect("query");
        if cfg!(target_os = "windows") {
            assert_eq!(polite.len(), 1, "the Announcements label: {polite:?}");
            let info = polite[0]
                .live_region
                .as_ref()
                .expect("UIA LiveSetting is published");
            assert_eq!(info.politeness, Some(Politeness::Polite));
        } else {
            // Neither accesskit_unix nor accesskit_macos publishes it.
            assert!(polite.is_empty(), "{polite:?}");
        }
    }

    #[test]
    #[ignore]
    fn the_controls_relation_names_the_controlled_element() {
        let app = h::app_root();
        let button = one(&app, r#"button[name="Toggle Details"]"#);
        let controlled = one(&app, r#"[name="Details Text"]"#);
        // macOS reports aria-controls and aria-flowto together, as `linked`.
        let kind = if cfg!(target_os = "macos") {
            RelationKind::Linked
        } else {
            RelationKind::Controls
        };
        let relations = button.relations(&[kind]).expect("read the relation");
        assert_eq!(relations.len(), 1, "one entry per kind asked for");
        let (got_kind, targets) = &relations[0];
        assert_eq!(*got_kind, kind);
        assert_eq!(targets.len(), 1, "{targets:?}");
        assert!(
            targets[0].same_as(&controlled).expect("compare"),
            "the target must be the controlled node: {:?}",
            targets[0]
        );

        // A supported kind with no targets is an empty list, not absent.
        let none = controlled.relations(&[kind]).expect("read the relation");
        assert_eq!(none.len(), 1);
        assert_eq!(none[0].0, kind);
        assert!(none[0].1.is_empty(), "{:?}", none[0].1);

        // The kind this platform does not report is refused, never empty.
        let other = if cfg!(target_os = "macos") {
            RelationKind::Controls
        } else {
            RelationKind::Linked
        };
        match button.relations(&[other]) {
            Err(Error::Unsupported { .. }) => {}
            got => panic!("{other} must be unsupported here, got {got:?}"),
        }
    }
}
