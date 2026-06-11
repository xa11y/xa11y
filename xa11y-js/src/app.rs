//! JS `App` class: the entry point for accessibility queries.

use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::{AsyncTask, Env, Task};

use crate::element::{DumpTask, Element, TreeTask};
use crate::locator::Locator;
use crate::map_err;
use crate::subscription::NativeSubscription;

/// A running application — the entry point for accessibility queries.
///
/// Construct via {@link App.byName}, {@link App.byPid}, or {@link App.list}.
/// An `App` is **not** an `Element` — it represents the application as a
/// whole and provides {@link App.locator} to search its accessibility tree.
#[napi]
pub struct App {
    name: String,
    pid: Option<u32>,
    data: xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
}

impl App {
    pub(crate) fn from_core(app: xa11y::App) -> Self {
        Self {
            name: app.name.clone(),
            pid: app.pid,
            provider: app.provider().clone(),
            data: app.data,
        }
    }
}

/// Options for `App.byName` / `App.byPid`.
#[napi(object)]
pub struct AppLookupOptions {
    /// Poll the accessibility API until the app appears or this many
    /// milliseconds elapse. When omitted, the process-wide default applies —
    /// 5000ms (5 seconds) unless overridden via `setDefaultTimeout()` or the
    /// `XA11Y_DEFAULT_TIMEOUT` environment variable. Pass `0` for a single
    /// attempt with no waiting. Only "not found" errors trigger a retry;
    /// other errors fail fast.
    pub timeout: Option<u32>,
}

#[napi]
impl App {
    /// Find an application by exact name.
    ///
    /// Polls the accessibility API until the app appears or
    /// `options.timeout` (ms) elapses. When omitted, the process-wide
    /// default applies — 5 seconds unless overridden via
    /// `setDefaultTimeout()` / `XA11Y_DEFAULT_TIMEOUT`. Pass `{ timeout: 0 }`
    /// for a single attempt with no waiting. Only "not found" errors trigger
    /// a retry; permission errors and the like fail fast.
    ///
    /// Rejects with `PermissionDeniedError` if accessibility is not enabled,
    /// or `SelectorNotMatchedError` if no matching app is found.
    #[napi(ts_return_type = "Promise<App>")]
    pub fn by_name(name: String, options: Option<AppLookupOptions>) -> AsyncTask<FindByNameTask> {
        AsyncTask::new(FindByNameTask {
            name,
            timeout_ms: options.and_then(|o| o.timeout),
        })
    }

    /// Find an application by process ID.
    ///
    /// See {@link App.byName} for the `options.timeout` behaviour.
    #[napi(ts_return_type = "Promise<App>")]
    pub fn by_pid(pid: u32, options: Option<AppLookupOptions>) -> AsyncTask<FindByPidTask> {
        AsyncTask::new(FindByPidTask {
            pid,
            timeout_ms: options.and_then(|o| o.timeout),
        })
    }

    /// List all running applications with an accessibility tree.
    #[napi(ts_return_type = "Promise<App[]>")]
    pub fn list() -> AsyncTask<ListAppsTask> {
        AsyncTask::new(ListAppsTask {})
    }

    /// The application's human-readable name (e.g. `"Safari"`).
    #[napi(getter)]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// The application's process ID, or `null` if the platform does not
    /// expose one for this app.
    #[napi(getter)]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Create a `Locator` scoped to this application's accessibility tree.
    ///
    /// The locator re-resolves `selector` on every operation, so it always
    /// targets the current UI state — see the `Locator` class for the full
    /// API.
    #[napi]
    pub fn locator(&self, selector: String) -> Locator {
        Locator::from_inner(xa11y::Locator::new(
            self.provider.clone(),
            Some(self.data.clone()),
            &selector,
        ))
    }

    /// Get direct children (typically windows) of this application.
    #[napi(ts_return_type = "Promise<Element[]>")]
    pub fn children(&self) -> AsyncTask<AppChildrenTask> {
        AsyncTask::new(AppChildrenTask {
            data: self.data.clone(),
            provider: self.provider.clone(),
        })
    }

    /// Get an `Element` handle for the application root.
    ///
    /// Useful for invoking Element-level methods (`children()`, `parent()`,
    /// etc.) without going through a locator. Synchronous — the App already
    /// holds the application's accessibility data.
    #[napi]
    pub fn as_element(&self) -> Element {
        Element::new(self.data.clone(), self.provider.clone())
    }

