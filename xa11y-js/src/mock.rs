//! Test-only JS entry points for the shared mock Provider.
//!
//! The mock itself (tree topology, Provider impl, action log) lives in
//! `xa11y-core::mock` behind the `test-support` feature. This module only
//! wraps it in the napi-rs exports that the JS unit tests consume.

use std::sync::Arc;

use crate::app::App;
use crate::locator::Locator;
use crate::shell::{parse_kind, ShellSurface};
use crate::subscription::NativeSubscription;

/// Create a mock `Locator` rooted at the shared synthetic tree. Used only
/// from the JS unit tests — not part of the public API.
#[napi(js_name = "_makeTestLocator")]
#[allow(
    dead_code,
    reason = "Exported via napi-derive for JS unit tests; the lib-test clippy build doesn't see the JS-side consumer"
)]
pub fn make_test_locator() -> Locator {
    let provider = xa11y::mock::build_provider();
    Locator::from_inner(xa11y::Locator::new(
        provider as Arc<dyn xa11y::Provider>,
        None,
        "application",
    ))
}

/// Create a mock `App` resolved against the shared synthetic tree
/// (`TestApp`). Used only from the JS unit tests — not part of the public API.
#[napi(js_name = "_makeTestApp")]
#[allow(
    dead_code,
    reason = "Exported via napi-derive for JS unit tests; the lib-test clippy build doesn't see the JS-side consumer"
)]
pub fn make_test_app() -> napi::Result<App> {
    let provider = xa11y::mock::build_provider() as Arc<dyn xa11y::Provider>;
    // Resolve via the predicate finder (not `by_name_with`) so the returned
    // app is foreground-tagged — the mock reports its root as the focused app,
    // letting `App.isForeground` tests observe a `true` value.
    let app = xa11y::App::find_with(provider, std::time::Duration::ZERO, |d| {
        d.name.as_deref() == Some("TestApp")
    })
    .map_err(crate::map_err)?;
    Ok(App::from_core(app))
}

/// Test handle that pairs a mock `Locator` with read-access to the mock's
/// action log. Lets JS unit tests assert that an action method dispatched
/// to the expected provider call. Not part of the public API.
#[napi(js_name = "_TestActionProbe")]
#[allow(
    dead_code,
    reason = "Exported via napi-derive for JS unit tests; the lib-test clippy build doesn't see the JS-side consumer"
)]
pub struct TestActionProbe {
    provider: Arc<xa11y::mock::MockProvider>,
}

#[napi]
#[allow(
    dead_code,
    reason = "Exported via napi-derive for JS unit tests; the lib-test clippy build doesn't see the JS-side consumer"
)]
impl TestActionProbe {
    /// A `Locator` rooted at the shared synthetic tree, backed by the same
    /// provider whose action log this probe exposes.
    #[napi]
    pub fn locator(&self) -> Locator {
        Locator::from_inner(xa11y::Locator::new(
            self.provider.clone() as Arc<dyn xa11y::Provider>,
            None,
            "application",
        ))
    }

    /// Action log entries recorded so far, as `[handle, action, data?]`
    /// tuples. `data` is `null` for nullary actions, a stringified
    /// argument otherwise (matches the core mock's record format).
    #[napi(ts_return_type = "Array<[number, string, string | null]>")]
    pub fn actions(&self) -> Vec<(u32, String, Option<String>)> {
        self.provider
            .actions()
            .into_iter()
            .map(|(h, a, d)| (h as u32, a, d))
            .collect()
    }

    /// Clear the recorded action log.
    #[napi]
    pub fn clear(&self) {
        self.provider.clear_actions();
    }
}

/// Create a `_TestActionProbe` wrapping a fresh mock provider. Used by JS
/// unit tests to verify that action methods dispatch to the expected
/// provider call.
#[napi(js_name = "_makeTestActionProbe")]
#[allow(
    dead_code,
    reason = "Exported via napi-derive for JS unit tests; the lib-test clippy build doesn't see the JS-side consumer"
)]
pub fn make_test_action_probe() -> TestActionProbe {
    TestActionProbe {
        provider: xa11y::mock::build_provider(),
    }
}

/// List the mock's fixture shell surfaces (`taskbar` "Taskbar" and `desktop`
/// "Desktop"). Used only from the JS unit tests — the real
/// `ShellSurface.list()` resolves the platform singleton provider, which no
/// unit-test environment has. Not part of the public API.
#[napi(js_name = "_makeTestShellSurfaces")]
#[allow(
    dead_code,
    reason = "Exported via napi-derive for JS unit tests; the lib-test clippy build doesn't see the JS-side consumer"
)]
pub fn make_test_shell_surfaces() -> napi::Result<Vec<ShellSurface>> {
    let provider = xa11y::mock::build_provider() as Arc<dyn xa11y::Provider>;
    Ok(xa11y::ShellSurface::list_with(provider)
        .map_err(crate::map_err)?
        .into_iter()
        .map(ShellSurface::from_core)
        .collect())
}

/// Resolve one mock shell surface by kind, mirroring `ShellSurface.byKind`'s
/// parse-then-look-up shape against the mock provider. Not part of the public
/// API.
#[napi(js_name = "_makeTestShellSurfaceByKind")]
#[allow(
    dead_code,
    reason = "Exported via napi-derive for JS unit tests; the lib-test clippy build doesn't see the JS-side consumer"
)]
pub fn make_test_shell_surface_by_kind(kind: String) -> napi::Result<ShellSurface> {
    let kind = parse_kind(&kind)?;
    let provider = xa11y::mock::build_provider() as Arc<dyn xa11y::Provider>;
    // `Duration::ZERO` — a fixture never materialises later, so waiting would
    // only turn a miss into a slow miss.
    xa11y::ShellSurface::by_kind_with(provider, kind, std::time::Duration::ZERO)
        .map(ShellSurface::from_core)
        .map_err(crate::map_err)
}

