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

/// Create a top-level [`Locator`](locator::Locator) that searches from the
/// system accessibility root (across all applications).
#[napi(js_name = "locator")]
pub fn make_locator(selector: String) -> napi::Result<locator::Locator> {
    let provider = provider()?;
    Ok(locator::Locator::from_inner(xa11y::Locator::new(
        provider, None, &selector,
    )))
}
