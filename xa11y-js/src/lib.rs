//! xa11y Node.js bindings (napi-rs).
//!
//! All provider-touching methods are exposed as async functions that run the
//! blocking a11y work on napi's tokio worker pool, so they never block the
//! Node event loop.

#![deny(clippy::all)]

use std::sync::Arc;

#[macro_use]
extern crate napi_derive;

mod app;
mod element;
mod errors;
mod input;
mod locator;
mod mock;
mod screenshot;
mod subscription;
mod types;

pub(crate) use errors::map_err;

fn provider() -> napi::Result<Arc<dyn xa11y::Provider>> {
    xa11y::provider().map_err(map_err)
}

/// Resolve an optional per-call timeout (seconds) to a `Duration`: an
/// explicit value wins; `None` falls back to the process-wide default (see
/// [`set_default_timeout`] / the `XA11Y_DEFAULT_TIMEOUT` environment
/// variable).
pub(crate) fn effective_timeout_secs(
    timeout_seconds: Option<f64>,
) -> napi::Result<std::time::Duration> {
    match timeout_seconds {
        Some(secs) if secs.is_finite() && secs >= 0.0 => {
            Ok(std::time::Duration::from_secs_f64(secs))
        }
        Some(secs) => Err(napi::Error::new(
            napi::Status::InvalidArg,
            format!("timeoutSeconds must be a non-negative, finite number of seconds, got {secs}"),
        )),
        None => xa11y::default_timeout().map_err(map_err),
    }
}

/// Set the process-wide default timeout, in seconds.
///
/// Becomes the default for every auto-waiting action method, `wait*` call,
/// and app lookup (`App.byName` / `App.byPid`) that doesn't pass an explicit
/// timeout. An explicit per-call timeout always wins. Takes precedence over
/// the `XA11Y_DEFAULT_TIMEOUT` environment variable (seconds, read once on
/// first use).
///
/// Pass `0` for "single attempt, no polling" semantics. Throws for negative
/// or non-finite values.
#[napi(js_name = "setDefaultTimeout")]
pub fn set_default_timeout(seconds: f64) -> napi::Result<()> {
    if !(seconds.is_finite() && seconds >= 0.0) {
        return Err(napi::Error::new(
            napi::Status::InvalidArg,
            format!("seconds must be a non-negative, finite number, got {seconds}"),
        ));
    }
    xa11y::set_default_timeout(std::time::Duration::from_secs_f64(seconds));
    Ok(())
}

/// Get the effective process-wide default timeout, in seconds.
///
/// Resolution order: the [`set_default_timeout`] value, else the
/// `XA11Y_DEFAULT_TIMEOUT` environment variable, else the built-in 5.0.
#[napi(js_name = "getDefaultTimeout")]
pub fn get_default_timeout() -> napi::Result<f64> {
    Ok(xa11y::default_timeout().map_err(map_err)?.as_secs_f64())
}

/// Create a top-level [`Locator`](locator::Locator) that searches from the
/// system accessibility root (across all applications).
#[napi(js_name = "locator")]
pub fn make_locator(selector: String) -> napi::Result<locator::Locator> {
    let provider = provider()?;
    Ok(locator::Locator::from_inner(xa11y::Locator::new(
        provider, None, &selector,
    )))
}