/// [`make_test_shell_surface_by_kind`] against a provider that reports the
/// mock's taskbar twice — the ambiguous shell (two `panel` frames, status
/// items from several processes, a leftover flyout). Not part of the public
/// API.
#[napi(js_name = "_makeTestAmbiguousShellSurfaceByKind")]
#[allow(
    dead_code,
    reason = "Exported via napi-derive for JS unit tests; the lib-test clippy build doesn't see the JS-side consumer"
)]
pub fn make_test_ambiguous_shell_surface_by_kind(kind: String) -> napi::Result<ShellSurface> {
    let kind = parse_kind(&kind)?;
    let provider = Arc::new(DuplicateShellProvider {
        inner: xa11y::mock::build_provider(),
    }) as Arc<dyn xa11y::Provider>;
    xa11y::ShellSurface::by_kind_with(provider, kind, std::time::Duration::ZERO)
        .map(ShellSurface::from_core)
        .map_err(crate::map_err)
}

/// Wraps the shared mock provider but reports the taskbar surface twice.
/// Everything else delegates — the ambiguity is the only difference, so the
/// JS test observes exactly the refusal core builds for a real duplicated
/// surface.
struct DuplicateShellProvider {
    inner: Arc<xa11y::mock::MockProvider>,
}

impl xa11y::Provider for DuplicateShellProvider {
    fn list_shell_surfaces(
        &self,
    ) -> xa11y::Result<Vec<(xa11y::ShellSurfaceKind, xa11y::ElementData)>> {
        let mut all = self.inner.list_shell_surfaces()?;
        let Some(dup) = all
            .iter()
            .find(|(k, _)| *k == xa11y::ShellSurfaceKind::Taskbar)
            .cloned()
        else {
            return Err(xa11y::Error::selector_not_matched(
                "the mock fixture's taskbar surface",
            ));
        };
        all.push(dup);
        Ok(all)
    }
    fn raise(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.raise(e)
    }
    fn minimize(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.minimize(e)
    }
    fn maximize(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.maximize(e)
    }
    fn restore(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.restore(e)
    }
    fn close(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.close(e)
    }
    fn move_to(&self, e: &xa11y::ElementData, x: i32, y: i32) -> xa11y::Result<()> {
        self.inner.move_to(e, x, y)
    }
    fn resize_to(&self, e: &xa11y::ElementData, w: u32, h: u32) -> xa11y::Result<()> {
        self.inner.resize_to(e, w, h)
    }
    fn get_children(
        &self,
        e: Option<&xa11y::ElementData>,
    ) -> xa11y::Result<Vec<xa11y::ElementData>> {
        self.inner.get_children(e)
    }
    fn get_parent(&self, e: &xa11y::ElementData) -> xa11y::Result<Option<xa11y::ElementData>> {
        self.inner.get_parent(e)
    }
    fn list_apps(&self) -> xa11y::Result<Vec<xa11y::ElementData>> {
        self.inner.list_apps()
    }
    fn focused_app(&self) -> xa11y::Result<xa11y::ElementData> {
        self.inner.focused_app()
    }
    fn press(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.press(e)
    }
    fn focus(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.focus(e)
    }
    fn blur(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.blur(e)
    }
    fn toggle(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.toggle(e)
    }
    fn select(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.select(e)
    }
    fn expand(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.expand(e)
    }
    fn collapse(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.collapse(e)
    }
    fn show_menu(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.show_menu(e)
    }
    fn increment(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.increment(e)
    }
    fn decrement(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.decrement(e)
    }
    fn scroll_into_view(&self, e: &xa11y::ElementData) -> xa11y::Result<()> {
        self.inner.scroll_into_view(e)
    }
    fn set_value(&self, e: &xa11y::ElementData, v: &str) -> xa11y::Result<()> {
        self.inner.set_value(e, v)
    }
    fn set_numeric_value(&self, e: &xa11y::ElementData, v: f64) -> xa11y::Result<()> {
        self.inner.set_numeric_value(e, v)
    }
    fn type_text(&self, e: &xa11y::ElementData, t: &str) -> xa11y::Result<()> {
        self.inner.type_text(e, t)
    }
    fn set_text_selection(&self, e: &xa11y::ElementData, s: u32, end: u32) -> xa11y::Result<()> {
        self.inner.set_text_selection(e, s, end)
    }
    fn perform_action(&self, e: &xa11y::ElementData, a: &str) -> xa11y::Result<()> {
        self.inner.perform_action(e, a)
    }
    fn subscribe(&self, e: &xa11y::ElementData) -> xa11y::Result<xa11y::Subscription> {
        self.inner.subscribe(e)
    }
}

/// Create a `_NativeSubscription` whose backing channel has already been
/// disconnected. Used by tests to verify the worker loop terminates cleanly
/// on sender-drop rather than hanging.
#[napi(js_name = "_makeDisconnectedSubscription")]
#[allow(
    dead_code,
    reason = "Exported via napi-derive for JS unit tests; the lib-test clippy build doesn't see the JS-side consumer"
)]
pub fn make_disconnected_subscription() -> NativeSubscription {
    let provider = xa11y::mock::build_provider();
    NativeSubscription::new(
        xa11y::mock::disconnected_subscription(),
        provider as Arc<dyn xa11y::Provider>,
    )
}
