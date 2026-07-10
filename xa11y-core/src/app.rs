use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::element::{Element, ElementData, TreeNode};
use crate::error::{Diagnosis, Error, Result};
use crate::event_provider::Subscription;
use crate::locator::Locator;
use crate::provider::Provider;

/// Polling interval shared by all timeout-bearing lookups.
const LOOKUP_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Maximum number of running applications listed in a lookup-failure
/// diagnosis. Bounded per tenet 6 — diagnostics must not grow with an
/// unbounded environment.
const DIAG_APP_LIST_LIMIT: usize = 20;

/// Run `attempt` repeatedly until it succeeds or `timeout` elapses, treating
/// `SelectorNotMatched` as a "not yet" signal. All other errors short-circuit.
///
/// `Duration::ZERO` performs exactly one attempt — identical to a non-polling
/// call. On timeout, returns the last `SelectorNotMatched` error enriched
/// with `diagnose()`'s context. `diagnose` runs only on that terminal
/// failure (tenet 6: enrich at the terminal site, keep the retry signal
/// cheap).
fn poll_lookup<F, D>(timeout: Duration, mut attempt: F, diagnose: D) -> Result<App>
where
    F: FnMut() -> Result<App>,
    D: FnOnce() -> Diagnosis,
{
    let start = Instant::now();
    loop {
        match attempt() {
            Ok(app) => return Ok(app),
            Err(e @ Error::SelectorNotMatched { .. }) => {
                if start.elapsed() >= timeout {
                    return Err(merge_diagnosis(e, diagnose()));
                }
            }
            Err(e) => return Err(e),
        }
        std::thread::sleep(LOOKUP_POLL_INTERVAL);
    }
}

/// Merge terminal-site context into an error that may already carry a cheap
/// diagnosis from its construction site (e.g. the enumeration counts that
/// `Provider::app_by_pid` records). Fields already present win — they
/// describe the actual failing attempt.
fn merge_diagnosis(err: Error, extra: Diagnosis) -> Error {
    let mut d = err.diagnosis().cloned().unwrap_or_default();
    if d.condition.is_none() {
        d.condition = extra.condition;
    }
    if d.last_observed.is_none() {
        d.last_observed = extra.last_observed;
    }
    if d.candidates.is_empty() {
        d.candidates = extra.candidates;
    }
    if d.scope.is_none() {
        d.scope = extra.scope;
    }
    err.diagnose(d)
}

/// Tag the foreground application within an enumerated app list.
///
/// Resolves the focused app's pid *once* via [`Provider::focused_app`] and
/// sets [`StateSet::focused`](crate::element::StateSet::focused) on every
/// entry whose pid matches, so `App::focused` reflects foreground status for
/// apps obtained through `list`/`find` without a per-app focus query.
///
/// Tagging is by pid, so *every* enumerated entry of the foreground process is
/// tagged. On Windows — where apps are top-level windows, so one process can
/// contribute several entries (a main window plus a modal dialog; issue #304)
/// — that means each of the process's windows reports `focused`. When the
/// exact foreground window matters, use [`App::foreground_with`] instead, which
/// resolves it directly rather than tagging a whole process by pid.
///
/// "Nothing is focused" ([`Error::SelectorNotMatched`]) is not an error here:
/// it leaves every entry untagged (`focused = false`). Any other error is a
/// genuine focus-resolution failure and propagates rather than being silently
/// swallowed (tenet 1) — on every backend the focus query needs no more access
/// than the enumeration that produced `apps`.
fn tag_focused(provider: &Arc<dyn Provider>, apps: &mut [ElementData]) -> Result<()> {
    let focused_pid = match provider.focused_app() {
        Ok(data) => data.pid,
        Err(Error::SelectorNotMatched { .. }) => None,
        Err(e) => return Err(e),
    };
    for app in apps.iter_mut() {
        app.states.focused = focused_pid.is_some() && app.pid == focused_pid;
    }
    Ok(())
}

