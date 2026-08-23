//! OS shell surfaces — the taskbar, desktop panels, docks, menu bars, status
//! items, the desktop itself, and transient shell flyouts.
//!
//! Shell UI is already an ordinary part of the platform accessibility tree;
//! what is missing is a way to *find* it, because the per-platform root
//! filters behind [`Provider::list_apps`] drop it. This module adds exactly
//! that: a discovery primitive
//! ([`Provider::list_shell_surfaces`](crate::provider::Provider::list_shell_surfaces))
//! and a handle type ([`ShellSurface`]) shaped like [`App`](crate::App).
//! Selectors, locators, auto-wait, actions, `tree` / `dump`, [`Element`] and
//! [`Diagnosis`] are reused unchanged — every surface wraps a **real**
//! platform element, never a synthetic node xa11y invented.
//!
//! Enumerating surfaces and reading their trees never opens, closes, focuses,
//! or presses anything. Where content only exists after a press (the Windows
//! tray overflow, macOS Control Center), the caller performs that press on a
//! real, advertised element and then re-enumerates:
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use std::time::Duration;
//! # use xa11y_core::{Provider, ShellSurface, ShellSurfaceKind};
//! # fn demo(provider: Arc<dyn Provider>) -> xa11y_core::Result<()> {
//! let taskbar = ShellSurface::by_kind_with(
//!     Arc::clone(&provider),
//!     ShellSurfaceKind::Taskbar,
//!     Duration::ZERO,
//! )?;
//! taskbar.locator("button[name='Show Hidden Icons']").press()?;
//! // The overflow window materialises asynchronously — wait for it.
//! let flyout = ShellSurface::by_kind_with(
//!     provider,
//!     ShellSurfaceKind::Flyout,
//!     Duration::from_secs(3),
//! )?;
//! flyout.locator("button[name*='Sync']").press()?;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::app::LOOKUP_POLL_INTERVAL;
use crate::element::{Element, ElementData, TreeNode};
use crate::error::{Diagnosis, Error, Result};
use crate::locator::Locator;
use crate::provider::Provider;

/// Maximum number of shell surfaces listed in a lookup-failure diagnosis.
/// Bounded per tenet 6 — diagnostics must not grow with an unbounded
/// environment. Mirrors `DIAG_APP_LIST_LIMIT` in `app.rs`.
const DIAG_SURFACE_LIST_LIMIT: usize = 20;

/// Raw-attribute key carrying the surface kind on a surface root.
///
/// [`ShellSurface::list_with`] stamps it in one place — no backend can drift
/// on the spelling — and it lands on the **surface root only**, nowhere else
/// in the tree. A consumer reads it back through
/// [`as_element`](ShellSurface::as_element):
/// `surface.as_element().raw["shell_kind"]`.
///
/// It is deliberately *not* a way to find a surface. A rooted [`Locator`]
/// emits only descendants of its root, so `[shell_kind='taskbar']` never
/// matches the root that carries the stamp, and [`TreeNode`] carries no `raw`
/// map, so `tree` / `dump` output does not show it either. The kind travels in
/// [`ShellSurface::kind`] and in the
/// [`list_shell_surfaces`](crate::provider::Provider::list_shell_surfaces)
/// signature; the stamp is the same fact, readable from a bare [`Element`].
const SHELL_KIND_RAW_KEY: &str = "shell_kind";

/// What kind of OS shell surface a [`ShellSurface`] is.
///
/// `#[non_exhaustive]`: backends map platform → kind, never the reverse (the
/// same direction as [`Role`](crate::Role)), and the set grows as more shell
/// UI is classified — Start menu, jump lists, secondary taskbars, widget
/// boards.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::EnumString,
    strum::IntoStaticStr,
)]
// Test-only: `EnumIter` is what lets `every_variant_is_in_all` compare
// [`ALL`](Self::ALL) against the real variant set instead of against a
// hand-counted length. Gated so the generated iterator type stays out of the
// public API (and out of the bindings-parity surface).
#[cfg_attr(test, derive(strum::EnumIter))]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ShellSurfaceKind {
    /// The system menu bar: the frontmost application's menus, Apple menu
    /// included. macOS only — Windows and Linux have no equivalent object.
    MenuBar,
    /// One process's status items (`AXExtrasMenuBar`). One surface per owning
    /// process; macOS only. The Windows tray has no per-app hosting, so tray
    /// icons live inside the [`Taskbar`](Self::Taskbar) surface instead.
    StatusItems,
    /// The Windows taskbar (`Shell_TrayWnd`), including the task band, the
    /// visible tray row, and the overflow chevron.
    Taskbar,
    /// A Linux desktop panel or dock: an AT-SPI frame carrying the
    /// `window-type:dock` attribute. One surface per frame.
    Panel,
    /// The macOS Dock.
    Dock,
    /// The desktop icon surface: `Progman`'s list view on Windows, Finder's
    /// desktop scroll area on macOS.
    Desktop,
    /// A transient shell window that exists only while open: the tray
    /// overflow flyout, Quick Settings, Notification Center, a shell
    /// context-menu popup, an opened Control Center panel.
    Flyout,
    /// A shell-owned window the backend could not classify. The documented
    /// fallback, like [`Role::Unknown`](crate::Role::Unknown) — present so a
    /// new OS surface degrades to "reachable but untagged" rather than
    /// invisible.
    Unknown,
}

