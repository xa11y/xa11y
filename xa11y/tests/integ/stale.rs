//! Actions on a destroyed GTK 3 widget fail as stale (issues #491, #586).
//!
//! GTK 3 keeps a destroyed widget's accessible answering D-Bus calls for
//! roughly 15–20 s, and the zombie reports writes as performed. Every Linux
//! action path therefore checks that the object is still attached to its
//! parent before the platform call. GTK 4 answers a destroyed object at once
//! and AccessKit removes nodes outright, so neither shows the bug; these
//! tests drive `test-apps/stale/stale.py`, a GTK 3 window that
//! `scripts/run_integ_tests.sh` launches next to the AccessKit test app.
//!
//! The fixture's "Rebuild" button destroys a button, an entry, a spin button
//! and a second top-level window, and builds fresh ones with the same names.
//! A handle read before the rebuild must fail every action as
//! [`Error::ElementStale`]; the same actions through a locator, which
//! re-resolves to the fresh widgets, are the control that keeps a check
//! calling *everything* stale from passing.
//!
//! Not covered: `press` / `toggle` / `select` / `expand` / `collapse` /
//! `show_menu` go through `DoAction`, whose check #578 added and
//! `tests/suites/python/test_actions.py` covers on GTK 4; `press` on the
//! doomed button is asserted here as well, for the GTK 3 zombie.

use std::time::{Duration, Instant};

use xa11y::*;

/// The fixture's AT-SPI application name (`APP_NAME` in `stale.py`).
const FIXTURE: &str = "xa11y-stale-fixture";

const REBUILD: &str = r#"button[name="Rebuild"]"#;
const BUTTON: &str = r#"button[name="Doomed button"]"#;
const ENTRY: &str = r#"text_field[name="Doomed entry"]"#;
const SPIN: &str = r#"spin_button[name="Doomed spin"]"#;
const WINDOW: &str = "Doomed window";

fn fixture() -> App {
    App::find(Duration::from_secs(5), |d| {
        d.name.as_deref() == Some(FIXTURE)
    })
    .unwrap_or_else(|e| {
        panic!(
            "the stale fixture ({FIXTURE}) is not running: scripts/run_integ_tests.sh \
             launches test-apps/stale/stale.py. {e}"
        )
    })
}

fn one(app: &App, selector: &str) -> Element {
    let found = app
        .locator(selector)
        .elements()
        .unwrap_or_else(|e| panic!("selector {selector:?} failed: {e}"));
    assert_eq!(
        found.len(),
        1,
        "selector {selector:?} matched {} elements, expected 1: {found:?}",
        found.len()
    );
    found.into_iter().next().expect("length checked above")
}

fn doomed_window(app: &App) -> Element {
    let windows = app.windows().expect("list the fixture's windows");
    let mut found: Vec<Element> = windows
        .into_iter()
        .filter(|w| w.name.as_deref() == Some(WINDOW))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected one {WINDOW:?} window, found {}: {found:?}",
        found.len()
    );
    found.pop().expect("length checked above")
}

/// Hands the foreground back to the AccessKit test app when dropped.
///
/// A rebuild maps a fresh "Doomed window", which the window manager
/// focuses, and the control test activates it. The `tree` tests that run
/// later expect the test app to be the foreground app, so every test here
/// restores it, even on panic.
struct RestoreForeground;

impl Drop for RestoreForeground {
    fn drop(&mut self) {
        // Not `integ::app_root()`: a panic in a drop during unwinding
        // aborts. The AccessKit app's AT-SPI name on Linux.
        let result = App::find(Duration::from_secs(2), |d| {
            d.name.as_deref() == Some("xa11y-test-app")
        })
        .and_then(|app| app.locator("window").element())
        .and_then(|window| window.activate());
        if let Err(e) = result {
            eprintln!("failed to reactivate the test app during cleanup: {e}");
        }
    }
}

