//! Test-only JS entry points for the shared mock Provider.
//!
//! The mock itself (tree topology, Provider impl, action log) lives in
//! `xa11y-core::mock` behind the `test-support` feature. This module only
//! wraps it in the napi-rs exports that the JS unit tests consume.

use std::sync::Arc;

use crate::locator::Locator;
use crate::subscription::NativeSubscription;

#[napi]
pub struct TestActionProbe {
    provider: Arc<xa11y::mock::MockProvider>,
}

#[napi]
impl TestActionProbe {
    #[napi]
    pub fn locator(&self) -> Locator {
        Locator::from_inner(xa11y::Locator::new(
            self.provider.clone() as Arc<dyn xa11y::Provider>,
            None,
            "application",
        ))
    }

    #[napi(ts_return_type = "Array<[number, string, string | null]>")]
    pub fn actions(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.provider
                .actions()
                .into_iter()
                .map(|(handle, action, data)| {
                    serde_json::json!([handle, action, data])
                })
                .collect(),
        )
    }

    #[napi]
    pub fn clear_actions(&self) {
        self.provider.clear_actions();
    }
}

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

/// Create a mock provider probe with a locator and inspectable action log.
/// Used only from the JS unit tests — not part of the public API.
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