impl ShellSurfaceKind {
    /// Every kind, in the order the CLI, MCP and both bindings advertise them.
    ///
    /// The single source for every list of kind spellings outside this crate:
    /// the CLI's `--shell` help and error message, MCP's `shell` enum, and
    /// each binding's parse-failure message all derive from
    /// `ALL` + [`to_snake_case`](Self::to_snake_case) rather than writing the
    /// strings out again. `ShellSurfaceKind` is `#[non_exhaustive]`, so a
    /// downstream `match` cannot be the thing that fails when a variant is
    /// added; `every_variant_is_in_all` in this module's tests is. It matches
    /// exhaustively (legal in the defining crate, so a new variant is a
    /// compile error there) and compares this list against strum's derived
    /// iterator, so `ALL` cannot quietly fall one kind behind the enum.
    pub const ALL: &'static [ShellSurfaceKind] = &[
        ShellSurfaceKind::MenuBar,
        ShellSurfaceKind::StatusItems,
        ShellSurfaceKind::Taskbar,
        ShellSurfaceKind::Panel,
        ShellSurfaceKind::Dock,
        ShellSurfaceKind::Desktop,
        ShellSurfaceKind::Flyout,
        ShellSurfaceKind::Unknown,
    ];

    /// Parse a snake_case kind name into a `ShellSurfaceKind` variant.
    /// Returns `None` if the name doesn't match any known kind.
    pub fn from_snake_case(s: &str) -> Option<Self> {
        s.parse::<ShellSurfaceKind>().ok()
    }

    /// Convert a `ShellSurfaceKind` to its snake_case string representation.
    ///
    /// This is the spelling the kind carries across every surface: the
    /// bindings, the CLI's `--shell` flag, MCP's `shell` parameter, and the
    /// `shell_kind` raw attribute on the surface root.
    pub fn to_snake_case(self) -> &'static str {
        self.into()
    }
}

impl std::fmt::Display for ShellSurfaceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_snake_case())
    }
}

/// Bounded `kind "name" (pid=N)` rendering of the surfaces that *were* found —
/// the candidate list a consumer would otherwise produce by hand-logging
/// `ShellSurface::list()` around the failure. Mirrors `running_apps_diagnosis`
/// in `app.rs`.
fn surface_candidates(surfaces: &[ShellSurface]) -> Vec<String> {
    let total = surfaces.len();
    let mut out: Vec<String> = surfaces
        .iter()
        .take(DIAG_SURFACE_LIST_LIMIT)
        .map(|s| {
            let pid = s.pid.map(|p| format!(" (pid={p})")).unwrap_or_default();
            format!("{} \"{}\"{pid}", s.kind, s.name)
        })
        .collect();
    if total > DIAG_SURFACE_LIST_LIMIT {
        out.push(format!("… (+{} more)", total - DIAG_SURFACE_LIST_LIMIT));
    }
    out
}

