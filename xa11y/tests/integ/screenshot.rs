//! Screenshot integration tests — capture pixels from the running test app
//! and verify the PNG round-trips through the decoder at the expected size.

#[cfg(test)]
mod tests {
    use crate::integ as h;

    #[test]
    #[ignore]
    fn capture_full_screen_yields_nonempty_png() {
        let shot = match xa11y::screenshotter()
            .expect("screenshotter construction")
            .capture()
        {
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

        let shot = match xa11y::screenshotter()
            .expect("screenshotter construction")
            .capture_element(&button)
        {
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
}