    /// Subscribe to accessibility events from this application.
    #[napi(ts_return_type = "Promise<_NativeSubscription>")]
    pub fn subscribe(&self) -> AsyncTask<AppSubscribeTask> {
        AsyncTask::new(AppSubscribeTask {
            data: self.data.clone(),
            provider: self.provider.clone(),
        })
    }

    /// Capture this application's accessibility tree as a recursive snapshot,
    /// rooted at the application element.
    ///
    /// `maxDepth` limits traversal depth: `0` = only the application node,
    /// `1` = application + direct children (typically windows), and so on.
    /// Omit for the full subtree.
    #[napi(
        ts_args_type = "maxDepth?: number | null",
        ts_return_type = "Promise<TreeNode>"
    )]
    pub fn tree(&self, max_depth: Option<u32>) -> AsyncTask<TreeTask> {
        AsyncTask::new(TreeTask::new(
            self.data.clone(),
            self.provider.clone(),
            max_depth.map(|d| d as usize),
        ))
    }

    /// Render this application's accessibility tree as an indented string.
    ///
    /// Returns the string without printing it. The primary inspection helper
    /// — call `console.log(await app.dump())` to discover the role and name
    /// of every element in the app before writing selectors.
    ///
    /// For the same output from the shell, use `xa11y tree --app NAME`.
    #[napi(
        ts_args_type = "maxDepth?: number | null",
        ts_return_type = "Promise<string>"
    )]
    pub fn dump(&self, max_depth: Option<u32>) -> AsyncTask<DumpTask> {
        AsyncTask::new(DumpTask::new(
            self.data.clone(),
            self.provider.clone(),
            max_depth.map(|d| d as usize),
        ))
    }
}

// ── Tasks ──────────────────────────────────────────────────────────────

/// Resolve the optional lookup timeout (ms): an explicit value wins; `None`
/// falls back to the process-wide default (`setDefaultTimeout()` /
/// `XA11Y_DEFAULT_TIMEOUT`, else 5 seconds) — matching the auto-wait
/// timeout used elsewhere in the API (e.g. `Locator.waitAttached`).
fn effective_timeout_ms(timeout_ms: Option<u32>) -> napi::Result<Duration> {
    match timeout_ms {
        Some(ms) => Ok(Duration::from_millis(ms.into())),
        None => xa11y::default_timeout().map_err(map_err),
    }
}

pub struct FindByNameTask {
    name: String,
    timeout_ms: Option<u32>,
}

impl Task for FindByNameTask {
    type Output = xa11y::App;
    type JsValue = App;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let timeout = effective_timeout_ms(self.timeout_ms)?;
        let provider = crate::provider()?;
        xa11y::App::by_name_with(provider, &self.name, timeout).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(App::from_core(output))
    }
}

pub struct FindByPidTask {
    pid: u32,
    timeout_ms: Option<u32>,
}

impl Task for FindByPidTask {
    type Output = xa11y::App;
    type JsValue = App;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let timeout = effective_timeout_ms(self.timeout_ms)?;
        let provider = crate::provider()?;
        xa11y::App::by_pid_with(provider, self.pid, timeout).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(App::from_core(output))
    }
}

pub struct ListAppsTask {}

impl Task for ListAppsTask {
    type Output = Vec<xa11y::App>;
    type JsValue = Vec<App>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let provider = crate::provider()?;
        xa11y::App::list_with(provider).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output.into_iter().map(App::from_core).collect())
    }
}

pub struct AppChildrenTask {
    data: xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
}

impl Task for AppChildrenTask {
    type Output = Vec<xa11y::ElementData>;
    type JsValue = Vec<Element>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.provider
            .get_children(Some(&self.data))
            .map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output
            .into_iter()
            .map(|d| Element::new(d, self.provider.clone()))
            .collect())
    }
}

pub struct AppSubscribeTask {
    data: xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
}

impl Task for AppSubscribeTask {
    type Output = xa11y::Subscription;
    type JsValue = NativeSubscription;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.provider.subscribe(&self.data).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(NativeSubscription::new(output, self.provider.clone()))
    }
}
