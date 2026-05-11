//! Test-only JS entry points for the shared mock Provider.
//!
//! The mock itself (tree topology, Provider impl, action log) lives in
//! `xa11y-core::mock` behind the `test-support` feature. This module only
//! wraps it in the napi-rs exports that the JS unit tests consume.

use std::sync::Arc;

use crate::app::App;
use crate::locator::Locator;
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
    let app = xa11y::App::by_name_with(provider, "TestApp").map_err(crate::map_err)?;
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
