use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::element::{Element, ElementData, TreeNode};
use crate::error::{Error, Result};
use crate::event_provider::Subscription;
use crate::locator::Locator;
use crate::provider::Provider;

/// Polling interval shared by all timeout-bearing lookups.
const LOOKUP_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Run `attempt` repeatedly until it succeeds or `timeout` elapses, treating
/// `SelectorNotMatched` as a "not yet" signal. All other errors short-circuit.
///
/// `Duration::ZERO` performs exactly one attempt — identical to a non-polling
/// call. On timeout, returns the last `SelectorNotMatched` error.
fn poll_lookup<F>(timeout: Duration, mut attempt: F) -> Result<App>
where
    F: FnMut() -> Result<App>,
{
    let start = Instant::now();
    loop {
        match attempt() {
            Ok(app) => return Ok(app),
            Err(e @ Error::SelectorNotMatched { .. }) => {
                if start.elapsed() >= timeout {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        }
        std::thread::sleep(LOOKUP_POLL_INTERVAL);
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
        Self::find_matching(provider, timeout, predicate, || {
            "application matching predicate".to_string()
        })
    }

    /// Shared predicate-based discovery loop. `describe` supplies the
    /// [`Error::SelectorNotMatched`] selector string so name/pid lookups keep
    /// their specific, actionable error messages while sharing one match loop.
    fn find_matching<F, D>(
        provider: Arc<dyn Provider>,
        timeout: Duration,
        predicate: F,
        describe: D,
    ) -> Result<Self>
    where
        F: Fn(&ElementData) -> Result<bool>,
        D: Fn() -> String,
    {
        poll_lookup(timeout, || {
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
            let apps = provider.list_apps()?;
            for data in apps {
                if predicate(&data)? {
                    return Ok(Self::from_data(Arc::clone(&provider), data));
                }
            }
            Err(Error::SelectorNotMatched {
                selector: describe(),
            })
        })
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
    pub fn by_name_with(
        provider: Arc<dyn Provider>,
        name: &str,
        timeout: Duration,
    ) -> Result<Self> {
        Self::find_matching(
            provider,
            timeout,
            |d| Ok(d.name.as_deref() == Some(name)),
            || format!(r#"application[name="{}"]"#, name),
        )
    }

    /// Find an application by process ID, using an explicit provider.
    ///
    /// Prefer `App::by_pid` from the `xa11y` crate which uses the global
    /// singleton provider. See [`by_name_with`](Self::by_name_with) for the
    /// timeout / polling semantics.
    pub fn by_pid_with(provider: Arc<dyn Provider>, pid: u32, timeout: Duration) -> Result<Self> {
        Self::find_matching(
            provider,
            timeout,
            |d| Ok(d.pid == Some(pid)),
            || format!("application with pid={}", pid),
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
        let datas = provider.list_apps()?;
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
    fn try_find_with_ok_false_keeps_polling_then_times_out() {
        let provider: Arc<dyn Provider> = build_provider();
        // `Ok(false)` is "not yet" — with a zero timeout that's one attempt
        // and then a normal not-found result (no error propagation).
        let err = App::try_find_with(provider, Duration::ZERO, |_| Ok(false))
            .expect_err("an always-Ok(false) predicate must not match");
        assert!(matches!(err, Error::SelectorNotMatched { .. }));
    }
}
