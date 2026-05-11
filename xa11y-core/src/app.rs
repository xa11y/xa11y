use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::element::{Element, ElementData, TreeNode};
use crate::error::{Error, Result};
use crate::event_provider::Subscription;
use crate::locator::Locator;
use crate::provider::Provider;
use crate::role::Role;
use crate::selector::{
    AttrFilter, Combinator, MatchOp, RoleMatch, Selector, SelectorSegment, SimpleSelector,
};

/// Build a single-segment selector that matches `role` with an exact-name filter.
///
/// Constructed directly from the selector AST rather than via string
/// interpolation, so an application name containing characters that are
/// special in the selector grammar (`"`, `]`, `\`) still produces a
/// well-formed selector that matches the literal name.
fn role_named(role: Role, name: &str) -> Selector {
    Selector {
        segments: vec![SelectorSegment {
            combinator: Combinator::Root,
            simple: SimpleSelector {
                role: Some(RoleMatch::Normalized(role)),
                filters: vec![AttrFilter {
                    attr: "name".to_string(),
                    op: MatchOp::Exact,
                    value: name.to_string(),
                }],
                nth: None,
            },
        }],
    }
}

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
        poll_lookup(timeout, || {
            // Try application role first (Linux/macOS), then window role
            // (Windows — UIA has no Application node at the top level).
            //
            // Selectors are built directly from the AST so app names
            // containing `"`, `]`, or other characters significant in the
            // selector grammar don't break or require escaping.
            //
            // `find_elements` returns `Ok(vec![])` for "no match" and
            // reserves `Err(_)` for real failures (permission denied,
            // platform errors, malformed selectors). We only fall through on
            // an empty result — real errors must propagate so callers can
            // distinguish "app not found" from "accessibility is broken".
            let app_selector = role_named(Role::Application, name);
            let results = provider.find_elements(None, &app_selector, Some(1), Some(0))?;
            if let Some(data) = results.into_iter().next() {
                return Ok(Self::from_data(Arc::clone(&provider), data));
            }
            let win_selector = role_named(Role::Window, name);
            let results = provider.find_elements(None, &win_selector, Some(1), Some(0))?;
            let data = results
                .into_iter()
                .next()
                .ok_or_else(|| Error::SelectorNotMatched {
                    selector: format!(r#"application[name="{}"]"#, name),
                })?;
            Ok(Self::from_data(Arc::clone(&provider), data))
        })
    }

    /// Find an application by process ID, using an explicit provider.
    ///
    /// Prefer `App::by_pid` from the `xa11y` crate which uses the global
    /// singleton provider. See [`by_name_with`](Self::by_name_with) for the
    /// timeout / polling semantics.
    pub fn by_pid_with(provider: Arc<dyn Provider>, pid: u32, timeout: Duration) -> Result<Self> {
        poll_lookup(timeout, || {
            // Try application role first, then window role (Windows
            // fallback). Propagate real errors; only fall through when the
            // role yielded no matching element.
            for role in ["application", "window"] {
                let selector = Selector::parse(role)?;
                let results = provider.find_elements(None, &selector, None, Some(0))?;
                if let Some(data) = results.into_iter().find(|d| d.pid == Some(pid)) {
                    return Ok(Self::from_data(Arc::clone(&provider), data));
                }
            }
            Err(Error::SelectorNotMatched {
                selector: format!("application with pid={}", pid),
            })
        })
    }

    /// List all running applications, using an explicit provider.
    ///
    /// Prefer `App::list` from the `xa11y` crate which uses the global
    /// singleton provider.
    pub fn list_with(provider: Arc<dyn Provider>) -> Result<Vec<Self>> {
        // Collect application role elements (Linux/macOS), then add window
        // role elements (Windows fallback) for any not already found by PID.
        // Real errors from either lookup propagate to the caller.
        let mut apps = Vec::new();
        let mut seen_pids = std::collections::HashSet::new();

        for role in ["application", "window"] {
            let selector = Selector::parse(role)?;
            let results = provider.find_elements(None, &selector, None, Some(0))?;
            for d in results {
                if let Some(pid) = d.pid {
                    if !seen_pids.insert(pid) {
                        continue; // already found via application role
                    }
                }
                apps.push(Self::from_data(Arc::clone(&provider), d));
            }
        }

        Ok(apps)
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
    use crate::selector::{matches_simple, Combinator};
    use serde_json::json;

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
    fn role_named_preserves_literal_name_with_special_chars() {
        // Regression for a selector-injection bug: an earlier `by_name`
        // implementation used `format!(r#"application[name="{}"]"#, name)`
        // without escaping, so a
        // name containing `"` terminated the attribute value early and either
        // failed to parse or matched the wrong element. The AST-based builder
        // stores the literal name in the filter value.
        let name = r#"My "Weird" App ]["#;
        let sel = role_named(Role::Application, name);
        assert_eq!(sel.segments.len(), 1);
        assert_eq!(sel.segments[0].combinator, Combinator::Root);
        let simple = &sel.segments[0].simple;
        assert_eq!(simple.filters.len(), 1);
        assert_eq!(simple.filters[0].attr, "name");
        assert_eq!(simple.filters[0].value, name);
    }

    #[test]
    fn role_named_matches_element_with_quoted_name() {
        // End-to-end: the constructed selector actually matches an element
        // whose name contains the special chars. Build a minimal ElementData
        // and verify `matches_simple`.
        let name = r#"Name"With"Quote"#;
        let data = ElementData {
            role: Role::Application,
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
            pid: Some(1),
            raw: std::collections::HashMap::from([("app_name".to_string(), json!(name))]),
            handle: 0,
        };
        let sel = role_named(Role::Application, name);
        assert!(matches_simple(&data, &sel.segments[0].simple));
    }
}