/// A shell surface: one OS-owned top-level accessibility root, tagged with
/// what it is. The entry point for shell queries, as [`App`](crate::App) is
/// for application queries.
///
/// `ShellSurface` is **not** an [`Element`] — it represents the surface as a
/// whole and provides a [`locator`](ShellSurface::locator) to search its
/// accessibility tree.
pub struct ShellSurface {
    /// What this surface is.
    pub kind: ShellSurfaceKind,
    /// Human-readable name: the owning app for per-app surfaces ("Safari"
    /// menu bar, "Arq" status items), the platform's own name otherwise
    /// ("Taskbar", "Dock"). Falls back to the kind's snake_case spelling when
    /// the platform vends no name for the root.
    pub name: String,
    /// Owning process where the platform reports one honestly. On macOS this
    /// is always the true owner. On Windows it is the *host* (explorer.exe /
    /// ShellHost.exe) because UIA carries no per-icon owner — documented as
    /// the host, never faked. On Linux it is the panel process.
    pub pid: Option<u32>,
    /// The surface's root element data.
    pub data: ElementData,
    provider: Arc<dyn Provider>,
}

impl ShellSurface {
    /// List the OS shell surfaces currently on screen, using an explicit
    /// provider.
    ///
    /// Prefer `ShellSurface::list` from the `xa11y` crate which uses the
    /// global singleton provider. Use this variant when you need to supply a
    /// specific provider (e.g. a mock in unit tests).
    ///
    /// The listing is live: [`Flyout`](ShellSurfaceKind::Flyout) surfaces
    /// appear only while they are open, and enumerating never opens, closes,
    /// or presses anything. A platform with no surface of a given kind simply
    /// returns none — that is honest scope, not a failure.
    pub fn list_with(provider: Arc<dyn Provider>) -> Result<Vec<Self>> {
        let entries = provider.list_shell_surfaces()?;
        Ok(entries
            .into_iter()
            .map(|(kind, mut data)| {
                // The kind is stamped onto the root's raw map HERE — the one
                // place — rather than in each backend, so no backend can drift
                // on the spelling. It lands on the surface root only; see
                // `SHELL_KIND_RAW_KEY` for what that does and does not buy.
                data.raw.insert(
                    SHELL_KIND_RAW_KEY.to_string(),
                    serde_json::Value::String(kind.to_snake_case().to_string()),
                );
                // Platform roots are not always named (a `Progman` list view
                // has no name of its own); the kind is the honest fallback,
                // and it is what the caller asked for by name anyway.
                let name = data
                    .name
                    .clone()
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| kind.to_snake_case().to_string());
                let pid = data.pid;
                Self {
                    kind,
                    name,
                    pid,
                    data,
                    provider: Arc::clone(&provider),
                }
            })
            .collect())
    }

    /// Wait for **exactly one** surface of `kind`, using an explicit provider.
    ///
    /// Prefer `ShellSurface::by_kind` from the `xa11y` crate which uses the
    /// global singleton provider.
    ///
    /// Polls [`list_with`](Self::list_with) until a single surface of `kind`
    /// exists or `timeout` elapses; `Duration::ZERO` performs exactly one
    /// attempt (no waiting). An enumeration failure short-circuits — "the
    /// shell cannot be listed" is not "the surface isn't up yet". The wait is
    /// what makes the flyout workflow a one-liner: press the tray chevron,
    /// then wait for the overflow window to materialise.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SelectorNotMatched`] both when no surface of `kind`
    /// is present and when **several** are — a second `Panel` frame, status
    /// items from several processes, a leftover `Flyout`. Ambiguity is
    /// refused rather than first-matched: for a surface whose consumers are
    /// mostly agents, silently acting on one of several indistinguishable
    /// candidates is the failure mode worth ruling out. The refusal is
    /// terminal, not a retry — the diagnosis lists the candidates so the
    /// caller can disambiguate with [`list_with`](Self::list_with) and a pid.
    pub fn by_kind_with(
        provider: Arc<dyn Provider>,
        kind: ShellSurfaceKind,
        timeout: Duration,
    ) -> Result<Self> {
        // The selector-shaped string the failures report. Shell surfaces are
        // not addressable by selector, but the lookup's error reads like every
        // other not-found in the API.
        let selector = format!("shell_surface[kind={kind}]");
        let start = Instant::now();
        loop {
            // Split rather than filter: the surfaces of other kinds are the
            // candidate list a "no such surface" failure owes the caller.
            let mut matched: Vec<Self> = Vec::new();
            let mut others: Vec<Self> = Vec::new();
            for surface in Self::list_with(Arc::clone(&provider))? {
                if surface.kind == kind {
                    matched.push(surface);
                } else {
                    others.push(surface);
                }
            }

            if matched.len() > 1 {
                // Terminal: waiting cannot make an ambiguous shell less
                // ambiguous, and returning the first would hide which of the
                // candidates the caller actually got.
                return Err(Error::selector_not_matched(selector).diagnose(
                    Diagnosis::new()
                        .condition(format!("exactly one {kind} shell surface"))
                        .last_observed(format!(
                            "{} {kind} surfaces are present; disambiguate with \
                             ShellSurface::list() and pick by pid",
                            matched.len()
                        ))
                        .candidates(surface_candidates(&matched)),
                ));
            }
            if let Some(surface) = matched.pop() {
                return Ok(surface);
            }
            if start.elapsed() >= timeout {
                return Err(Error::selector_not_matched(selector).diagnose(
                    Diagnosis::new()
                        .condition(format!("a {kind} shell surface"))
                        .last_observed(format!(
                            "no {kind} surface present; {} other shell surface(s) enumerated",
                            others.len()
                        ))
                        .candidates(surface_candidates(&others)),
                ));
            }
            std::thread::sleep(LOOKUP_POLL_INTERVAL);
        }
    }

    /// Create a [`Locator`] to search this surface's accessibility tree.
    pub fn locator(&self, selector: &str) -> Locator {
        Locator::new(
            Arc::clone(&self.provider),
            Some(self.data.clone()),
            selector,
        )
    }

    /// Get direct children of the surface root.
    pub fn children(&self) -> Result<Vec<Element>> {
        let children = self.provider.get_children(Some(&self.data))?;
        Ok(children
            .into_iter()
            .map(|d| Element::new(d, Arc::clone(&self.provider)))
            .collect())
    }

    /// Capture the surface's accessibility tree as a recursive snapshot,
    /// rooted at the surface element.
    ///
    /// Equivalent to `self.as_element().tree(max_depth)`. See
    /// [`Element::tree`] for `max_depth` semantics.
    pub fn tree(&self, max_depth: Option<usize>) -> Result<TreeNode> {
        self.as_element().tree(max_depth)
    }

    /// Render the surface's accessibility tree as an indented string, rooted
    /// at the surface element.
    ///
    /// The primary inspection helper for figuring out the role/name of every
    /// element in a shell surface before writing selectors. Equivalent to
    /// `self.as_element().dump(max_depth)`. See [`Element::dump`] for the
    /// output format.
    pub fn dump(&self, max_depth: Option<usize>) -> Result<String> {
        self.as_element().dump(max_depth)
    }

    /// Get an [`Element`] handle for the surface root.
    ///
    /// Useful when you want to use Element-level methods (e.g. `tree`,
    /// `dump`, `children`) without going through a locator.
    pub fn as_element(&self) -> Element {
        Element::new(self.data.clone(), Arc::clone(&self.provider))
    }
}

