//! `Locator::wait_stable` against a widget that really moves (issue #530).
//!
//! The test app's "Start slide" button restarts a three-second slide of the
//! "Slide target" button, pushing a tree update every 30 ms, so every
//! `wait_*` poll (100 ms apart) sees a different position until it stops.

#[cfg(test)]
mod tests {
    use crate::integ as h;
    use std::time::{Duration, Instant};
    use xa11y::*;

    const START: &str = r#"button[name="Start slide"]"#;
    const TARGET: &str = r#"button[name="Slide target"]"#;

    /// Bounds of the target once any slide has certainly finished.
    fn settled_bounds(app: &App) -> Rect {
        app.locator(TARGET)
            .wait_stable(Duration::from_secs(10))
            .expect("the slide target settles")
            .bounds
            .expect("the slide target has bounds")
    }

    #[test]
    #[ignore]
    fn wait_stable_returns_the_position_the_slide_ends_at() {
        let app = h::app_root();
        let rest = settled_bounds(&app);

        app.locator(START).press().expect("start the slide");
        let started = Instant::now();
        let el = app
            .locator(TARGET)
            .wait_stable(Duration::from_secs(10))
            .expect("the slide ends well inside the timeout");
        let waited = started.elapsed();

        // The slide ends where the target rests, so stable bounds equal the
        // resting ones. A wait that returned mid-slide would report an
        // earlier x.
        assert_eq!(
            el.bounds,
            Some(rest),
            "wait_stable returned before the slide ended (after {waited:?})"
        );
        assert!(
            waited >= Duration::from_millis(1500),
            "the slide takes 3 s, so the wait cannot have proven stability in {waited:?}"
        );
    }

    #[test]
    #[ignore]
    fn wait_stable_times_out_naming_the_moving_bounds() {
        let app = h::app_root();
        settled_bounds(&app);

        app.locator(START).press().expect("start the slide");
        let err = app
            .locator(TARGET)
            // 1.5 s leaves room for several polls where a resolve is slow
            // (UIA), and still ends well inside the 3 s slide.
            .wait_stable(Duration::from_millis(1500))
            .expect_err("a 3 s slide cannot settle within 1.5 s");
        let Error::Timeout { .. } = err else {
            panic!("expected a timeout, got {err:?}");
        };
        let d = err.diagnosis().expect("the timeout carries a diagnosis");
        let observed = d.last_observed.as_deref().unwrap_or_default();
        assert!(
            observed.starts_with("bounds still changing: {x="),
            "diagnosis should name the moving bounds: {observed}"
        );
        assert!(
            d.condition
                .as_deref()
                .is_some_and(|c| c.contains("two consecutive polls")),
            "{d:?}"
        );

        // Leave the app at rest for whatever test runs next.
        settled_bounds(&app);
    }

    #[test]
    #[ignore]
    fn wait_stable_on_a_still_element_returns_promptly() {
        let app = h::app_root();
        let started = Instant::now();
        let el = app
            .locator(r#"button[name="Submit"]"#)
            .wait_stable(Duration::from_secs(5))
            .expect("Submit never moves");
        assert!(el.bounds.is_some());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "two polls of a still element took {:?}",
            started.elapsed()
        );
    }
}