/// Press "Rebuild" and wait until the fixture has swapped in fresh targets.
fn rebuild(app: &App) {
    let locator = app.locator(REBUILD);
    let before = locator
        .element()
        .expect("the Rebuild button is present")
        .description
        .clone();
    locator.press().expect("press Rebuild");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // The rebuild count is in the button's description.
        match locator.element() {
            Ok(el) if el.description != before => break,
            Ok(_) | Err(Error::SelectorNotMatched { .. }) => {}
            Err(e) => panic!("re-reading the Rebuild button failed: {e}"),
        }
        assert!(
            Instant::now() < deadline,
            "the fixture never rebuilt its targets (description stayed {before:?})"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // The fresh targets can trail the description by a moment.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let ready = [BUTTON, ENTRY, SPIN]
            .iter()
            .all(|s| matches!(app.locator(s).count(), Ok(1)))
            && matches!(
                app.windows().map(|ws| ws
                    .iter()
                    .filter(|w| w.name.as_deref() == Some(WINDOW))
                    .count()),
                Ok(1)
            );
        if ready {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the fixture's fresh targets never all appeared"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore]
fn every_action_on_a_destroyed_gtk3_widget_is_stale() {
    let _restore = RestoreForeground;
    let app = fixture();
    let button = one(&app, BUTTON);
    let entry = one(&app, ENTRY);
    let spin = one(&app, SPIN);
    let window = doomed_window(&app);

    rebuild(&app);

    // Well inside GTK 3's 15–20 s zombie window: every call below reaches a
    // destroyed widget whose accessible still answers.
    let outcomes: Vec<(&str, Result<()>)> = vec![
        ("button.press", button.press()),
        ("button.focus", button.focus()),
        ("button.scroll_into_view", button.scroll_into_view()),
        ("entry.focus", entry.focus()),
        ("entry.set_value", entry.set_value("written to a zombie")),
        ("entry.type_text", entry.type_text("typed into a zombie")),
        ("entry.select_text", entry.select_text(0, 2)),
        ("spin.set_numeric_value", spin.set_numeric_value(42.0)),
        ("spin.increment", spin.increment()),
        ("spin.decrement", spin.decrement()),
        ("window.activate", window.activate()),
    ];
    let wrong: Vec<String> = outcomes
        .iter()
        .filter(|(_, r)| !matches!(r, Err(Error::ElementStale { .. })))
        .map(|(name, r)| format!("{name}: {r:?}"))
        .collect();
    assert!(
        wrong.is_empty(),
        "these actions on destroyed GTK 3 widgets did not fail as stale:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
#[ignore]
fn actions_through_a_locator_reach_the_rebuilt_gtk3_widgets() {
    // The control for the test above: the attachment check must pass a live
    // widget, so the same paths succeed against the fresh targets.
    let _restore = RestoreForeground;
    let app = fixture();
    rebuild(&app);

    let entry = app.locator(ENTRY);
    entry.focus().expect("focus the fresh entry");
    entry
        .set_value("fresh")
        .expect("set_value on the fresh entry");
    entry.type_text("!").expect("type_text on the fresh entry");
    entry
        .select_text(0, 2)
        .expect("select_text on the fresh entry");

    let spin = app.locator(SPIN);
    spin.set_numeric_value(42.0)
        .expect("set_numeric_value on the fresh spin button");
    spin.increment().expect("increment the fresh spin button");
    spin.decrement().expect("decrement the fresh spin button");

    let button = app.locator(BUTTON);
    button.focus().expect("focus the fresh button");
    // GTK 3 may decline Component.ScrollTo (ActionNotSupported); what this
    // asserts is that a live widget is not called stale.
    match button.scroll_into_view() {
        Ok(()) | Err(Error::ActionNotSupported { .. }) => {}
        Err(e) => panic!("scroll_into_view on the fresh button: {e:?}"),
    }

    doomed_window(&app)
        .activate()
        .expect("activate the fresh Doomed window");
}
