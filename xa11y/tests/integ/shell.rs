//! Shell-surface integration tests — the end-to-end evidence that each
//! platform's classifier matches something real.
//!
//! Every other file in this suite drives the AccessKit test app. This one
//! drives the desktop the tests are running on: the surfaces are the OS's,
//! and the classifiers are three hand-written tables of magic constants —
//! window class names on Windows, AX attribute names on macOS, an AT-SPI
//! attribute on Linux.
//!
//! The rule the whole file is built around is that **an empty enumeration
//! fails**. Issue #383 is the reason it needs stating: the coverage that
//! existed for shell surfaces would have stayed green if
//! `list_shell_surfaces` had returned an empty vec on all three platforms,
//! because every assertion sat behind a guard that an empty listing
//! satisfied. [`REQUIRED_KINDS`] names, per platform, the surface the desktop
//! these tests run on *must* vend; anything less is a failure, never a skip.
//!
//! What makes each platform's requirement true:
//!
//! * **macOS** — `AXFocusedApplication` → `AXMenuBar`. Something is always
//!   frontmost in a live session, and an AppKit app always has a menu bar.
//! * **Windows** — the `Shell_TrayWnd` taskbar, which explorer.exe owns in
//!   every interactive session. The hosted `windows-latest` runner has one;
//!   that is the same session property the UIA event tests already rely on.
//! * **Linux** — nothing on a bare Xvfb display vends a `window-type:dock`
//!   frame, so `scripts/run_integ_tests.sh` launches one:
//!   `test-apps/panel/panel.py`, a GTK3 dock window. The fixture is part of
//!   the harness precisely so this requirement can be unconditional here.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[cfg(target_os = "macos")]
    use xa11y::AppExt;
    use xa11y::{Error, ShellSurface, ShellSurfaceExt, ShellSurfaceKind};

    /// The surface kinds the desktop under test must vend. See the module
    /// docs for why each one holds.
    #[cfg(target_os = "macos")]
    const REQUIRED_KINDS: &[ShellSurfaceKind] = &[ShellSurfaceKind::MenuBar];
    #[cfg(target_os = "windows")]
    const REQUIRED_KINDS: &[ShellSurfaceKind] = &[ShellSurfaceKind::Taskbar];
    #[cfg(target_os = "linux")]
    const REQUIRED_KINDS: &[ShellSurfaceKind] = &[ShellSurfaceKind::Panel];

    /// A kind this platform's backend cannot emit at all, used to check the
    /// miss path. Each backend classifies a fixed set — macOS never produces
    /// a taskbar, and neither Windows nor Linux produces a menu bar — so this
    /// is absent by construction rather than by whatever happens to be open.
    #[cfg(target_os = "macos")]
    const IMPOSSIBLE_KIND: ShellSurfaceKind = ShellSurfaceKind::Taskbar;
    #[cfg(not(target_os = "macos"))]
    const IMPOSSIBLE_KIND: ShellSurfaceKind = ShellSurfaceKind::MenuBar;

    /// How long to wait for a required surface. The shell is already up
    /// before the suite starts; the budget covers a panel or taskbar that is
    /// still registering with the accessibility bus.
    const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

    /// The listing, or a panic carrying the enumeration failure. Every test
    /// here starts from it, so it is never allowed to degrade to "no
    /// surfaces".
    fn listing() -> Vec<ShellSurface> {
        ShellSurface::list().unwrap_or_else(|e| panic!("enumerating shell surfaces failed: {e}"))
    }

    /// `kind "name" (pid=N)` for each surface — what a failure message owes
    /// the reader, since the desktop is not reproducible from the test file.
    fn describe(surfaces: &[ShellSurface]) -> String {
        if surfaces.is_empty() {
            return "<none>".to_string();
        }
        surfaces
            .iter()
            .map(|s| format!("{} \"{}\" (pid={:?})", s.kind, s.name, s.pid))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The load-bearing test: every kind this platform must vend is there.
    ///
    /// This is the one that fails when a classifier stops matching — a
    /// renamed window class, an AX attribute that moved, a `window-type`
    /// check that no longer fires. Nothing about it can pass on an empty
    /// listing.
    #[test]
    #[ignore]
    fn every_kind_this_desktop_must_vend_is_present() {
        let surfaces = listing();
        assert!(
            !surfaces.is_empty(),
            "no shell surfaces at all on a desktop that owns {:?}; the platform \
             classifier matched nothing",
            REQUIRED_KINDS
        );
        for &kind in REQUIRED_KINDS {
            let matching: Vec<&ShellSurface> = surfaces.iter().filter(|s| s.kind == kind).collect();
            assert_eq!(
                matching.len(),
                1,
                "expected exactly one {kind} surface, found {}. Listing: {}",
                matching.len(),
                describe(&surfaces)
            );
        }
    }

    /// `by_kind` resolves each required kind, and resolves it to the same
    /// surface the listing reported — the two entry points cannot disagree
    /// about what is on the desktop.
    #[test]
    #[ignore]
    fn by_kind_resolves_every_required_surface_the_listing_reports() {
        let surfaces = listing();
        for &kind in REQUIRED_KINDS {
            let resolved = ShellSurface::by_kind(kind, LOOKUP_TIMEOUT).unwrap_or_else(|e| {
                panic!(
                    "by_kind({kind}) failed: {e}. Listing: {}",
                    describe(&surfaces)
                )
            });
            let listed = surfaces
                .iter()
                .find(|s| s.kind == kind)
                .unwrap_or_else(|| panic!("{kind} resolved but is not in the listing"));
            assert_eq!(resolved.name, listed.name, "{kind} name");
            assert_eq!(resolved.pid, listed.pid, "{kind} pid");
        }
    }

    /// Every row a caller might target has to be usable as one: a name to
    /// recognise it by, an honest pid, and the kind stamped on the root so a
    /// bare `Element` carries it too.
    #[test]
    #[ignore]
    fn each_listed_surface_carries_the_fields_a_caller_targets_it_by() {
        let surfaces = listing();
        assert!(
            !surfaces.is_empty(),
            "the listing is empty, so this test would check nothing; \
             this desktop owes {REQUIRED_KINDS:?}"
        );

        for surface in &surfaces {
            assert!(
                !surface.name.is_empty(),
                "a surface a caller has to recognise needs a name: {surface:?}"
            );
            assert!(
                surface.pid.is_none_or(|pid| pid > 0),
                "pid must be absent or real, never 0: {surface:?}"
            );
            let root = surface.as_element();
            assert_eq!(
                root.raw.get("shell_kind").and_then(|v| v.as_str()),
                Some(surface.kind.to_snake_case()),
                "the surface root must carry its kind in `raw`: {surface:?}"
            );
        }
    }

    /// A surface root is an ordinary search root: it has children, it dumps,
    /// and a locator rooted at it resolves. This is the claim the feature
    /// exists for, asserted against a surface that must be there rather than
    /// whichever one happened to be listed.
    #[test]
    #[ignore]
    fn a_required_surface_root_searches_like_any_other_root() {
        for &kind in REQUIRED_KINDS {
            let surface = ShellSurface::by_kind(kind, LOOKUP_TIMEOUT)
                .unwrap_or_else(|e| panic!("by_kind({kind}) failed: {e}"));

            let children = surface
                .children()
                .unwrap_or_else(|e| panic!("{kind} children: {e}"));
            assert!(
                !children.is_empty(),
                "the {kind} surface root reports no children; \
                 the classifier matched a node with no tree under it"
            );

            let tree = surface
                .tree(Some(3))
                .unwrap_or_else(|e| panic!("{kind} tree: {e}"));
            assert!(
                !tree.children.is_empty(),
                "the {kind} tree snapshot has no children"
            );

            let dump = surface
                .dump(Some(2))
                .unwrap_or_else(|e| panic!("{kind} dump: {e}"));
            assert!(!dump.trim().is_empty(), "the {kind} dump is empty");

            let all = surface
                .locator("*")
                .elements()
                .unwrap_or_else(|e| panic!("{kind} locator(\"*\"): {e}"));
            assert!(
                !all.is_empty(),
                "a locator rooted at the {kind} surface matched nothing"
            );
        }
    }

    /// The miss path, on a kind the backend cannot produce: it must say what
    /// it was looking for and what it did see, not just that it failed
    /// (tenet 6).
    #[test]
    #[ignore]
    fn a_kind_this_platform_cannot_vend_reports_what_it_did_see() {
        let surfaces = listing();
        // ZERO: one attempt, no waiting — the kind is absent by construction.
        let err = ShellSurface::by_kind(IMPOSSIBLE_KIND, Duration::ZERO).expect_err(
            "this platform's backend cannot classify a surface as this kind, \
             so the lookup must fail",
        );
        let Error::SelectorNotMatched {
            selector,
            diagnosis,
        } = err
        else {
            panic!("expected SelectorNotMatched, got {err:?}");
        };
        assert_eq!(selector, format!("shell_surface[kind={IMPOSSIBLE_KIND}]"));

        let diagnosis = diagnosis.expect("a terminal miss carries a diagnosis");
        assert_eq!(
            diagnosis.condition.as_deref(),
            Some(format!("a {IMPOSSIBLE_KIND} shell surface").as_str())
        );
        let observed = diagnosis
            .last_observed
            .as_deref()
            .expect("the miss must report what it last observed");
        // The count itself is live — a flyout can open between the listing and
        // the lookup — so what is asserted is that the miss counted *something*
        // when the desktop had surfaces to count.
        assert!(
            observed.contains("other shell surface(s)"),
            "the miss must say how many other surfaces it saw: {observed}"
        );
        assert!(
            surfaces.is_empty() || !observed.contains("0 other shell surface(s)"),
            "the listing had {} surface(s) but the miss reported none: {observed}",
            surfaces.len()
        );
        // The candidate list is the way out of the miss: it must name the
        // surfaces this desktop *does* vend.
        let candidates = diagnosis.candidates.join(" ");
        for &kind in REQUIRED_KINDS {
            assert!(
                candidates.contains(kind.to_snake_case()),
                "the candidate list must name the {kind} surface that is present: \
                 {candidates:?}"
            );
        }
    }

    // ── Per-platform contents ────────────────────────────────────────────

    /// macOS: the menu bar surface is the frontmost application's, and its
    /// menus are reachable through it.
    ///
    /// The pid check is what proves `AXFocusedApplication` was followed
    /// rather than some other app's menu bar being picked up: the surface's
    /// owner must be the app the system reports as foreground.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn the_menu_bar_surface_holds_the_frontmost_apps_menus() {
        let menu_bar = ShellSurface::by_kind(ShellSurfaceKind::MenuBar, LOOKUP_TIMEOUT)
            .unwrap_or_else(|e| panic!("no menu bar surface: {e}"));

        let items = menu_bar
            .locator("menu_item")
            .elements()
            .unwrap_or_else(|e| panic!("menu bar items: {e}"));
        assert!(
            !items.is_empty(),
            "the menu bar surface holds no menu items; dump:\n{}",
            menu_bar.dump(Some(2)).unwrap_or_default()
        );

        let frontmost = xa11y::App::foreground(LOOKUP_TIMEOUT)
            .unwrap_or_else(|e| panic!("no foreground app: {e}"));
        // Both sides being `None` would satisfy the comparison below while
        // saying nothing, so the pid has to be real before it is compared.
        assert!(
            menu_bar.pid.is_some(),
            "the menu bar surface reports no owning process: {menu_bar:?}"
        );
        assert_eq!(
            menu_bar.pid, frontmost.pid,
            "the menu bar surface must belong to the frontmost app ({}), not {:?}",
            frontmost.name, menu_bar.name
        );
    }

    /// Windows: the taskbar surface reaches the shell's own controls.
    ///
    /// No assertion on a particular button's name — those move between
    /// Windows releases and locales — but a `Shell_TrayWnd` that yields no
    /// buttons at all means the class matched something that is not the
    /// taskbar, or that the surface root is not searchable.
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore]
    fn the_taskbar_surface_reaches_the_shells_own_buttons() {
        let taskbar = ShellSurface::by_kind(ShellSurfaceKind::Taskbar, LOOKUP_TIMEOUT)
            .unwrap_or_else(|e| panic!("no taskbar surface: {e}"));
        assert!(
            taskbar.pid.is_some_and(|pid| pid > 0),
            "the taskbar is hosted by a real process (explorer.exe): {taskbar:?}"
        );

        let buttons = taskbar
            .locator("button")
            .elements()
            .unwrap_or_else(|e| panic!("taskbar buttons: {e}"));
        assert!(
            !buttons.is_empty(),
            "the taskbar surface holds no buttons; dump:\n{}",
            taskbar.dump(Some(3)).unwrap_or_default()
        );
        assert!(
            buttons.iter().any(|b| b.name.is_some()),
            "every taskbar button is unnamed, so none is addressable by name: {:?}",
            buttons.iter().map(|b| &b.role).collect::<Vec<_>>()
        );
    }

    /// Linux: the panel surface is the harness's dock frame, and its widgets
    /// are reachable through it.
    ///
    /// The name check is deliberate — matching *this* frame proves the
    /// `window-type:dock` attribute was read off a real AT-SPI frame rather
    /// than some other top-level being mistaken for a panel.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn the_panel_surface_is_the_harnesss_dock_frame() {
        let panel =
            ShellSurface::by_kind(ShellSurfaceKind::Panel, LOOKUP_TIMEOUT).unwrap_or_else(|e| {
                panic!(
                    "no panel surface: {e}. scripts/run_integ_tests.sh launches \
                     test-apps/panel/panel.py for this test — is it running?"
                )
            });
        assert_eq!(
            panel.name, "xa11y-test-panel",
            "the panel surface is the harness's dock frame"
        );
        assert!(
            panel.pid.is_some_and(|pid| pid > 0),
            "the panel frame belongs to the panel process: {panel:?}"
        );

        let button = panel
            .locator("button[name=\"Panel Button\"]")
            .element()
            .unwrap_or_else(|e| {
                panic!(
                    "the panel's own button is not reachable from the surface root: {e}\n{}",
                    panel.dump(Some(3)).unwrap_or_default()
                )
            });
        assert_eq!(button.name.as_deref(), Some("Panel Button"));
        assert!(
            button.actions.iter().any(|a| a == "press"),
            "the panel button advertises no press action: {:?}",
            button.actions
        );
    }
}