/// Bounded "what *is* running" snapshot for application-lookup failures:
/// the candidate list a consumer would otherwise produce by hand-logging
/// `App::list()` around the failure.
fn running_apps_diagnosis(provider: &Arc<dyn Provider>) -> Diagnosis {
    let candidates = match provider.list_apps() {
        Ok(apps) => {
            let total = apps.len();
            let mut out: Vec<String> = apps
                .iter()
                .take(DIAG_APP_LIST_LIMIT)
                .map(|a| {
                    let pid = a.pid.map(|p| format!(" (pid={p})")).unwrap_or_default();
                    format!("\"{}\"{pid}", a.name.clone().unwrap_or_default())
                })
                .collect();
            if total > DIAG_APP_LIST_LIMIT {
                out.push(format!("… (+{} more)", total - DIAG_APP_LIST_LIMIT));
            }
            out
        }
        // Surface the collection failure inside the diagnosis instead of
        // dropping it (tenet 1) — the original lookup error still wins.
        Err(e) => vec![format!("(application enumeration failed: {e})")],
    };
    Diagnosis {
        condition: Some("application discovery".to_string()),
        candidates,
        ..Diagnosis::default()
    }
}

/// A running application, the entry point for accessibility queries.
///
/// `App` is **not** an [`Element`] — it represents the application as a whole
/// and provides a [`locator`](App::locator) to search its accessibility tree.
pub struct App {
    /// Application name.
    pub name: String,
    /// Process ID.
    pub pid: Option<u32>,
    /// The underlying element data for this application.
    pub data: ElementData,
    provider: Arc<dyn Provider>,
}

impl App {
    /// Find an application matching `predicate`, using an explicit provider.
    ///
    /// Prefer `App::find` from the `xa11y` crate which uses the global
    /// singleton provider. `predicate` runs against each running app's
    /// [`ElementData`] on every poll; the first match in enumeration order
    /// wins. Timeout / polling semantics match
    /// [`by_name_with`](Self::by_name_with): `Duration::ZERO` performs a
    /// single attempt, only [`Error::SelectorNotMatched`] triggers a retry,
    /// and a failing `list_apps()` short-circuits.
    ///
    /// For a predicate that can itself fail, see
    /// [`try_find_with`](Self::try_find_with).
    pub fn find_with<F>(
        provider: Arc<dyn Provider>,
        timeout: Duration,
        predicate: F,
    ) -> Result<Self>
    where
        F: Fn(&ElementData) -> bool,
    {
        Self::try_find_with(provider, timeout, move |d| Ok(predicate(d)))
    }

    /// Like [`find_with`](Self::find_with), but with a fallible predicate.
    ///
    /// The predicate's result drives the same retry contract the lookup uses
    /// for the apps it enumerates: `Ok(false)` means "not this one, keep
    /// polling", while `Err(_)` aborts the search immediately and propagates
    /// — it is *not* treated as "no match". Language bindings use this so a
    /// predicate exception fails fast instead of being silently swallowed and
    /// surfacing later as a spurious timeout.
    pub fn try_find_with<F>(
        provider: Arc<dyn Provider>,
        timeout: Duration,
        predicate: F,
    ) -> Result<Self>
    where
        F: Fn(&ElementData) -> Result<bool>,
    {
        // Predicate finders tag the foreground app so the predicate can match
        // on `focused` (e.g. `find(|a| a.focused)`) and matched apps carry
        // correct foreground state.
        Self::find_matching(
            provider,
            timeout,
            predicate,
            || "application matching predicate".to_string(),
            true,
        )
    }