impl std::fmt::Display for ShellSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} \"{}\"", self.kind, self.name)
    }
}

impl std::fmt::Debug for ShellSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellSurface")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("pid", &self.pid)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_provider::Subscription;
    use crate::mock::{build_provider, MockProvider, MOCK_SHELL_PID};
    use crate::role::Role;

    fn surfaces() -> Vec<ShellSurface> {
        let provider: Arc<dyn Provider> = build_provider();
        ShellSurface::list_with(provider).expect("the mock must list its shell surfaces")
    }

    /// Wraps the standard mock provider but reports the taskbar surface twice,
    /// modelling the ambiguous shell (two `Panel` frames, status items from
    /// several processes, a leftover flyout). Everything else delegates.
    struct DuplicateSurfaceProvider {
        inner: Arc<MockProvider>,
    }

    impl DuplicateSurfaceProvider {
        fn new() -> Self {
            Self {
                inner: build_provider(),
            }
        }
    }

    impl Provider for DuplicateSurfaceProvider {
        fn list_shell_surfaces(&self) -> Result<Vec<(ShellSurfaceKind, ElementData)>> {
            let mut all = self.inner.list_shell_surfaces()?;
            let dup = all
                .iter()
                .find(|(k, _)| *k == ShellSurfaceKind::Taskbar)
                .cloned()
                .expect("the mock fixture must contain a taskbar surface");
            all.push(dup);
            Ok(all)
        }
        fn get_children(&self, e: Option<&ElementData>) -> Result<Vec<ElementData>> {
            self.inner.get_children(e)
        }
        fn get_parent(&self, e: &ElementData) -> Result<Option<ElementData>> {
            self.inner.get_parent(e)
        }
        fn list_apps(&self) -> Result<Vec<ElementData>> {
            self.inner.list_apps()
        }
        fn focused_app(&self) -> Result<ElementData> {
            self.inner.focused_app()
        }
        fn press(&self, e: &ElementData) -> Result<()> {
            self.inner.press(e)
        }
        fn focus(&self, e: &ElementData) -> Result<()> {
            self.inner.focus(e)
        }
        fn blur(&self, e: &ElementData) -> Result<()> {
            self.inner.blur(e)
        }
        fn toggle(&self, e: &ElementData) -> Result<()> {
            self.inner.toggle(e)
        }
        fn select(&self, e: &ElementData) -> Result<()> {
            self.inner.select(e)
        }
        fn expand(&self, e: &ElementData) -> Result<()> {
            self.inner.expand(e)
        }
        fn collapse(&self, e: &ElementData) -> Result<()> {
            self.inner.collapse(e)
        }
        fn show_menu(&self, e: &ElementData) -> Result<()> {
            self.inner.show_menu(e)
        }
        fn increment(&self, e: &ElementData) -> Result<()> {
            self.inner.increment(e)
        }
        fn decrement(&self, e: &ElementData) -> Result<()> {
            self.inner.decrement(e)
        }
        fn scroll_into_view(&self, e: &ElementData) -> Result<()> {
            self.inner.scroll_into_view(e)
        }
        fn set_value(&self, e: &ElementData, v: &str) -> Result<()> {
            self.inner.set_value(e, v)
        }
        fn set_numeric_value(&self, e: &ElementData, v: f64) -> Result<()> {
            self.inner.set_numeric_value(e, v)
        }
        fn type_text(&self, e: &ElementData, t: &str) -> Result<()> {
            self.inner.type_text(e, t)
        }
        fn set_text_selection(&self, e: &ElementData, s: u32, end: u32) -> Result<()> {
            self.inner.set_text_selection(e, s, end)
        }
        fn perform_action(&self, e: &ElementData, a: &str) -> Result<()> {
            self.inner.perform_action(e, a)
        }
        fn subscribe(&self, e: &ElementData) -> Result<Subscription> {
            self.inner.subscribe(e)
        }
    }

    /// Provider whose shell enumeration fails outright — the "accessibility is
    /// broken" case, which must never be mistaken for "no surface yet".
    struct BrokenShellProvider {
        inner: Arc<MockProvider>,
    }

    impl Provider for BrokenShellProvider {
        fn list_shell_surfaces(&self) -> Result<Vec<(ShellSurfaceKind, ElementData)>> {
            Err(Error::Platform {
                code: 55,
                message: "shell enumeration failed".to_string(),
            })
        }
        fn get_children(&self, e: Option<&ElementData>) -> Result<Vec<ElementData>> {
            self.inner.get_children(e)
        }
        fn get_parent(&self, e: &ElementData) -> Result<Option<ElementData>> {
            self.inner.get_parent(e)
        }
        fn list_apps(&self) -> Result<Vec<ElementData>> {
            self.inner.list_apps()
        }
        fn focused_app(&self) -> Result<ElementData> {
            self.inner.focused_app()
        }
        fn press(&self, e: &ElementData) -> Result<()> {
            self.inner.press(e)
        }
        fn focus(&self, e: &ElementData) -> Result<()> {
            self.inner.focus(e)
        }
        fn blur(&self, e: &ElementData) -> Result<()> {
            self.inner.blur(e)
        }
        fn toggle(&self, e: &ElementData) -> Result<()> {
            self.inner.toggle(e)
        }
        fn select(&self, e: &ElementData) -> Result<()> {
            self.inner.select(e)
        }
        fn expand(&self, e: &ElementData) -> Result<()> {
            self.inner.expand(e)
        }
        fn collapse(&self, e: &ElementData) -> Result<()> {
            self.inner.collapse(e)
        }
        fn show_menu(&self, e: &ElementData) -> Result<()> {
            self.inner.show_menu(e)
        }
        fn increment(&self, e: &ElementData) -> Result<()> {
            self.inner.increment(e)
        }
        fn decrement(&self, e: &ElementData) -> Result<()> {
            self.inner.decrement(e)
        }
        fn scroll_into_view(&self, e: &ElementData) -> Result<()> {
            self.inner.scroll_into_view(e)
        }
        fn set_value(&self, e: &ElementData, v: &str) -> Result<()> {
            self.inner.set_value(e, v)
        }
        fn set_numeric_value(&self, e: &ElementData, v: f64) -> Result<()> {
            self.inner.set_numeric_value(e, v)
        }
        fn type_text(&self, e: &ElementData, t: &str) -> Result<()> {
            self.inner.type_text(e, t)
        }
        fn set_text_selection(&self, e: &ElementData, s: u32, end: u32) -> Result<()> {
            self.inner.set_text_selection(e, s, end)
        }
        fn perform_action(&self, e: &ElementData, a: &str) -> Result<()> {
            self.inner.perform_action(e, a)
        }
        fn subscribe(&self, e: &ElementData) -> Result<Subscription> {
            self.inner.subscribe(e)
        }
    }

    /// `ShellSurfaceKind::ALL` must list every variant.
    ///
    /// This is the guard `#[non_exhaustive]` took away: outside this crate a
    /// `match` needs a `_` arm, so nothing downstream fails to compile when a
    /// variant is added. Inside the defining crate an exhaustive `match` is
    /// still legal — so adding a variant without adding it to `ALL` is a
    /// **compile error here**, and every derived list (the CLI's `--shell`
    /// help and error text, MCP's `shell` enum, both bindings' parse errors)
    /// picks the new kind up for free.
    #[test]
    fn every_variant_is_in_all() {
        use strum::IntoEnumIterator;

        // The derived iterator is the second, independent enumeration of the
        // variants: `ALL` cannot silently fall behind it, because adding a
        // variant changes what `iter()` yields whether or not anyone edits
        // this file.
        let declared: Vec<ShellSurfaceKind> = ShellSurfaceKind::iter().collect();
        assert_eq!(
            ShellSurfaceKind::ALL,
            declared.as_slice(),
            "ShellSurfaceKind::ALL must list every variant, in declaration order — \
             every advertised kind list (the CLI's --shell help and error text, MCP's \
             `shell` enum, both bindings' parse errors) is derived from it"
        );

        // And the exhaustive `match` that makes a new variant a compile error
        // *here*, so the author is sent to this test rather than shipping a
        // kind nothing advertises. No `_` arm, on purpose.
        for kind in ShellSurfaceKind::ALL {
            let named = match kind {
                ShellSurfaceKind::MenuBar => "menu_bar",
                ShellSurfaceKind::StatusItems => "status_items",
                ShellSurfaceKind::Taskbar => "taskbar",
                ShellSurfaceKind::Panel => "panel",
                ShellSurfaceKind::Dock => "dock",
                ShellSurfaceKind::Desktop => "desktop",
                ShellSurfaceKind::Flyout => "flyout",
                ShellSurfaceKind::Unknown => "unknown",
            };
            assert_eq!(kind.to_snake_case(), named);
        }

        // No duplicate spellings: `ALL` is what every advertised list, and
        // every JSON Schema `enum`, is built from.
        let unique: std::collections::BTreeSet<&str> = ShellSurfaceKind::ALL
            .iter()
            .map(|k| k.to_snake_case())
            .collect();
        assert_eq!(unique.len(), ShellSurfaceKind::ALL.len());
    }

    #[test]
    fn all_kinds_roundtrip() {
        // Every kind must parse back from its own snake_case representation —
        // that string is what crosses the bindings, the CLI and MCP.
        for &kind in ShellSurfaceKind::ALL {
            let s = kind.to_snake_case();
            assert_eq!(
                ShellSurfaceKind::from_snake_case(s),
                Some(kind),
                "roundtrip failed for {s}"
            );
            assert_eq!(format!("{kind}"), s);
        }
        assert_eq!(
            ShellSurfaceKind::StatusItems.to_snake_case(),
            "status_items"
        );
        assert_eq!(ShellSurfaceKind::from_snake_case("not_a_kind"), None);
    }

    #[test]
    fn list_with_returns_the_mock_fixture_surfaces() {
        let surfaces = surfaces();
        let kinds: Vec<ShellSurfaceKind> = surfaces.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![ShellSurfaceKind::Taskbar, ShellSurfaceKind::Desktop]
        );
        assert_eq!(surfaces[0].name, "Taskbar");
        assert_eq!(surfaces[0].pid, Some(MOCK_SHELL_PID));
    }

    #[test]
    fn list_with_stamps_the_kind_onto_the_root() {
        // The stamp lands on the surface root, and `as_element().raw` is
        // where a consumer reads it back — the only place it is visible.
        let surfaces = surfaces();
        for surface in &surfaces {
            assert_eq!(
                surface.data.raw.get(SHELL_KIND_RAW_KEY),
                Some(&serde_json::Value::String(
                    surface.kind.to_snake_case().to_string()
                )),
                "{surface} must carry its kind in raw"
            );
        }
    }

    #[test]
    fn list_with_falls_back_to_the_kind_for_an_unnamed_root() {
        // A platform root without a name of its own (Progman's list view)
        // still needs a name; the kind is the honest one.
        struct UnnamedRootProvider {
            inner: Arc<MockProvider>,
        }
        impl Provider for UnnamedRootProvider {
            fn list_shell_surfaces(&self) -> Result<Vec<(ShellSurfaceKind, ElementData)>> {
                let mut all = self.inner.list_shell_surfaces()?;
                for (_, data) in all.iter_mut() {
                    data.name = None;
                }
                Ok(all)
            }
            fn get_children(&self, e: Option<&ElementData>) -> Result<Vec<ElementData>> {
                self.inner.get_children(e)
            }
            fn get_parent(&self, e: &ElementData) -> Result<Option<ElementData>> {
                self.inner.get_parent(e)
            }
            fn list_apps(&self) -> Result<Vec<ElementData>> {
                self.inner.list_apps()
            }
            fn focused_app(&self) -> Result<ElementData> {
                self.inner.focused_app()
            }
            fn press(&self, e: &ElementData) -> Result<()> {
                self.inner.press(e)
            }
            fn focus(&self, e: &ElementData) -> Result<()> {
                self.inner.focus(e)
            }
            fn blur(&self, e: &ElementData) -> Result<()> {
                self.inner.blur(e)
            }
            fn toggle(&self, e: &ElementData) -> Result<()> {
                self.inner.toggle(e)
            }
            fn select(&self, e: &ElementData) -> Result<()> {
                self.inner.select(e)
            }
            fn expand(&self, e: &ElementData) -> Result<()> {
                self.inner.expand(e)
            }
            fn collapse(&self, e: &ElementData) -> Result<()> {
                self.inner.collapse(e)
            }
            fn show_menu(&self, e: &ElementData) -> Result<()> {
                self.inner.show_menu(e)
            }
            fn increment(&self, e: &ElementData) -> Result<()> {
                self.inner.increment(e)
            }
            fn decrement(&self, e: &ElementData) -> Result<()> {
                self.inner.decrement(e)
            }
            fn scroll_into_view(&self, e: &ElementData) -> Result<()> {
                self.inner.scroll_into_view(e)
            }
            fn set_value(&self, e: &ElementData, v: &str) -> Result<()> {
                self.inner.set_value(e, v)
            }
            fn set_numeric_value(&self, e: &ElementData, v: f64) -> Result<()> {
                self.inner.set_numeric_value(e, v)
            }
            fn type_text(&self, e: &ElementData, t: &str) -> Result<()> {
                self.inner.type_text(e, t)
            }
            fn set_text_selection(&self, e: &ElementData, s: u32, end: u32) -> Result<()> {
                self.inner.set_text_selection(e, s, end)
            }
            fn perform_action(&self, e: &ElementData, a: &str) -> Result<()> {
                self.inner.perform_action(e, a)
            }
            fn subscribe(&self, e: &ElementData) -> Result<Subscription> {
                self.inner.subscribe(e)
            }
        }

        let provider: Arc<dyn Provider> = Arc::new(UnnamedRootProvider {
            inner: build_provider(),
        });
        let surfaces = ShellSurface::list_with(provider).expect("list must succeed");
        assert_eq!(surfaces[0].name, "taskbar");
    }

    #[test]
    fn list_with_propagates_enumeration_failures() {
        // Tenet 1: a broken shell enumeration surfaces, it does not become an
        // empty list.
        let provider: Arc<dyn Provider> = Arc::new(BrokenShellProvider {
            inner: build_provider(),
        });
        let err = ShellSurface::list_with(provider).expect_err("enumeration failure must surface");
        assert!(matches!(err, Error::Platform { code: 55, .. }));
    }

    #[test]
    fn locator_is_rooted_at_the_surface() {
        let provider: Arc<dyn Provider> = build_provider();
        let taskbar =
            ShellSurface::by_kind_with(provider, ShellSurfaceKind::Taskbar, Duration::ZERO)
                .expect("the mock must vend a taskbar");
        let el = taskbar
            .locator("button[name='Show Hidden Icons']")
            .element()
            .expect("the taskbar's overflow chevron must be reachable from the surface root");
        assert_eq!(el.data().role, Role::Button);
        // The app tree is NOT in scope: the surface root is its own subtree.
        assert!(matches!(
            taskbar.locator("button[name='Back']").element(),
            Err(Error::SelectorNotMatched { .. })
        ));
    }

    #[test]
    fn surface_tree_and_dump_are_rooted_at_the_surface() {
        let surfaces = surfaces();
        let node = surfaces[0].tree(None).expect("tree must succeed");
        assert_eq!(node.name.as_deref(), Some("Taskbar"));
        assert_eq!(node.children.len(), 2);
        let dump = surfaces[0].dump(None).expect("dump must succeed");
        assert!(
            dump.contains("Show Hidden Icons"),
            "dump must render the surface subtree: {dump}"
        );
    }

    #[test]
    fn children_and_as_element_expose_the_surface_root() {
        let surfaces = surfaces();
        let el = surfaces[0].as_element();
        assert_eq!(el.data().name.as_deref(), Some("Taskbar"));
        let children = surfaces[0].children().expect("children must succeed");
        let names: Vec<Option<&str>> = children.iter().map(|c| c.data().name.as_deref()).collect();
        assert_eq!(names, vec![Some("Show Hidden Icons"), Some("Volume")]);
    }

    #[test]
    fn by_kind_with_resolves_a_unique_surface() {
        let provider: Arc<dyn Provider> = build_provider();
        let desktop =
            ShellSurface::by_kind_with(provider, ShellSurfaceKind::Desktop, Duration::ZERO)
                .expect("the mock must vend a desktop surface");
        assert_eq!(desktop.kind, ShellSurfaceKind::Desktop);
        assert_eq!(desktop.name, "Desktop");
    }

    #[test]
    fn by_kind_with_reports_the_surfaces_that_were_present() {
        // Tenet 6: a "no such surface" failure names what *was* there.
        let provider: Arc<dyn Provider> = build_provider();
        let err = ShellSurface::by_kind_with(provider, ShellSurfaceKind::Dock, Duration::ZERO)
            .expect_err("the mock has no dock surface");
        let Error::SelectorNotMatched { selector, .. } = &err else {
            panic!("expected SelectorNotMatched, got: {err:?}");
        };
        assert_eq!(selector, "shell_surface[kind=dock]");
        let diagnosis = err.diagnosis().expect("the terminal failure must diagnose");
        assert_eq!(
            diagnosis.candidates,
            vec![
                format!("taskbar \"Taskbar\" (pid={MOCK_SHELL_PID})"),
                format!("desktop \"Desktop\" (pid={MOCK_SHELL_PID})"),
            ]
        );
    }

    #[test]
    fn by_kind_with_refuses_ambiguity_immediately() {
        // Several surfaces of one kind: refuse with the candidate list rather
        // than acting on an arbitrary one — and refuse *now*, since waiting
        // cannot make the shell less ambiguous.
        let provider: Arc<dyn Provider> = Arc::new(DuplicateSurfaceProvider::new());
        let start = Instant::now();
        let err = ShellSurface::by_kind_with(
            provider,
            ShellSurfaceKind::Taskbar,
            Duration::from_secs(30),
        )
        .expect_err("two taskbars must be refused, not first-matched");
        assert!(matches!(err, Error::SelectorNotMatched { .. }));
        let diagnosis = err.diagnosis().expect("ambiguity must diagnose");
        assert_eq!(diagnosis.candidates.len(), 2);
        assert!(
            diagnosis
                .last_observed
                .as_deref()
                .is_some_and(|s| s.contains("ShellSurface::list()")),
            "the diagnosis must say how to disambiguate: {diagnosis:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "ambiguity is terminal, not a retry"
        );
    }

    #[test]
    fn by_kind_with_propagates_enumeration_failures_and_fails_fast() {
        // A broken enumeration is not "not yet" — it short-circuits the poll.
        let provider: Arc<dyn Provider> = Arc::new(BrokenShellProvider {
            inner: build_provider(),
        });
        let start = Instant::now();
        let err = ShellSurface::by_kind_with(
            provider,
            ShellSurfaceKind::Taskbar,
            Duration::from_secs(30),
        )
        .expect_err("a real enumeration error must propagate");
        assert!(matches!(err, Error::Platform { code: 55, .. }));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a real enumeration error must fail fast, not wait out the timeout"
        );
    }
}
