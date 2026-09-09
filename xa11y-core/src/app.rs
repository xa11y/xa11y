use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::element::{Element, ElementData, TreeNode};
use crate::error::{Diagnosis, Error, Result};
use crate::event_provider::Subscription;
use crate::locator::Locator;
use crate::provider::Provider;
use crate::role::Role;

/// Polling interval shared by all timeout-bearing lookups (application and
/// shell-surface alike — see `shell.rs`).
pub(crate) const LOOKUP_POLL_INTERVAL: Duration = Duration::from_millis(100);

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
/// sets [`StateSet::focused`](crate::element::StateSet::focused) on the
/// entry that is actually in the foreground, so `App::is_foreground` reflects
/// foreground status for apps obtained through `list`/`find` without a per-app
/// focus query.
///
/// Tagging is *window-precise*. One pid can surface several `Application`
/// entries — the Linux AT-SPI registry can register several accessibles for
/// one process (a main application plus a dialog it exposes as a second app
/// node), so a single process can contribute several entries that all share
/// the foreground pid. macOS synthesizes exactly one node per pid and
/// Windows one per process, so there a pid match alone is unambiguous. Where
/// several entries share the foreground pid, the platform's window-level
/// `active` flag picks the one actually in the foreground. Concretely, an
/// entry is tagged when its pid matches *and* either it is the only
/// pid-matching entry (the unambiguous case) or it reports `active`.
///
/// If several entries share the foreground pid and none reports `active` (the
/// foreground window wasn't enumerable), none is tagged — that's honest, not a
/// fallback (tenet 1). When the exact foreground window matters, resolve the
/// foreground application via [`App::foreground_with`], then pick the window
/// reporting [`active`](crate::element::StateSet::active) from its
/// [`App::windows`].
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
    // How many enumerated entries share the foreground pid. One match → pid
    // alone is unambiguous; several → disambiguate by the entry-level
    // `active` flag (the Linux AT-SPI multi-registration case).
    let n_matches = apps
        .iter()
        .filter(|a| focused_pid.is_some() && a.pid == focused_pid)
        .count();
    for app in apps.iter_mut() {
        let pid_match = focused_pid.is_some() && app.pid == focused_pid;
        app.states.focused = pid_match && (n_matches == 1 || app.states.active);
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

/// Whether two element snapshots identify the same window (or the same
/// Application entry).
///
/// Keyed by the platform's stable identity: `stable_id` — on Linux the D-Bus
/// object path, scoped by the access bus name recorded in `raw["bus_name"]`
/// (two bus connections of one process can expose distinct windows under the
/// same path), on Windows the native HWND for top-level windows. Elements
/// without a stable_id (macOS `AXIdentifier` is often absent) key on the
/// handle, which is unique per built node within one enumeration — distinct
/// windows are never merged on presentation data like a shared title and
/// bounds.
fn same_window_identity(a: &ElementData, b: &ElementData) -> bool {
    fn key(d: &ElementData) -> String {
        d.stable_id
            .clone()
            .map(|sid| match d.raw.get("bus_name") {
                Some(serde_json::Value::String(bus)) => format!("{bus}:{sid}"),
                _ => sid,
            })
            .unwrap_or_else(|| format!("h{}", d.handle))
    }
    key(a) == key(b)
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
    /// this calls the platform foreground query directly. The result is the
    /// foreground *process*'s `Application` node — one per process on every
    /// platform — not the exact window holding the foreground. On Windows the
    /// query resolves the foreground HWND to its process's synthesized
    /// `Application` node instead of returning the window itself (the modal
    /// case; issues #304/#305). To reach the exact foreground window, list the
    /// node's [`windows`](Self::windows) and pick the one reporting
    /// [`active`](crate::element::StateSet::active).
    ///
    /// Timeout / polling semantics match [`by_name_with`](Self::by_name_with):
    /// `Duration::ZERO` performs exactly one attempt, only
    /// [`Error::SelectorNotMatched`] ("nothing currently holds focus" — focus
    /// rests on the desktop / shell, or the screen is locked) triggers a retry,
    /// and any other error short-circuits immediately. The returned `App`
    /// always reports [`is_foreground()`](Self::is_foreground) `== true`.
    pub fn foreground_with(provider: Arc<dyn Provider>, timeout: Duration) -> Result<Self> {
        let diag_provider = Arc::clone(&provider);
        poll_lookup(
            timeout,
            || {
                let mut data = provider.focused_app()?;
                // focused_app resolves the foreground app by definition; tag it
                // so `App::is_foreground()` agrees with how list/find populate
                // the flag.
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
        // `list_apps()` returns one Application node per process on macOS and
        // Windows (Windows synthesizes it); Linux's AT-SPI registry can
        // register several entries for one pid, and the list returns them
        // all — consumers that need process-complete window listings merge
        // them by stable identity (see `windows_with`).
        let mut datas = provider.list_apps()?;
        // Mark the foreground app (one focus query) so `App::focused` is
        // populated across the returned list without an extra call per app.
        tag_focused(&provider, &mut datas)?;
        Ok(datas
            .into_iter()
            .map(|d| Self::from_data(Arc::clone(&provider), d))
            .collect())
    }

    /// List the top-level windows of the process owning `data`, using an
    /// explicit provider.
    ///
    /// Prefer [`App::windows`], which uses this `App`'s provider.
    ///
    /// `data` must be an `Application` element. Passing any other role is an
    /// [`Error::ActionNotSupported`] naming the role; the "windows of a
    /// window" question has no meaning once app entries stopped being
    /// windows. On Windows the children of the synthetic `Application` node
    /// *are* the process's top-level windows, so this returns them in
    /// enumeration (z-) order; on macOS and Linux it is the same
    /// `get_children` + `Window|Dialog` filter applied to the platform's
    /// Application node.
    ///
    /// The listing is **process-complete**: macOS and Windows guarantee one
    /// Application node per pid, so the single node's children already cover
    /// the process, but the Linux AT-SPI registry can register several
    /// `Application` entries for one pid (a main application plus a dialog
    /// exposed as its own app node), and each entry's `get_children` answer is
    /// only its own windows. `App::by_pid` resolves a single entry, so
    /// without aggregation `by_pid(pid).windows()` would silently omit the
    /// sibling entries' windows. Every same-pid entry is therefore merged and
    /// deduplicated by the platform's stable identity — the same key the
    /// `xa11y windows` listing applies per pid, with one subtle difference:
    /// results here keep the *windows* of the calling entry first, in
    /// enumeration order, followed by the other entries' windows (registry
    /// order), so a window is never listed twice whatever the entry count.
    /// The merge runs only for providers that can split a process across
    /// entries ([`Provider::splits_app_across_entries`]): on macOS and
    /// Windows one entry covers the process, so `list_apps` is not
    /// re-enumerated there at all.
    pub fn windows_with(provider: Arc<dyn Provider>, data: &ElementData) -> Result<Vec<Element>> {
        if data.role != Role::Application {
            return Err(Error::ActionNotSupported {
                action: "windows".into(),
                role: data.role,
            });
        }
        let mut children = provider.get_children(Some(data))?;
        // Only providers that can expose several Application entries per pid
        // need the cross-entry merge. On macOS and Windows one entry covers
        // the process, so re-enumerating `list_apps` here would be pure waste
        // — and a re-enumeration rebuilds macOS data with fresh handles, so
        // windows without an AXIdentifier would no longer deduplicate and
        // `windows()` would report each window twice. See
        // `Provider::splits_app_across_entries`.
        if let Some(pid) = data.pid {
            if provider.splits_app_across_entries() {
                for entry in provider.list_apps()? {
                    // Skip the calling entry itself (its children are already in
                    // the list) — identified by the same stable-identity key, not
                    // by handle: entries are rebuilt per query on Linux, so the
                    // handle differs across calls while the stable identity
                    // (D-Bus object path scoped by bus name) does not.
                    if entry.pid != Some(pid) || same_window_identity(&entry, data) {
                        continue;
                    }
                    for child in provider.get_children(Some(&entry))? {
                        if !children.iter().any(|c| same_window_identity(c, &child)) {
                            children.push(child);
                        }
                    }
                }
            }
        }
        Ok(children
            .into_iter()
            .filter(|d| matches!(d.role, Role::Window | Role::Dialog))
            .map(|d| Element::new(d, Arc::clone(&provider)))
            .collect())
    }

    /// List the top-level windows of this application.
    ///
    /// Each call queries the provider — results are not cached. The windows
    /// are the application's top-level windows with role `window` or
    /// `dialog`, in enumeration order. On Windows the application entry is a
    /// synthesized process node whose children are the process's top-level
    /// windows (main window plus modal dialogs); macOS's own Application node
    /// has the same one-entry-per-process property. On Linux the answer is
    /// process-complete: [`App::windows_with`] merges the filtered children of
    /// every same-pid AT-SPI Application entry (an app that registers several
    /// entries reports its whole process), so the results need not be the
    /// direct children of this node and no single z-order spans them. Calling
    /// `windows` on a non-Application element (e.g. a window from a previous
    /// release, or the `Window` entries this API used to return on Windows)
    /// fails with [`Error::ActionNotSupported`] rather than silently
    /// answering a question that no longer has a meaning.
    pub fn windows(&self) -> Result<Vec<Element>> {
        Self::windows_with(Arc::clone(&self.provider), &self.data)
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

    /// Whether this application is the foreground application.
    ///
    /// Named `is_foreground` because "focused" is reserved for element-level
    /// keyboard focus ([`StateSet::focused`](crate::element::StateSet::focused))
    /// elsewhere in the API; this is the foreground-*application* flag.
    ///
    /// Populated when the `App` is obtained via [`list_with`](Self::list_with)
    /// or the predicate finders ([`find_with`](Self::find_with) /
    /// [`try_find_with`](Self::try_find_with) — where it is also visible to the
    /// predicate via `d.states.focused`, so `find(|a| a.states.focused)` selects
    /// the foreground app). The value is a point-in-time snapshot taken when the
    /// `App` was resolved. Apps obtained directly via
    /// [`by_pid_with`](Self::by_pid_with) carry the platform's raw app-element
    /// focus state instead (typically `false`).
    ///
    /// Tagging is window-precise. On Linux the AT-SPI registry can surface
    /// several `Application` entries for one pid, so only the entry actually
    /// in the foreground — the one reporting the platform's window-level
    /// `active` flag — reports `is_foreground`, not every entry of the
    /// process. macOS and Windows report one node per pid/process, so there
    /// this mirrors the foreground process.
    /// Use [`foreground_with`](Self::foreground_with) (or `App::foreground`
    /// from the `xa11y` crate) to resolve the foreground application directly,
    /// then pick the exact foreground window from its [`windows`](Self::windows)
    /// by the [`active`](crate::element::StateSet::active) state.
    pub fn is_foreground(&self) -> bool {
        self.data.states.focused
    }

    /// Deprecated alias for [`is_foreground`](Self::is_foreground).
    #[deprecated(
        note = "renamed to `is_foreground`; `focused` refers to element keyboard focus elsewhere in the API"
    )]
    pub fn focused(&self) -> bool {
        self.is_foreground()
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
        fn list_shell_surfaces(
            &self,
        ) -> Result<Vec<(crate::shell::ShellSurfaceKind, ElementData)>> {
            self.inner.list_shell_surfaces()
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
        fn raise(&self, e: &ElementData) -> Result<()> {
            self.inner.raise(e)
        }
        fn minimize(&self, e: &ElementData) -> Result<()> {
            self.inner.minimize(e)
        }
        fn maximize(&self, e: &ElementData) -> Result<()> {
            self.inner.maximize(e)
        }
        fn restore(&self, e: &ElementData) -> Result<()> {
            self.inner.restore(e)
        }
        fn close(&self, e: &ElementData) -> Result<()> {
            self.inner.close(e)
        }
        fn move_to(&self, e: &ElementData, x: i32, y: i32) -> Result<()> {
            self.inner.move_to(e, x, y)
        }
        fn resize_to(&self, e: &ElementData, w: u32, h: u32) -> Result<()> {
            self.inner.resize_to(e, w, h)
        }
        fn subscribe(&self, e: &ElementData) -> Result<Subscription> {
            self.inner.subscribe(e)
        }
    }

    /// Provider modelling the Windows modal case (issue #304): one process
    /// owning two top-level windows. `list_apps` returns ONE Application node
    /// (pid 42) for the whole process, and its `get_children` answer is both
    /// windows — the shape every platform reports after the app-node
    /// unification (Windows synthesizes the Application node and answers
    /// `get_children(Some(app))` from a process-wide UIA query; macOS reads
    /// `AXWindows`, Linux filters the AT-SPI walk). Only the dialog reports
    /// `active`, the window-level foreground flag.
    struct MultiWindowProvider {
        inner: Arc<crate::mock::MockProvider>,
    }

    impl MultiWindowProvider {
        fn new() -> Self {
            Self {
                inner: build_provider(),
            }
        }

        /// The Application node for the shared process (pid 42).
        fn app(handle: u64) -> ElementData {
            ElementData {
                role: Role::Application,
                name: Some("SharedApp".to_string()),
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

        /// A top-level window owned by the shared process (pid 42). `active`
        /// models the platform's window-level foreground flag — only the
        /// window actually in the foreground reports it.
        fn window(name: &str, handle: u64, active: bool) -> ElementData {
            ElementData {
                role: Role::Window,
                name: Some(name.to_string()),
                value: None,
                description: None,
                bounds: None,
                actions: vec![],
                states: crate::element::StateSet {
                    active,
                    ..crate::element::StateSet::default()
                },
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
            // One Application node per process; both top-level windows of the
            // shared process are its children (issue #304). The app node is
            // not a window, so enumerating it yields the windows, not the
            // process twice.
            Ok(vec![Self::app(700)])
        }
        fn focused_app(&self) -> Result<ElementData> {
            Ok(Self::app(700))
        }
        fn list_shell_surfaces(
            &self,
        ) -> Result<Vec<(crate::shell::ShellSurfaceKind, ElementData)>> {
            self.inner.list_shell_surfaces()
        }
        fn get_children(&self, e: Option<&ElementData>) -> Result<Vec<ElementData>> {
            match e {
                // The Application node's children are the process's top-level
                // windows — main + modal, in enumeration (z-) order.
                Some(el) if matches!(el.role, Role::Application) && el.pid == Some(42) => Ok(vec![
                    Self::window("Main", 100, false),
                    Self::window("Modal", 101, true),
                ]),
                _ => self.inner.get_children(e),
            }
        }
        fn get_parent(&self, e: &ElementData) -> Result<Option<ElementData>> {
            if matches!(e.role, Role::Window | Role::Dialog) && e.pid == Some(42) {
                return Ok(Some(Self::app(700)));
            }
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
        fn raise(&self, e: &ElementData) -> Result<()> {
            self.inner.raise(e)
        }
        fn minimize(&self, e: &ElementData) -> Result<()> {
            self.inner.minimize(e)
        }
        fn maximize(&self, e: &ElementData) -> Result<()> {
            self.inner.maximize(e)
        }
        fn restore(&self, e: &ElementData) -> Result<()> {
            self.inner.restore(e)
        }
        fn close(&self, e: &ElementData) -> Result<()> {
            self.inner.close(e)
        }
        fn move_to(&self, e: &ElementData, x: i32, y: i32) -> Result<()> {
            self.inner.move_to(e, x, y)
        }
        fn resize_to(&self, e: &ElementData, w: u32, h: u32) -> Result<()> {
            self.inner.resize_to(e, w, h)
        }
        fn subscribe(&self, e: &ElementData) -> Result<Subscription> {
            self.inner.subscribe(e)
        }
    }

    /// Provider modelling several Application registrations for one process —
    /// the shape the Linux AT-SPI registry can legitimately produce (two
    /// accessibles sharing a pid, as `desktop-testing.mdx` documents) and what
    /// `tag_focused` must disambiguate with the entry-level `active` flag. Two
    /// Application entries share pid 42; only the modal one reports `active`.
    struct SharedPidAppProvider {
        inner: Arc<crate::mock::MockProvider>,
    }

    impl SharedPidAppProvider {
        fn new() -> Self {
            Self {
                inner: build_provider(),
            }
        }

        /// An Application entry for the shared process (pid 42). `active`
        /// models the window-level foreground flag — only the entry whose
        /// window is actually in the foreground reports it.
        fn app(name: &str, handle: u64, active: bool) -> ElementData {
            ElementData {
                role: Role::Application,
                name: Some(name.to_string()),
                value: None,
                description: None,
                bounds: None,
                actions: vec![],
                states: crate::element::StateSet {
                    active,
                    ..crate::element::StateSet::default()
                },
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

    impl Provider for SharedPidAppProvider {
        fn splits_app_across_entries(&self) -> bool {
            true
        }
        fn list_apps(&self) -> Result<Vec<ElementData>> {
            Ok(vec![
                Self::app("Main", 200, false),
                Self::app("Modal", 201, true),
            ])
        }
        fn focused_app(&self) -> Result<ElementData> {
            Ok(Self::app("Modal", 201, true))
        }
        fn list_shell_surfaces(
            &self,
        ) -> Result<Vec<(crate::shell::ShellSurfaceKind, ElementData)>> {
            self.inner.list_shell_surfaces()
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
        fn raise(&self, e: &ElementData) -> Result<()> {
            self.inner.raise(e)
        }
        fn minimize(&self, e: &ElementData) -> Result<()> {
            self.inner.minimize(e)
        }
        fn maximize(&self, e: &ElementData) -> Result<()> {
            self.inner.maximize(e)
        }
        fn restore(&self, e: &ElementData) -> Result<()> {
            self.inner.restore(e)
        }
        fn close(&self, e: &ElementData) -> Result<()> {
            self.inner.close(e)
        }
        fn move_to(&self, e: &ElementData, x: i32, y: i32) -> Result<()> {
            self.inner.move_to(e, x, y)
        }
        fn resize_to(&self, e: &ElementData, w: u32, h: u32) -> Result<()> {
            self.inner.resize_to(e, w, h)
        }
        fn subscribe(&self, e: &ElementData) -> Result<Subscription> {
            self.inner.subscribe(e)
        }
    }

    /// Provider modelling the Linux AT-SPI shape where one process registers
    /// twice and each entry exposes **distinct** windows (a main application
    /// plus a dialog surfaced as its own app node). `App::by_pid` resolves a
    /// single entry, so without the aggregation in [`App::windows_with`] the
    /// sibling entry's window would be silently omitted.
    struct SplitPidAppProvider {
        inner: Arc<crate::mock::MockProvider>,
        /// When true the second entry reports the same window as the first
        /// (same stable id) — the registry can surface one window under both
        /// entries, and the listing must not duplicate it.
        overlap: bool,
        /// Whether the fixture claims `splits_app_across_entries`. The merge
        /// tests build the Linux shape (`true`); the gate test builds a
        /// provider that violates the contract by returning two same-pid
        /// entries while claiming `false`, so `windows_with` must skip the
        /// merge.
        claims_split: bool,
        /// `list_apps` call counter — the gate test asserts `windows_with`
        /// does not re-enumerate when the provider claims a single entry.
        list_apps_calls: std::sync::atomic::AtomicUsize,
    }

    impl SplitPidAppProvider {
        fn new(overlap: bool) -> Self {
            Self {
                inner: build_provider(),
                overlap,
                claims_split: true,
                list_apps_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn new_claiming_single_entry(overlap: bool) -> Self {
            Self {
                inner: build_provider(),
                overlap,
                claims_split: false,
                list_apps_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn app(handle: u64) -> ElementData {
            ElementData {
                role: Role::Application,
                name: Some("SharedApp".to_string()),
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

        fn window(name: &str, handle: u64, stable_id: &str) -> ElementData {
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
                stable_id: Some(stable_id.to_string()),
                pid: Some(42),
                raw: Default::default(),
                handle,
            }
        }
    }

    impl Provider for SplitPidAppProvider {
        fn splits_app_across_entries(&self) -> bool {
            self.claims_split
        }
        fn list_apps(&self) -> Result<Vec<ElementData>> {
            self.list_apps_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![Self::app(700), Self::app(701)])
        }
        fn focused_app(&self) -> Result<ElementData> {
            Ok(Self::app(700))
        }
        fn list_shell_surfaces(
            &self,
        ) -> Result<Vec<(crate::shell::ShellSurfaceKind, ElementData)>> {
            self.inner.list_shell_surfaces()
        }
        fn get_children(&self, e: Option<&ElementData>) -> Result<Vec<ElementData>> {
            match e {
                Some(el) if matches!(el.role, Role::Application) && el.pid == Some(42) => {
                    if el.handle == 701 {
                        let modal = Self::window("Modal", 101, "path/modal");
                        Ok(if self.overlap {
                            vec![Self::window("Main", 100, "path/main")]
                        } else {
                            vec![modal]
                        })
                    } else {
                        Ok(vec![Self::window("Main", 100, "path/main")])
                    }
                }
                _ => self.inner.get_children(e),
            }
        }
        fn get_parent(&self, e: &ElementData) -> Result<Option<ElementData>> {
            if matches!(e.role, Role::Window | Role::Dialog) && e.pid == Some(42) {
                if e.stable_id.as_deref() == Some("path/main") {
                    return Ok(Some(Self::app(700)));
                }
                return Ok(Some(Self::app(701)));
            }
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
        fn raise(&self, e: &ElementData) -> Result<()> {
            self.inner.raise(e)
        }
        fn minimize(&self, e: &ElementData) -> Result<()> {
            self.inner.minimize(e)
        }
        fn maximize(&self, e: &ElementData) -> Result<()> {
            self.inner.maximize(e)
        }
        fn restore(&self, e: &ElementData) -> Result<()> {
            self.inner.restore(e)
        }
        fn close(&self, e: &ElementData) -> Result<()> {
            self.inner.close(e)
        }
        fn move_to(&self, e: &ElementData, x: i32, y: i32) -> Result<()> {
            self.inner.move_to(e, x, y)
        }
        fn resize_to(&self, e: &ElementData, w: u32, h: u32) -> Result<()> {
            self.inner.resize_to(e, w, h)
        }
        fn subscribe(&self, e: &ElementData) -> Result<Subscription> {
            self.inner.subscribe(e)
        }
    }

    #[test]
    fn windows_with_merges_windows_of_same_pid_application_entries() {
        // `App::by_pid` resolves the first same-pid entry only; the Linux
        // registry can register a second entry (the dialog's own app node),
        // and `windows_with` must merge its windows — the D2 gap the review
        // flagged: `by_pid(pid).windows()` used to silently omit them.
        let provider: Arc<dyn Provider> = Arc::new(SplitPidAppProvider::new(false));
        let app = App::by_pid_with(Arc::clone(&provider), 42, Duration::ZERO)
            .expect("pid 42 must resolve in the split fixture");
        let windows =
            App::windows_with(Arc::clone(&provider), &app.data).expect("windows_with must succeed");
        let names: Vec<&str> = windows
            .iter()
            .map(|w| w.data().name.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            names,
            vec!["Main", "Modal"],
            "both entries' windows must list"
        );
    }

    #[test]
    fn windows_with_deduplicates_a_window_reported_by_two_entries() {
        // The registry can surface one window under both Application entries
        // (same stable id); the merged listing must not duplicate it.
        let provider: Arc<dyn Provider> = Arc::new(SplitPidAppProvider::new(true));
        let app = App::by_pid_with(Arc::clone(&provider), 42, Duration::ZERO)
            .expect("pid 42 must resolve in the split fixture");
        let windows =
            App::windows_with(Arc::clone(&provider), &app.data).expect("windows_with must succeed");
        let names: Vec<&str> = windows
            .iter()
            .map(|w| w.data().name.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            names,
            vec!["Main"],
            "a window shared by entries must list once"
        );
    }

    #[test]
    fn windows_with_skips_the_pid_merge_for_single_entry_providers() {
        // macOS and Windows guarantee one Application entry per pid, so the
        // same-pid merge must not run there: `windows_with` trusts the
        // provider's `splits_app_across_entries` claim. This fixture
        // deliberately violates the contract — `list_apps` returns two
        // same-pid entries while the provider claims `false` — and the test
        // pins the trust: the sibling entry's window must NOT be merged, and
        // `list_apps` must not be re-enumerated at all. That re-enumeration is
        // also the macOS duplicate-window bug: a rebuilt entry carries fresh
        // handles, so an unclaimed merge duplicates windows without an
        // AXIdentifier instead of deduplicating them.
        let provider = Arc::new(SplitPidAppProvider::new_claiming_single_entry(false));
        let any: Arc<dyn Provider> = provider.clone();
        let app = App::by_pid_with(Arc::clone(&any), 42, Duration::ZERO)
            .expect("pid 42 must resolve in the split fixture");
        let calls_after_by_pid = provider
            .list_apps_calls
            .load(std::sync::atomic::Ordering::SeqCst);
        let windows =
            App::windows_with(Arc::clone(&any), &app.data).expect("windows_with must succeed");
        let names: Vec<&str> = windows
            .iter()
            .map(|w| w.data().name.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            names,
            vec!["Main"],
            "a provider claiming one entry per pid must not merge a sibling entry's window"
        );
        assert_eq!(
            provider
                .list_apps_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            calls_after_by_pid,
            "windows_with must not re-enumerate list_apps on a one-entry-per-pid provider"
        );
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
    fn list_with_tags_foreground_app_as_foreground() {
        // MockProvider::focused_app reports the application root (pid 1234) as
        // foreground. It's the lone pid-matching entry (the macOS/Linux shape),
        // so even though the app root itself carries active=false it is tagged
        // via the n_matches == 1 rule and comes back `is_foreground()`.
        let provider: Arc<dyn Provider> = build_provider();
        let apps = App::list_with(provider).expect("list must succeed");
        assert_eq!(apps.len(), 1);
        assert!(
            !apps[0].data.states.active,
            "the app root itself is not an active window"
        );
        assert!(
            apps[0].is_foreground(),
            "the sole pid-matching entry must be tagged foreground by list_with"
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
        assert!(app.is_foreground());
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
            !apps[0].is_foreground(),
            "no app should be foreground when focused_app reports none"
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
    fn windows_with_lists_both_windows_of_shared_pid() {
        // The unified shape: ONE Application node per process whose children
        // are the process's top-level windows (issue #304: main + modal, in
        // enumeration order). Both windows must survive the enumeration, and
        // the per-app `windows()` convenience must agree with the explicit
        // `windows_with` call.
        let provider: Arc<dyn Provider> = Arc::new(MultiWindowProvider::new());
        let app = App::by_pid_with(Arc::clone(&provider), 42, Duration::ZERO)
            .expect("pid 42 must resolve in the multi-window fixture");
        let windows =
            App::windows_with(Arc::clone(&provider), &app.data).expect("windows_with must succeed");
        let names: Vec<&str> = windows
            .iter()
            .map(|w| w.data().name.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(names, vec!["Main", "Modal"], "both windows must list");
        let via_app = app.windows().expect("App::windows must succeed");
        let names2: Vec<&str> = via_app
            .iter()
            .map(|w| w.data().name.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(names2, vec!["Main", "Modal"]);
    }

    #[test]
    fn windows_rejects_non_application_elements() {
        // "Windows of an app" is defined only for Application elements — the
        // app-node-unified contract every platform reports. Asking a window
        // for its windows can no longer mean anything, so it must fail with
        // the role named in the error (tenet 6), not silently answer
        // "no windows".
        let app = mock_app();
        let window = app
            .windows()
            .expect("windows must succeed")
            .pop()
            .expect("the mock app owns a window");
        let err = App::windows_with(Arc::clone(app.provider()), window.data())
            .expect_err("windows_with on a window must fail");
        assert!(
            matches!(
                &err,
                Error::ActionNotSupported { action, role: Role::Window } if action == "windows"
            ),
            "expected ActionNotSupported naming the window role, got {err:?}"
        );
    }

    #[test]
    fn windows_on_mock_returns_main_window_child() {
        // The macOS/Linux shape: the app element's `role=Window` children.
        let app = mock_app();
        let windows = app.windows().expect("windows must succeed");
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].data().name.as_deref(), Some("Main Window"));
    }

    #[test]
    fn closed_window_leaves_windows_enumeration() {
        // Real providers drop a closed window from the tree; the mock must
        // model that too, or a close regression would pass against the mock
        // and fail on real platforms. A minimized window (also visible=false)
        // must by contrast STAY listed — `closed`, not `visible`, is the
        // removal signal.
        let app = mock_app();
        let windows = app.windows().expect("windows must succeed");
        assert_eq!(
            windows.len(),
            1,
            "mock app must expose exactly its main window before any mutation"
        );
        windows[0].minimize().expect("minimize must succeed");
        assert_eq!(
            app.windows().expect("windows must succeed").len(),
            1,
            "a minimized window is visible=false but must stay listed"
        );
        let windows = app.windows().expect("windows must succeed");
        windows[0].close().expect("close must succeed");
        assert_eq!(
            app.windows().expect("windows must succeed").len(),
            0,
            "a closed window must drop out of the enumeration"
        );
    }

    #[test]
    fn closed_window_stale_handle_is_dead() {
        // The `closed` flag must make a window vanish not only from
        // `App::windows()` but from every access through a stale handle:
        // `get_children` and `get_parent` both treat it as gone, exactly as
        // real providers do after destroying the element. A mock that
        // resolved a closed window's subtree would pass tests that fail on
        // real platforms.
        let app = mock_app();
        let windows = app.windows().expect("windows must succeed");
        let window = windows[0].clone();
        // A descendant captured before close — real providers destroy the
        // whole subtree with the window, so this handle must die too.
        let button = app
            .locator("button")
            .elements()
            .expect("buttons must resolve before the window closes")
            .pop()
            .expect("the mock app owns a button");
        window.close().expect("close must succeed");

        assert!(
            window
                .children()
                .expect("children of a closed window must resolve")
                .is_empty(),
            "a closed window must not resolve children through a stale handle"
        );
        assert!(
            window
                .parent()
                .expect("parent of a closed window must resolve")
                .is_none(),
            "a closed window must not resolve a parent through a stale handle"
        );
        assert!(
            button
                .parent()
                .expect("parent of a stale descendant must resolve")
                .is_none(),
            "a closed window's descendant must not resolve a parent"
        );
        assert!(
            matches!(button.press(), Err(Error::Platform { .. })),
            "a closed window's descendant must not remain actionable"
        );
        // `windows_with` on a window is a role error after the app-node
        // unification (a window is not an Application, and the self-inclusion
        // question it used to answer is gone); a closed window's liveness is
        // asserted through the app instead — `closed_window_leaves_windows_enumeration`
        // covers the empty listing, this asserts the role gate itself.
        assert!(
            matches!(
                App::windows_with(Arc::clone(app.provider()), window.data()),
                Err(Error::ActionNotSupported { action, role: Role::Window }) if action == "windows"
            ),
            "windows_with on a window must be ActionNotSupported, not a listing"
        );
        // The verbs are the same rule with a handle: act on a closed window
        // and the mock must fail, not silently mutate a dead node and report
        // success (which would let stale-handle regressions pass against the
        // mock, tenet 1).
        for action in [
            "raise",
            "minimize",
            "maximize",
            "restore",
            "close",
            "move_to",
            "resize_to",
        ] {
            let err = match action {
                "raise" => window
                    .raise()
                    .expect_err("raise on a closed window must fail"),
                "minimize" => window
                    .minimize()
                    .expect_err("minimize on a closed window must fail"),
                "maximize" => window
                    .maximize()
                    .expect_err("maximize on a closed window must fail"),
                "restore" => window
                    .restore()
                    .expect_err("restore on a closed window must fail"),
                "close" => window
                    .close()
                    .expect_err("close on a closed window must fail"),
                "move_to" => window
                    .move_to(0, 0)
                    .expect_err("move_to on a closed window must fail"),
                "resize_to" => window
                    .resize_to(100, 100)
                    .expect_err("resize_to on a closed window must fail"),
                _ => unreachable!(),
            };
            assert!(
                matches!(err, Error::Platform { .. }),
                "{action} on a closed window must fail with Platform, got {err:?}"
            );
        }
    }

    #[test]
    fn perform_action_window_verbs_match_the_typed_verbs() {
        // `perform_action` is the generic escape hatch; the window names are
        // well-known, so the mock must route them to the typed methods
        // exactly like the real providers (see WindowsProvider: uia.rs). A
        // mock that only recorded the string would let a test that fails on
        // every platform pass against it — and the payload verbs take no data
        // on this path, so they are rejected surfaceably (tenet 1).
        let app = mock_app();
        let windows = app.windows().expect("windows must succeed");
        let window = windows[0].clone();
        window
            .perform_action("minimize")
            .expect("perform_action(minimize) must succeed");
        let re_listed = app.windows().expect("windows must succeed");
        assert_eq!(
            re_listed[0].states.minimized,
            Some(true),
            "perform_action(\"minimize\") must really minimize the window"
        );
        assert!(
            matches!(
                window.perform_action("move_to"),
                Err(Error::InvalidActionData { .. })
            ),
            "perform_action(\"move_to\") must be rejected without coordinates"
        );
        assert!(
            matches!(
                window.perform_action("resize_to"),
                Err(Error::InvalidActionData { .. })
            ),
            "perform_action(\"resize_to\") must be rejected without dimensions"
        );
    }

    #[test]
    fn list_with_tags_only_the_active_window_of_a_shared_pid() {
        // Regression for issue #304: a process owning several top-level
        // windows (main window + modal dialog) must surface as one entry per
        // registration sharing the pid in `App::list_with` — the old pid
        // dedup silently dropped one. Both Application entries share pid 42,
        // which `focused_app` reports as foreground, but tagging is
        // window-precise: only the *active* entry (the modal) comes back
        // `is_foreground()`, not every entry of the process.
        let provider: Arc<dyn Provider> = Arc::new(SharedPidAppProvider::new());
        let apps = App::list_with(provider).expect("list must succeed");
        assert_eq!(
            apps.len(),
            2,
            "both registrations of the shared pid must appear"
        );
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert!(
            names.contains(&"Main"),
            "main window must be listed: {names:?}"
        );
        assert!(
            names.contains(&"Modal"),
            "modal window must be listed: {names:?}"
        );
        let foreground: Vec<&str> = apps
            .iter()
            .filter(|a| a.is_foreground())
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(
            foreground,
            vec!["Modal"],
            "only the active entry must be tagged foreground, got {foreground:?}"
        );
    }

    #[test]
    fn list_with_tags_none_when_shared_pid_has_no_active_window() {
        // Several Application entries share the foreground pid but none
        // reports `active` (the actual foreground window wasn't enumerable).
        // Window-precise tagging must then tag *none* of them — honest, not a
        // fallback to "tag them all" (tenet 1).
        struct NoActiveMultiWindow {
            inner: Arc<crate::mock::MockProvider>,
        }
        impl Provider for NoActiveMultiWindow {
            fn list_apps(&self) -> Result<Vec<ElementData>> {
                Ok(vec![
                    SharedPidAppProvider::app("Main", 200, false),
                    SharedPidAppProvider::app("Modal", 201, false),
                ])
            }
            fn focused_app(&self) -> Result<ElementData> {
                // Reports pid 42 as foreground, but no enumerated entry is
                // active.
                Ok(SharedPidAppProvider::app("Ghost", 202, false))
            }
            fn list_shell_surfaces(
                &self,
            ) -> Result<Vec<(crate::shell::ShellSurfaceKind, ElementData)>> {
                self.inner.list_shell_surfaces()
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
            fn raise(&self, e: &ElementData) -> Result<()> {
                self.inner.raise(e)
            }
            fn minimize(&self, e: &ElementData) -> Result<()> {
                self.inner.minimize(e)
            }
            fn maximize(&self, e: &ElementData) -> Result<()> {
                self.inner.maximize(e)
            }
            fn restore(&self, e: &ElementData) -> Result<()> {
                self.inner.restore(e)
            }
            fn close(&self, e: &ElementData) -> Result<()> {
                self.inner.close(e)
            }
            fn move_to(&self, e: &ElementData, x: i32, y: i32) -> Result<()> {
                self.inner.move_to(e, x, y)
            }
            fn resize_to(&self, e: &ElementData, w: u32, h: u32) -> Result<()> {
                self.inner.resize_to(e, w, h)
            }
            fn subscribe(&self, e: &ElementData) -> Result<Subscription> {
                self.inner.subscribe(e)
            }
        }

        let provider: Arc<dyn Provider> = Arc::new(NoActiveMultiWindow {
            inner: build_provider(),
        });
        let apps = App::list_with(provider).expect("list must succeed");
        assert_eq!(apps.len(), 2);
        assert!(
            apps.iter().all(|a| !a.is_foreground()),
            "no window may be tagged foreground when none is active"
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
            app.is_foreground(),
            "the app returned by foreground_with must always be is_foreground()"
        );
    }

    #[test]
    #[allow(deprecated)]
    fn focused_is_deprecated_alias_for_is_foreground() {
        // `focused()` is retained only as a deprecated alias; it must return
        // exactly what `is_foreground()` returns.
        let provider: Arc<dyn Provider> = build_provider();
        let apps = App::list_with(provider).expect("list must succeed");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].focused(), apps[0].is_foreground());
        assert!(apps[0].focused());
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