    /// Shared predicate-based discovery loop. `describe` supplies the
    /// [`Error::SelectorNotMatched`] selector string so name/pid lookups keep
    /// their specific, actionable error messages while sharing one match loop.
    ///
    /// `tag_focus` controls whether each poll resolves the foreground app and
    /// tags it onto the enumerated candidates. The predicate finders enable it
    /// (so `focused` is visible to the predicate); `by_name` disables it — a
    /// name lookup neither needs foreground state nor should pay the per-tick
    /// focus query (and shouldn't gain a focus-resolution failure mode).
    fn find_matching<F, D>(
        provider: Arc<dyn Provider>,
        timeout: Duration,
        predicate: F,
        describe: D,
        tag_focus: bool,
    ) -> Result<Self>
    where
        F: Fn(&ElementData) -> Result<bool>,
        D: Fn() -> String,
    {
        let diag_provider = Arc::clone(&provider);
        poll_lookup(
            timeout,
            || {
                // Discovery is platform-specific (CGWindowList on macOS, AT-SPI
                // registry on Linux, UIA desktop root on Windows). `list_apps()`
                // is the canonical enumeration primitive and we filter in Rust,
                // so app names containing `"`, `]`, or other characters
                // significant in the selector grammar don't need escaping.
                //
                // Errors from `list_apps()` propagate so callers can distinguish
                // "app not found" from "accessibility is broken". A predicate
                // error propagates for the same reason — `poll_lookup` only
                // retries `SelectorNotMatched`, so anything else fails fast.
                let mut apps = provider.list_apps()?;
                if tag_focus {
                    tag_focused(&provider, &mut apps)?;
                }
                for data in apps {
                    if predicate(&data)? {
                        return Ok(Self::from_data(Arc::clone(&provider), data));
                    }
                }
                Err(Error::selector_not_matched(describe()))
            },
            || running_apps_diagnosis(&diag_provider),
        )
    }

