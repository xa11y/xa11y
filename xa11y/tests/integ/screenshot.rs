//! Screenshot integration tests — capture pixels from the running test app
//! and verify the PNG round-trips through the decoder at the expected size.

#[cfg(test)]
mod tests {
    use crate::integ as h;

    #[test]
    #[ignore]
    fn capture_full_screen_yields_nonempty_png() {
        let shot = match xa11y::screenshot() {
            Ok(s) => s,
            // Disconnected RDP sessions / non-interactive CI jobs can't capture
            // the desktop; the backend surfaces that as Unsupported. Skip
            // rather than fail — the construction path is still exercised.
            Err(xa11y::Error::Unsupported { feature }) => {
                eprintln!("skipping: {feature}");
                return;
            }
            Err(e) => panic!("full-screen capture: {e}"),
        };
        assert!(shot.width > 0 && shot.height > 0, "empty capture dims");
        assert_eq!(
            shot.pixels.len(),
            (shot.width as usize) * (shot.height as usize) * 4
        );

        let bytes = shot.to_png().expect("PNG encode");
        assert!(bytes.len() > 100, "PNG unexpectedly small");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "missing PNG signature");
    }

    #[test]
    #[ignore]
    fn capture_element_matches_bounds_at_scale() {
        let app = h::app_root();
        // Any element with on-screen bounds works; Submit is a well-known
        // named button in the AccessKit test app.
        let button = h::named(&app, "Submit");

        // In headless CI (--headless winit), the test app has no on-screen
        // bounds. Skip the assertion part in that case — the full-screen
        // test in this module still validates the core pipeline.
        let Some(bounds) = button.bounds else {
            eprintln!("skipping: element has no bounds (likely headless)");
            return;
        };
        if bounds.width == 0 || bounds.height == 0 {
            eprintln!("skipping: element bounds are zero-sized");
            return;
        }

        let shot = match xa11y::screenshot_element(&button) {
            Ok(s) => s,
            Err(xa11y::Error::Unsupported { feature }) => {
                eprintln!("skipping: {feature}");
                return;
            }
            Err(e) => panic!("element capture: {e}"),
        };

        assert!(shot.scale > 0.0);
        let expected_w = (bounds.width as f32 * shot.scale).round() as u32;
        let expected_h = (bounds.height as f32 * shot.scale).round() as u32;
        // Allow 1px slack for rounding on fractional scale factors.
        assert!(
            (shot.width as i64 - expected_w as i64).abs() <= 1,
            "width {} not within 1 of expected {} (scale {})",
            shot.width,
            expected_w,
            shot.scale
        );
        assert!(
            (shot.height as i64 - expected_h as i64).abs() <= 1,
            "height {} not within 1 of expected {} (scale {})",
            shot.height,
            expected_h,
            shot.scale
        );

        // Round-trip through PNG.
        let bytes = shot.to_png().expect("PNG encode");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    /// Contract guard for issue #300: `Element::bounds` are **logical**
    /// coordinates and `Screenshot::scale` is the honest physical-to-logical
    /// ratio, so a region captured from those logical bounds must contain
    /// `bounds × scale` physical pixels.
    ///
    /// Under the old Windows model (physical bounds, `scale` hard-coded to
    /// 1.0) this invariant only held by accident at 100% DPI; on a scaled
    /// display the reported scale disagreed with the bounds. This test encodes
    /// the relationship directly. On 100%-DPI CI runners `scale == 1.0`, but
    /// the equation is still exercised end-to-end through every backend.
    #[test]
    #[ignore]
    fn region_from_logical_bounds_matches_scale() {
        let app = h::app_root();
        let button = h::named(&app, "Submit");

        let Some(bounds) = button.bounds else {
            eprintln!("skipping: element has no bounds (likely headless)");
            return;
        };
        if bounds.width == 0 || bounds.height == 0 {
            eprintln!("skipping: element bounds are zero-sized");
            return;
        }

        // Capture the exact logical rect (not via screenshot_element) to prove
        // that a caller passing logical bounds straight to screenshot_region
        // gets pixels back at the reported scale.
        let shot = match xa11y::screenshot_region(bounds) {
            Ok(s) => s,
            Err(xa11y::Error::Unsupported { feature }) => {
                eprintln!("skipping: {feature}");
                return;
            }
            Err(e) => panic!("region capture: {e}"),
        };

        assert!(
            shot.scale.is_finite() && shot.scale > 0.0,
            "scale must be a positive, finite ratio, got {}",
            shot.scale
        );
        let expected_w = (bounds.width as f32 * shot.scale).round() as i64;
        let expected_h = (bounds.height as f32 * shot.scale).round() as i64;
        assert!(
            (shot.width as i64 - expected_w).abs() <= 1,
            "region width {} != logical {} × scale {} (= {})",
            shot.width,
            bounds.width,
            shot.scale,
            expected_w
        );
        assert!(
            (shot.height as i64 - expected_h).abs() <= 1,
            "region height {} != logical {} × scale {} (= {})",
            shot.height,
            bounds.height,
            shot.scale,
            expected_h
        );
    }

    // ── Annotated captures ───────────────────────────────────────────────

    /// Skip helper shared by the annotation tests: a headless runner has no
    /// capture path at all, and the same `Unsupported` skip the plain capture
    /// tests use applies here.
    fn annotate_or_skip(
        region: Option<xa11y::Rect>,
        groups: &[xa11y::Locator],
    ) -> Option<xa11y::Annotated> {
        match xa11y::screenshot_annotated(region, groups) {
            Ok(a) => Some(a),
            Err(xa11y::Error::Unsupported { feature }) => {
                eprintln!("skipping: {feature}");
                None
            }
            Err(e) => panic!("annotated capture: {e}"),
        }
    }

    /// The round trip the feature exists for: every legend entry's selector
    /// must resolve, against the same scope the group's locator had, to the
    /// element that entry describes.
    #[test]
    #[ignore]
    fn legend_selectors_round_trip_against_the_same_scope() {
        let app = h::app_root();
        let buttons = app.locator("button").elements().expect("buttons");
        if buttons.is_empty() {
            eprintln!("skipping: the test app reports no buttons");
            return;
        }

        let Some(annotated) = annotate_or_skip(None, &[app.locator("button")]) else {
            return;
        };
        if annotated.legend.is_empty() {
            // Headless winit gives every element zero-sized or absent bounds,
            // so nothing is drawable. The omissions still have to be honest.
            assert_eq!(
                annotated.omitted.len() + annotated.truncated,
                buttons.len(),
                "every match must be either drawn, omitted, or counted as truncated"
            );
            eprintln!("skipping: no button has drawable bounds (likely headless)");
            return;
        }

        for entry in &annotated.legend {
            assert_eq!(entry.group, 1, "one --annotate is group 1");
            assert_eq!(
                entry.selector,
                format!("button:nth({})", entry.index),
                "the selector must be the nth argument spelled out"
            );
            assert_eq!(entry.tag, format!("A{}", entry.index));

            let resolved = app
                .locator(&entry.selector)
                .element()
                .unwrap_or_else(|e| panic!("{} must resolve: {e}", entry.selector));
            assert_eq!(
                resolved.name, entry.name,
                "{} resolved to a different element",
                entry.selector
            );
            assert_eq!(resolved.role.to_snake_case(), entry.role);
            assert_eq!(resolved.bounds, Some(entry.bounds));
        }
    }

    /// A named element the tree knows about must be findable in the legend by
    /// the same name, and its box must sit where the tree says it does.
    #[test]
    #[ignore]
    fn a_named_button_appears_in_the_legend_with_its_own_bounds() {
        let app = h::app_root();
        let submit = h::named(&app, "Submit");

        let Some(bounds) = submit.bounds else {
            eprintln!("skipping: element has no bounds (likely headless)");
            return;
        };
        if bounds.width == 0 || bounds.height == 0 {
            eprintln!("skipping: element bounds are zero-sized");
            return;
        }

        let Some(annotated) = annotate_or_skip(None, &[app.locator("button")]) else {
            return;
        };
        let entry = annotated
            .legend
            .iter()
            .find(|e| e.name.as_deref().is_some_and(|n| n.contains("Submit")));
        let Some(entry) = entry else {
            panic!(
                "Submit has bounds but no legend entry; omitted: {:?}",
                annotated.omitted
            );
        };
        assert_eq!(entry.bounds, bounds, "legend bounds are the tree's bounds");
        assert_eq!(entry.color, xa11y::screenshot::ANNOTATION_PALETTE[0]);
    }

    /// The image is still a PNG, still the size of the underlying capture, and
    /// visibly different from the same capture without annotations.
    #[test]
    #[ignore]
    fn an_annotated_capture_is_a_png_of_the_same_size_with_pixels_changed() {
        let app = h::app_root();
        let Some(annotated) = annotate_or_skip(None, &[app.locator("button")]) else {
            return;
        };
        let plain = match xa11y::screenshot() {
            Ok(s) => s,
            Err(xa11y::Error::Unsupported { feature }) => {
                eprintln!("skipping: {feature}");
                return;
            }
            Err(e) => panic!("plain capture: {e}"),
        };

        assert_eq!(annotated.screenshot.width, plain.width);
        assert_eq!(annotated.screenshot.height, plain.height);
        assert_eq!(
            annotated.screenshot.pixels.len(),
            (annotated.screenshot.width as usize) * (annotated.screenshot.height as usize) * 4
        );

        let bytes = annotated.screenshot.to_png().expect("PNG encode");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "missing PNG signature");

        if !annotated.legend.is_empty() {
            assert_ne!(
                annotated.screenshot.pixels, plain.pixels,
                "at least one box must have been drawn"
            );
        }
    }

    /// Two groups get two colours and two tag letters, and neither steals the
    /// other's numbering.
    #[test]
    #[ignore]
    fn two_groups_get_distinct_colours_and_letters() {
        let app = h::app_root();
        let Some(annotated) =
            annotate_or_skip(None, &[app.locator("button"), app.locator("text_field")])
        else {
            return;
        };
        if annotated.legend.is_empty() {
            eprintln!("skipping: nothing has drawable bounds (likely headless)");
            return;
        }

        for entry in &annotated.legend {
            assert!(entry.group == 1 || entry.group == 2, "{entry:?}");
            assert_eq!(
                entry.color,
                xa11y::screenshot::ANNOTATION_PALETTE[entry.group - 1]
            );
            let letter = if entry.group == 1 { "A" } else { "B" };
            assert_eq!(entry.tag, format!("{letter}{}", entry.index));
        }

        // Numbering is per group and 1-based, with no gaps.
        for group in [1_usize, 2] {
            let indices: Vec<usize> = annotated
                .legend
                .iter()
                .filter(|e| e.group == group)
                .map(|e| e.index)
                .collect();
            assert_eq!(
                indices,
                (1..=indices.len()).collect::<Vec<_>>(),
                "group {group} numbering"
            );
        }
    }

    /// A region that cannot contain the app's elements must report them as
    /// `outside_capture` rather than clamping boxes to the edge.
    #[test]
    #[ignore]
    fn elements_outside_an_explicit_region_are_reported_not_clamped() {
        let app = h::app_root();
        let submit = h::named(&app, "Submit");

        let Some(bounds) = submit.bounds else {
            eprintln!("skipping: element has no bounds (likely headless)");
            return;
        };
        if bounds.width == 0 || bounds.height == 0 {
            eprintln!("skipping: element bounds are zero-sized");
            return;
        }

        // A 1×1 region far from anything the app owns. `capture_region` may
        // legitimately refuse an off-display rect, in which case there is
        // nothing to assert.
        let region = xa11y::Rect {
            x: bounds.x,
            y: bounds.y,
            width: 1,
            height: 1,
        };
        let Some(annotated) = annotate_or_skip(Some(region), &[app.locator("button")]) else {
            return;
        };

        let drawn_or_reported = annotated.legend.len() + annotated.omitted.len();
        assert!(drawn_or_reported > 0, "every match must be accounted for");
        assert!(
            annotated
                .omitted
                .iter()
                .any(|o| o.reason == xa11y::OmissionReason::OutsideCapture),
            "a 1x1 region cannot contain every button; omitted: {:?}",
            annotated.omitted
        );
        for omission in &annotated.omitted {
            assert!(
                omission.selector.starts_with("button"),
                "an omitted element still carries a usable selector: {omission:?}"
            );
        }
    }

    /// A selector that matches nothing is not an error: the capture succeeds
    /// with an empty legend, so a caller can annotate speculatively.
    #[test]
    #[ignore]
    fn a_group_that_matches_nothing_yields_an_empty_legend() {
        let app = h::app_root();
        let Some(annotated) = annotate_or_skip(None, &[app.locator("progress_bar[name=\"nope\"]")])
        else {
            return;
        };
        assert!(annotated.legend.is_empty());
        assert!(annotated.omitted.is_empty());
        assert_eq!(annotated.truncated, 0);
        assert!(annotated.screenshot.width > 0);
    }

    /// A comma-separated alternation cannot produce a round-tripping
    /// `:nth(n)`, so it is refused before any capture is attempted.
    #[test]
    #[ignore]
    fn a_comma_separated_group_is_refused_with_the_fix_named() {
        let app = h::app_root();
        let err = xa11y::screenshot_annotated(None, &[app.locator("button, text_field")])
            .expect_err("an alternation has no round-tripping nth");
        match err {
            xa11y::Error::InvalidSelector { message, .. } => {
                assert!(
                    message.contains("one annotation group per clause"),
                    "{message}"
                );
            }
            other => panic!("expected InvalidSelector, got {other:?}"),
        }
    }
}