    /// Find an application by exact name, using an explicit provider.
    ///
    /// Prefer `App::by_name` from the `xa11y` crate which uses the global
    /// singleton provider. Use this variant when you need to supply a specific
    /// provider (e.g. a mock in unit tests).
    ///
    /// Polls the accessibility API until the app appears or `timeout` elapses.
    /// `Duration::ZERO` performs exactly one attempt (no waiting). Only
    /// [`Error::SelectorNotMatched`] triggers a retry; other errors
    /// (permission, parse, platform) short-circuit immediately.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSelector`] if `name` contains a double quote
    /// (`"`). The not-found diagnostic for this lookup is the selector string
    /// `application[name="<name>"]`, and the selector grammar has no escape
    /// sequence for quotes inside attribute values, so such a name cannot be
    /// represented — rather than emitting a malformed selector, the lookup is
    /// rejected up front. Use [`find_with`](Self::find_with) with a name
    /// predicate to locate apps whose names contain double quotes.
    pub fn by_name_with(
        provider: Arc<dyn Provider>,
        name: &str,
        timeout: Duration,
    ) -> Result<Self> {
        // The selector grammar's attribute values (`"..."` / `'...'`) have no
        // escape support (see `selector.rs`), so a name containing `"` would
        // interpolate into a malformed `application[name="..."]` selector
        // string below. Surface that clearly instead of producing it.
        if name.contains('"') {
            return Err(Error::InvalidSelector {
                selector: name.to_string(),
                message: "app name contains a double quote, which cannot be escaped in the \
                          selector grammar; use App::find_with with a name predicate instead"
                    .to_string(),
            });
        }
        // `tag_focus = false`: a name lookup doesn't need foreground state, so
        // it skips the per-tick focus query (apps from `by_name` therefore
        // report `focused() == false` — use `list`/`find` to query foreground
        // status).
        Self::find_matching(
            provider,
            timeout,
            |d| Ok(d.name.as_deref() == Some(name)),
            || format!(r#"application[name="{}"]"#, name),
            false,
        )
    }

    /// Find an application by process ID, using an explicit provider.
    ///
    /// Prefer `App::by_pid` from the `xa11y` crate which uses the global
    /// singleton provider.
    ///
    /// This is the supported way to **wait for a freshly launched process to
    /// surface** in the accessibility tree: the lookup polls
    /// [`Provider::app_by_pid`] until the application becomes reachable or
    /// `timeout` elapses, covering the window between process spawn and the
    /// platform bridge registering the app (slow CI runners, toolkits that
    /// initialise accessibility lazily). There is no need to hand-roll a
    /// poll over [`list_with`](Self::list_with).
    ///
    /// Timeout / polling semantics match [`by_name_with`](Self::by_name_with):
    /// `Duration::ZERO` performs exactly one attempt, only
    /// [`Error::SelectorNotMatched`] ("not reachable yet") triggers a retry,
    /// and permission / platform errors short-circuit immediately.
    ///
    /// Where the platform supports it (macOS AX, Windows UIA), the provider
    /// attaches to the process directly instead of filtering app enumeration,
    /// so an app whose window is still unnamed mid-startup is found as soon
    /// as the accessibility API can reach it.
    pub fn by_pid_with(provider: Arc<dyn Provider>, pid: u32, timeout: Duration) -> Result<Self> {
        let diag_provider = Arc::clone(&provider);
        poll_lookup(
            timeout,
            || {
                let data = provider.app_by_pid(pid)?;
                Ok(Self::from_data(Arc::clone(&provider), data))
            },
            || running_apps_diagnosis(&diag_provider),
        )
    }

    /// Resolve the application that currently holds the system foreground,
    /// using an explicit provider.
    ///
    /// Prefer `App::foreground` from the `xa11y` crate which uses the global
    /// singleton provider.
    ///
    /// Identifies the foreground application via each platform's canonical
    /// mechanism: the system-wide `AXUIElement`'s focused-application attribute
    /// (macOS), `GetForegroundWindow` + `ElementFromHandle` (Windows), and the
    /// focused element's `Application` ancestor in the AT-SPI registry (Linux).
    /// Unlike [`find_with`](Self::find_with) with a `|d| d.states.focused`
    /// predicate — which enumerates apps and tags foreground state by pid —
    /// this calls the platform foreground query directly, so on Windows it
    /// returns the *exact* foreground window even when the owning process holds
    /// several top-level windows (the modal case; issues #304/#305).
    ///
    /// Timeout / polling semantics match [`by_name_with`](Self::by_name_with):
    /// `Duration::ZERO` performs exactly one attempt, only
    /// [`Error::SelectorNotMatched`] ("nothing currently holds focus" — focus
    /// rests on the desktop / shell, or the screen is locked) triggers a retry,
    /// and any other error short-circuits immediately. The returned `App`
    /// always reports [`focused()`](Self::focused) `== true`.
    pub fn foreground_with(provider: Arc<dyn Provider>, timeout: Duration) -> Result<Self> {
        let diag_provider = Arc::clone(&provider);
        poll_lookup(
            timeout,
            || {
                let mut data = provider.focused_app()?;
                // focused_app resolves the foreground app by definition; tag it
                // so `App::focused()` agrees with how list/find populate the flag.
                data.states.focused = true;
                Ok(Self::from_data(Arc::clone(&provider), data))
            },
            || running_apps_diagnosis(&diag_provider),
        )
    }

    /// List all running applications, using an explicit provider.
    ///
    /// Prefer `App::list` from the `xa11y` crate which uses the global
    /// singleton provider.
    pub fn list_with(provider: Arc<dyn Provider>) -> Result<Vec<Self>> {
        // `list_apps()` is the platform-specific discovery primitive — it
        // already handles the per-OS app/window split (Linux/macOS return
        // `Application` elements; Windows returns top-level `Window`
        // elements), so we just wrap each entry.
        let mut datas = provider.list_apps()?;
        // Mark the foreground app (one focus query) so `App::focused` is
        // populated across the returned list without an extra call per app.
        tag_focused(&provider, &mut datas)?;
        Ok(datas
            .into_iter()
            .map(|d| Self::from_data(Arc::clone(&provider), d))
            .collect())
    }

    fn from_data(provider: Arc<dyn Provider>, data: ElementData) -> Self {
        let name = data.name.clone().unwrap_or_default();
        let pid = data.pid;
        Self {
            name,
            pid,
            data,
            provider,
        }
    }

    /// Create a [`Locator`] to search this application's accessibility tree.
    pub fn locator(&self, selector: &str) -> Locator {
        Locator::new(
            Arc::clone(&self.provider),
            Some(self.data.clone()),
            selector,
        )
    }

    /// Subscribe to accessibility events from this application.
    pub fn subscribe(&self) -> Result<Subscription> {
        self.provider.subscribe(&self.data)
    }

    /// Get direct children (typically windows) of this application.
    pub fn children(&self) -> Result<Vec<Element>> {
        let children = self.provider.get_children(Some(&self.data))?;
        Ok(children
            .into_iter()
            .map(|d| Element::new(d, Arc::clone(&self.provider)))
            .collect())
    }

    /// Capture the application's accessibility tree as a recursive snapshot,
    /// rooted at the application element.
    ///
    /// Equivalent to `self.as_element().tree(max_depth)`. See
    /// [`Element::tree`] for `max_depth` semantics.
    pub fn tree(&self, max_depth: Option<usize>) -> Result<TreeNode> {
        self.as_element().tree(max_depth)
    }

    /// Render the application's accessibility tree as an indented string,
    /// rooted at the application element.
    ///
    /// The primary inspection helper for figuring out the role/name of every
    /// element in an app before writing selectors. Equivalent to
    /// `self.as_element().dump(max_depth)`. See [`Element::dump`] for the
    /// output format.
    pub fn dump(&self, max_depth: Option<usize>) -> Result<String> {
        self.as_element().dump(max_depth)
    }

    /// Get an [`Element`] handle for the application root.
    ///
    /// Useful when you want to use Element-level methods (e.g. `tree`,
    /// `dump`, `children`) without going through a locator.
    pub fn as_element(&self) -> Element {
        Element::new(self.data.clone(), Arc::clone(&self.provider))
    }

    /// Whether this application currently holds the foreground / input focus.
    ///
    /// Mirrors [`StateSet::focused`](crate::element::StateSet::focused) one
    /// level up: just as an element is `focused` when it has input focus, an
    /// application is `focused` when it is the foreground app.
    ///
    /// Populated when the `App` is obtained via [`list_with`](Self::list_with)
    /// or the predicate finders ([`find_with`](Self::find_with) /
    /// [`try_find_with`](Self::try_find_with) — where it is also visible to the
    /// predicate, so `find(|a| a.focused())` selects the foreground app). The
    /// value is a point-in-time snapshot taken when the `App` was resolved.
    /// Apps obtained directly via [`by_pid_with`](Self::by_pid_with) carry the
    /// platform's raw app-element focus state instead (typically `false`).
    ///
    /// On Windows apps are top-level windows, so tagging by pid means *every*
    /// top-level window of the foreground process reports `focused`. Use
    /// [`foreground_with`](Self::foreground_with) (or `App::foreground` from
    /// the `xa11y` crate) to obtain the exact foreground window.
    pub fn focused(&self) -> bool {
        self.data.states.focused
    }

    /// Get the provider reference.
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }
}

impl std::fmt::Display for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "application \"{}\"", self.name)
    }
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("name", &self.name)
            .field("pid", &self.pid)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::build_provider;
    use crate::role::Role;

    fn mock_app() -> App {
        let provider: Arc<dyn Provider> = build_provider();
        App::by_name_with(provider, "TestApp", Duration::ZERO)
            .expect("TestApp must exist in mock tree")
    }

    /// What [`FocusModeProvider`] reports from `focused_app`.
    enum FocusMode {
        /// No application currently holds focus.
        None,
        /// Focus resolution hit a genuine platform failure.
        Error,
    }

    /// Wraps the standard mock provider but overrides `focused_app` so the
    /// foreground-tagging error paths can be exercised. Everything else
    /// delegates to the inner mock.
    struct FocusModeProvider {
        inner: Arc<crate::mock::MockProvider>,
        mode: FocusMode,
    }

    impl FocusModeProvider {
        fn new(mode: FocusMode) -> Self {
            Self {
                inner: build_provider(),
                mode,
            }
        }
    }

    impl Provider for FocusModeProvider {
        fn focused_app(&self) -> Result<ElementData> {
            match self.mode {
                FocusMode::None => Err(Error::selector_not_matched("focused application")),
                FocusMode::Error => Err(Error::Platform {
                    code: 99,
                    message: "focus query failed".to_string(),
                }),
            }
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

    /// Provider modelling the Windows modal case (issue #304): one process
    /// owning two top-level windows. `list_apps` returns both windows sharing
    /// pid 42, and `focused_app` reports that pid as foreground. Everything
    /// else delegates to the inner mock.
    struct MultiWindowProvider {
        inner: Arc<crate::mock::MockProvider>,
    }

    impl MultiWindowProvider {
        fn new() -> Self {
            Self {
                inner: build_provider(),
            }
        }

        /// A top-level window owned by the shared process (pid 42).
        fn window(name: &str, handle: u64) -> ElementData {
            ElementData {
                role: Role::Window,
                name: Some(name.to_string()),
                value: None,
                description: None,
                bounds: None,
                actions: vec![],
                states: crate::element::StateSet::default(),
                numeric_value: None,
                min_value: None,
                max_value: None,
                stable_id: None,
                pid: Some(42),
                raw: Default::default(),
                handle,
            }
        }
    }

    impl Provider for MultiWindowProvider {
        fn list_apps(&self) -> Result<Vec<ElementData>> {
            // Two top-level windows of the same process — a main window and a
            // modal dialog. Both must survive enumeration (no pid dedup).
            Ok(vec![Self::window("Main", 100), Self::window("Modal", 101)])
        }
        fn focused_app(&self) -> Result<ElementData> {
            Ok(Self::window("Modal", 101))
        }
        fn get_children(&self, e: Option<&ElementData>) -> Result<Vec<ElementData>> {
            self.inner.get_children(e)
        }
        fn get_parent(&self, e: &ElementData) -> Result<Option<ElementData>> {
            self.inner.get_parent(e)
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

    #[test]
    fn app_tree_returns_application_root() {
        let node = mock_app().tree(None).expect("tree must succeed");
        assert_eq!(node.role, "application");
        assert_eq!(node.name.as_deref(), Some("TestApp"));
        assert!(
            !node.children.is_empty(),
            "TestApp must have at least one window child"
        );
    }

    #[test]
    fn app_tree_max_depth_zero_has_no_children() {
        let node = mock_app().tree(Some(0)).expect("tree must succeed");
        assert_eq!(node.role, "application");
        assert!(node.children.is_empty());
    }

    #[test]
    fn app_tree_max_depth_one_stops_at_direct_children() {
        let node = mock_app().tree(Some(1)).expect("tree must succeed");
        assert!(!node.children.is_empty());
        for child in &node.children {
            assert!(
                child.children.is_empty(),
                "max_depth=1 must stop after direct children"
            );
        }
    }

    #[test]
    fn app_dump_contains_application_root() {
        let s = mock_app().dump(None).expect("dump must succeed");
        assert!(
            s.contains(r#"application "TestApp""#),
            "dump output should include the application root: {s}"
        );
    }

    #[test]
    fn app_dump_max_depth_zero_is_one_line() {
        let s = mock_app().dump(Some(0)).expect("dump must succeed");
        let non_empty: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(non_empty.len(), 1, "max_depth=0 should be a single line");
        assert!(non_empty[0].contains("application"));
    }

    #[test]
    fn app_as_element_is_root() {
        let app = mock_app();
        let el = app.as_element();
        assert_eq!(el.data().role, Role::Application);
        assert_eq!(el.data().name.as_deref(), Some("TestApp"));
    }

    #[test]
    fn by_name_with_rejects_double_quote_in_name() {
        // The selector grammar has no escape sequence for quotes inside
        // attribute values, so a name containing `"` cannot be represented
        // in the `application[name="..."]` diagnostic selector. The lookup
        // must fail clearly up front instead of emitting a malformed
        // selector (tenet 1: no silent fallback to a broken string).
        let provider: Arc<dyn Provider> = build_provider();
        let err = App::by_name_with(provider, r#"My "Quoted" App"#, Duration::ZERO)
            .expect_err("names containing '\"' must be rejected");
        match err {
            Error::InvalidSelector { selector, message } => {
                assert_eq!(selector, r#"My "Quoted" App"#);
                assert!(
                    message.contains("double quote"),
                    "message must explain the quote limitation: {message}"
                );
                assert!(
                    message.contains("find_with"),
                    "message must point at the predicate-based alternative: {message}"
                );
            }
            other => panic!("expected InvalidSelector, got: {other:?}"),
        }
    }

    #[test]
    fn find_with_matches_by_predicate() {
        let provider: Arc<dyn Provider> = build_provider();
        let app = App::find_with(provider, Duration::ZERO, |d| {
            d.name.as_deref() == Some("TestApp")
        })
        .expect("predicate must match TestApp in mock tree");
        assert_eq!(app.name, "TestApp");
    }

    #[test]
    fn find_with_no_match_returns_selector_not_matched() {
        let provider: Arc<dyn Provider> = build_provider();
        let err = App::find_with(provider, Duration::ZERO, |_| false)
            .expect_err("a never-true predicate must not match any app");
        assert!(matches!(err, Error::SelectorNotMatched { .. }));
    }

    #[test]
    fn try_find_with_propagates_predicate_error_and_fails_fast() {
        let provider: Arc<dyn Provider> = build_provider();
        // A generous timeout: if the predicate error were treated as "no
        // match" the call would block for 30s. Returning immediately proves
        // the error short-circuits the poll loop.
        let start = Instant::now();
        let err = App::try_find_with(provider, Duration::from_secs(30), |_| {
            Err(Error::Platform {
                code: 7,
                message: "boom".to_string(),
            })
        })
        .expect_err("a predicate error must propagate, not retry");
        assert!(matches!(err, Error::Platform { code: 7, .. }));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "predicate error must fail fast, not wait out the timeout"
        );
    }

    #[test]
    fn list_with_tags_foreground_app_as_focused() {
        // MockProvider::focused_app reports the application root (pid 1234) as
        // foreground, so the lone listed app must come back `focused()`.
        let provider: Arc<dyn Provider> = build_provider();
        let apps = App::list_with(provider).expect("list must succeed");
        assert_eq!(apps.len(), 1);
        assert!(
            apps[0].focused(),
            "the foreground app must be tagged focused by list_with"
        );
    }

    #[test]
    fn find_with_predicate_sees_focused_flag() {
        // The predicate runs against the tagged ElementData, so selecting on
        // `focused` must match the foreground app.
        let provider: Arc<dyn Provider> = build_provider();
        let app = App::find_with(provider, Duration::ZERO, |d| d.states.focused)
            .expect("the foreground app must be findable via the focused flag");
        assert_eq!(app.name, "TestApp");
        assert!(app.focused());
    }

    #[test]
    fn list_with_leaves_apps_untagged_when_nothing_focused() {
        // A provider whose `focused_app` reports "nothing focused"
        // (SelectorNotMatched) must not fail enumeration — every app stays
        // unfocused rather than the error propagating.
        let provider: Arc<dyn Provider> = Arc::new(FocusModeProvider::new(FocusMode::None));
        let apps = App::list_with(provider).expect("list must succeed with no focused app");
        assert_eq!(apps.len(), 1);
        assert!(
            !apps[0].focused(),
            "no app should be focused when focused_app reports none"
        );
    }

    #[test]
    fn list_with_propagates_real_focus_errors() {
        // A genuine focus-resolution failure (not "nothing focused") must
        // surface, not be silently swallowed (tenet 1).
        let provider: Arc<dyn Provider> = Arc::new(FocusModeProvider::new(FocusMode::Error));
        let err = App::list_with(provider).expect_err("a real focus error must propagate");
        assert!(matches!(err, Error::Platform { code: 99, .. }));
    }

    #[test]
    fn try_find_with_ok_false_keeps_polling_then_times_out() {
        let provider: Arc<dyn Provider> = build_provider();
        // `Ok(false)` is "not yet" — with a zero timeout that's one attempt
        // and then a normal not-found result (no error propagation).
        let err = App::try_find_with(provider, Duration::ZERO, |_| Ok(false))
            .expect_err("an always-Ok(false) predicate must not match");
        assert!(matches!(err, Error::SelectorNotMatched { .. }));
    }

    #[test]
    fn list_with_keeps_all_windows_of_a_shared_pid() {
        // Regression for issue #304: a process owning several top-level
        // windows (main window + modal dialog) must surface every window in
        // `App::list_with` — the old pid dedup silently dropped the modal.
        // Both entries share pid 42, which `focused_app` reports as
        // foreground, so both come back `focused()`.
        let provider: Arc<dyn Provider> = Arc::new(MultiWindowProvider::new());
        let apps = App::list_with(provider).expect("list must succeed");
        assert_eq!(apps.len(), 2, "both windows of the shared pid must appear");
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert!(
            names.contains(&"Main"),
            "main window must be listed: {names:?}"
        );
        assert!(
            names.contains(&"Modal"),
            "modal window must be listed: {names:?}"
        );
        assert!(
            apps.iter().all(|a| a.focused()),
            "every window of the foreground process must be tagged focused"
        );
    }

    #[test]
    fn foreground_with_resolves_and_tags_the_foreground_app() {
        // The mock reports its application root (pid 1234) as the foreground
        // app; `foreground_with` must return it and mark it `focused()`.
        let provider: Arc<dyn Provider> = build_provider();
        let app = App::foreground_with(provider, Duration::ZERO)
            .expect("the mock's foreground app must resolve");
        assert_eq!(app.name, "TestApp");
        assert!(
            app.focused(),
            "the app returned by foreground_with must always be focused()"
        );
    }

    #[test]
    fn foreground_with_returns_selector_not_matched_when_nothing_focused() {
        // "Nothing holds focus" is the retryable not-found signal; with a zero
        // timeout that's one attempt and a plain SelectorNotMatched.
        let provider: Arc<dyn Provider> = Arc::new(FocusModeProvider::new(FocusMode::None));
        let err = App::foreground_with(provider, Duration::ZERO)
            .expect_err("nothing focused must surface as not-matched");
        assert!(matches!(err, Error::SelectorNotMatched { .. }));
    }

    #[test]
    fn foreground_with_propagates_real_focus_errors_and_fails_fast() {
        // A genuine foreground-query failure (not "nothing focused") must
        // short-circuit the poll immediately rather than being retried until
        // the timeout (mirrors `try_find_with_propagates_predicate_error_...`).
        let provider: Arc<dyn Provider> = Arc::new(FocusModeProvider::new(FocusMode::Error));
        let start = Instant::now();
        let err = App::foreground_with(provider, Duration::from_secs(30))
            .expect_err("a real focus error must propagate");
        assert!(matches!(err, Error::Platform { code: 99, .. }));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a real focus error must fail fast, not wait out the timeout"
        );
    }
}
