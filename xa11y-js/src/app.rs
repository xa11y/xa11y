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
    /// Explicit accessibility source; overrides XA11Y_ACCESSIBILITY_SOURCE.
    pub backend: Option<String>,
    /// Poll the accessibility API until the app appears or this many
    /// milliseconds elapse. When omitted, the process-wide default applies —
    /// 5000ms (5 seconds) unless overridden via `setDefaultTimeout()` or the
    /// `XA11Y_DEFAULT_TIMEOUT` environment variable. Pass `0` for a single
    /// attempt with no waiting. Only "not found" errors trigger a retry;
    /// other errors fail fast.
    pub timeout: Option<f64>,
}

/// Options for `App.enableWebAccessibility`.
#[napi(object)]
pub struct WebAccessibilityOptions {
    /// How many milliseconds to wait for the web content to appear once the
    /// app has taken the request. When omitted, the process-wide default
    /// applies: 5000ms (5 seconds) unless overridden via
    /// `setDefaultTimeout()` or the `XA11Y_DEFAULT_TIMEOUT` environment
    /// variable. Pass `0` to check once.
    pub timeout: Option<f64>,
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
            timeout_ms: options.as_ref().and_then(|o| o.timeout),
            backend: options.and_then(|o| o.backend),
        })
    }

    /// Find an application by process ID.
    ///
    /// See {@link App.byName} for the `options.timeout` behaviour.
    #[napi(ts_return_type = "Promise<App>")]
    pub fn by_pid(pid: f64, options: Option<AppLookupOptions>) -> AsyncTask<FindByPidTask> {
        AsyncTask::new(FindByPidTask {
            pid,
            timeout_ms: options.as_ref().and_then(|o| o.timeout),
            backend: options.and_then(|o| o.backend),
        })
    }

    /// Resolve the application that currently holds the system foreground.
    ///
    /// Uses the platform's direct foreground query rather than enumerating and
    /// tagging by pid. The result is the foreground *process*'s `Application`
    /// node — one per process on every platform — not the exact window holding
    /// the focus; on Windows that is the synthesized node of the foreground
    /// process (the modal case, issues #304/#305), and this stays reliable
    /// when an app shows a modal dialog. To reach the exact foreground window,
    /// call `windows()` and pick the entry reporting `active`. Polls while
    /// nothing holds focus; see {@link App.byName} for the `options.timeout`
    /// behaviour. The returned app has `isForeground === true`.
    #[napi(ts_return_type = "Promise<App>")]
    pub fn foreground(options: Option<AppLookupOptions>) -> AsyncTask<ForegroundTask> {
        AsyncTask::new(ForegroundTask {
            timeout_ms: options.as_ref().and_then(|o| o.timeout),
            backend: options.and_then(|o| o.backend),
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

    /// Whether this application is the foreground application.
    ///
    /// Named `isForeground` because "focused" is reserved for element-level
    /// keyboard focus (`Element.focused`) elsewhere in the API; this is the
    /// foreground-*application* flag. Populated for apps obtained via
    /// {@link App.list}. A point-in-time snapshot taken when the `App` was
    /// resolved.
    ///
    /// Tagging is window-precise. On Windows apps surface as one synthesized
    /// `Application` node per process, so a pid match there is unambiguous; on
    /// Linux the AT-SPI registry can surface several entries for one pid, and
    /// only the entry reporting the window-level `active` flag gets
    /// `isForeground`. Use {@link App.foreground} to resolve the foreground
    /// application directly, then pick the exact foreground window from its
    /// `windows()`.
    #[napi(getter)]
    pub fn is_foreground(&self) -> bool {
        self.data.states.focused
    }

    /// Whether this application is the foreground application.
    ///
    /// @deprecated Use {@link App.isForeground}. "focused" refers to element
    /// keyboard focus elsewhere in the API.
    #[napi(getter)]
    pub fn focused(&self) -> bool {
        self.data.states.focused
    }

    /// Create a `Locator` scoped to this application's accessibility tree.
    ///
    /// The locator re-resolves `selector` on every operation, so it always
    /// targets the current UI state — see the `Locator` class for the full
    /// API.
    #[napi]
    pub fn locator(&self, selector: String) -> Locator {
        Locator::from_inner(xa11y::Locator::new_for_app(
            self.provider.clone(),
            self.data.clone(),
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

    /// List the top-level windows of this application.
    ///
    /// Each call queries the provider; results are not cached. The windows
    /// are the application's top-level windows with role `window` or
    /// `dialog`, in enumeration order. On Windows the application entry is a
    /// synthesized process node whose children are the process's top-level
    /// windows (main window plus modal dialogs). On Linux the answer is
    /// process-complete: `App::windows_with` merges the filtered children of
    /// every same-pid AT-SPI Application entry (an app that registers several
    /// entries reports its whole process), so the results need not be the
    /// direct children of this node and no single z-order spans them.
    /// Calling `windows` on a non-application element fails with
    /// `ActionNotSupportedError`.
    #[napi(ts_return_type = "Promise<Element[]>")]
    pub fn windows(&self) -> AsyncTask<AppWindowsTask> {
        AsyncTask::new(AppWindowsTask {
            data: self.data.clone(),
            provider: self.provider.clone(),
        })
    }

    /// Turn on the accessibility tree for the app's web content, then wait
    /// for that content to appear.
    ///
    /// Chromium and Electron build the accessibility tree for their web
    /// content only when a client asks for it. Until then a selector for an
    /// element on the page finds nothing. This makes that request; xa11y
    /// never makes it implicitly.
    ///
    /// On macOS it sets `AXManualAccessibility` (Electron) on the
    /// application element and reads it back, rejecting with
    /// `AccessibilityNotEnabledError` when it does not read back as set;
    /// relaunch such an app with `--force-renderer-accessibility`. It never
    /// sets `AXEnhancedUserInterface`. It does not check that the app is
    /// Chromium-based: an app that keeps the value counts as having taken
    /// the request, and the call then rejects with `TimeoutError` because no
    /// web content appears. The attribute stays set to true afterwards and
    /// is not restored. It then waits up to
    /// `options.timeout` ms until some
    /// `web_area` has a child, rejecting with `TimeoutError` and a diagnosis
    /// otherwise. On Linux and Windows it rejects with
    /// `ActionNotSupportedError` saying what to do instead: on Linux, launch
    /// the app with `--force-renderer-accessibility`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn enable_web_accessibility(
        &self,
        options: Option<WebAccessibilityOptions>,
    ) -> AsyncTask<EnableWebAccessibilityTask> {
        AsyncTask::new(EnableWebAccessibilityTask {
            data: self.data.clone(),
            provider: self.provider.clone(),
            timeout_ms: options.and_then(|o| o.timeout),
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

    /// Subscribe to accessibility events from this application. See
    /// `Element.subscribe` for the options. The public `App` class in
    /// `index.js` overrides this; the native function takes `(kinds, buffer)`
    /// positionally and resolves to the internal `_NativeSubscription`.
    #[napi(
        ts_args_type = "options?: SubscribeOptions | null",
        ts_return_type = "Promise<Subscription>"
    )]
    pub fn subscribe(
        &self,
        kinds: Option<Vec<String>>,
        buffer: Option<f64>,
    ) -> AsyncTask<AppSubscribeTask> {
        AsyncTask::new(AppSubscribeTask {
            data: self.data.clone(),
            provider: self.provider.clone(),
            options: crate::subscription::subscribe_options(kinds, buffer),
        })
    }

    /// Read the subtree into a bounded capture: every node read, pre-order,
    /// with which limits cut it. Read breadth-first, so when `maxNodes` cuts,
    /// the top of the tree is complete. `detail` adds `'text'`,
    /// `'formatting'` (text plus its attribute runs, at most `maxRuns` in
    /// total) and/or `'relations'`. `timeout` (ms) resolves with what was
    /// read when it runs out, with `truncated.byTime`; calling `cancel()` on
    /// the `cancel` token does the same, with `truncated.byCancel`.
    ///
    /// The root is the application; its children are those of every native
    /// registration of the process, as in `children()`.
    #[napi(
        ts_args_type = "options?: CaptureOptions | null",
        ts_return_type = "Promise<Capture>"
    )]
    pub fn capture(
        &self,
        options: Option<crate::capture::CaptureOptions>,
    ) -> napi::Result<AsyncTask<crate::text::Blocking<crate::capture::Capture>>> {
        let options = crate::capture::parse_options(options)?;
        let app = xa11y::App::from_data(self.provider.clone(), self.data.clone());
        Ok(AsyncTask::new(crate::text::Blocking::new(move || {
            app.capture(&options).map(Into::into)
        })))
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
    pub fn tree(&self, max_depth: Option<f64>) -> napi::Result<AsyncTask<TreeTask>> {
        let max_depth = crate::types::checked_depth(max_depth)?;
        Ok(AsyncTask::new(TreeTask::new_for_app(
            self.data.clone(),
            self.provider.clone(),
            max_depth,
        )))
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
    pub fn dump(&self, max_depth: Option<f64>) -> napi::Result<AsyncTask<DumpTask>> {
        let max_depth = crate::types::checked_depth(max_depth)?;
        Ok(AsyncTask::new(DumpTask::new_for_app(
            self.data.clone(),
            self.provider.clone(),
            max_depth,
        )))
    }
}

// ── Tasks ──────────────────────────────────────────────────────────────

/// Resolve the optional lookup timeout (ms): an explicit value wins; `None`
/// falls back to the process-wide default (`setDefaultTimeout()` /
/// `XA11Y_DEFAULT_TIMEOUT`, else 5 seconds) — matching the auto-wait
/// timeout used elsewhere in the API (e.g. `Locator.waitAttached`).
///
/// Called first in each task's `compute`, so a negative, fractional or `NaN`
/// timeout rejects with `InvalidActionDataError` before any platform call.
pub(crate) fn effective_timeout_ms(timeout_ms: Option<f64>) -> napi::Result<Duration> {
    match timeout_ms {
        Some(ms) => crate::types::checked_millis(ms, "timeout").map_err(map_err),
        None => xa11y::default_timeout().map_err(map_err),
    }
}

pub struct FindByNameTask {
    backend: Option<String>,
    name: String,
    timeout_ms: Option<f64>,
}

impl Task for FindByNameTask {
    type Output = xa11y::App;
    type JsValue = App;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let timeout = effective_timeout_ms(self.timeout_ms)?;
        let provider = match self.backend.as_deref() {
            Some(b) => xa11y::provider_for_backend(b).map_err(map_err)?,
            None => crate::provider()?,
        };
        xa11y::App::by_name_with(provider, &self.name, timeout).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(App::from_core(output))
    }
}

pub struct FindByPidTask {
    backend: Option<String>,
    /// The raw JS number; checked first in `compute`.
    pid: f64,
    timeout_ms: Option<f64>,
}

impl Task for FindByPidTask {
    type Output = xa11y::App;
    type JsValue = App;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let pid = crate::types::checked_offset(self.pid, "pid")?;
        let timeout = effective_timeout_ms(self.timeout_ms)?;
        let provider = match self.backend.as_deref() {
            Some(b) => xa11y::provider_for_backend(b).map_err(map_err)?,
            None => crate::provider()?,
        };
        xa11y::App::by_pid_with(provider, pid, timeout).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(App::from_core(output))
    }
}

pub struct ForegroundTask {
    backend: Option<String>,
    timeout_ms: Option<f64>,
}

impl Task for ForegroundTask {
    type Output = xa11y::App;
    type JsValue = App;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let timeout = effective_timeout_ms(self.timeout_ms)?;
        let provider = match self.backend.as_deref() {
            Some(b) => xa11y::provider_for_backend(b).map_err(map_err)?,
            None => crate::provider()?,
        };
        xa11y::App::foreground_with(provider, timeout).map_err(map_err)
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
        xa11y::App::from_data(self.provider.clone(), self.data.clone())
            .children()
            .map(|children| {
                children
                    .into_iter()
                    .map(|child| child.data().clone())
                    .collect()
            })
            .map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output
            .into_iter()
            .map(|d| Element::new(d, self.provider.clone()))
            .collect())
    }
}

pub struct AppWindowsTask {
    data: xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
}

impl Task for AppWindowsTask {
    type Output = Vec<xa11y::ElementData>;
    type JsValue = Vec<Element>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        xa11y::App::windows_with(self.provider.clone(), &self.data)
            .map(|windows| windows.into_iter().map(|w| w.data().clone()).collect())
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
    /// The parsed options, or why they were refused: `compute` rejects with
    /// that before subscribing.
    options: xa11y::Result<xa11y::SubscribeOptions>,
}

impl Task for AppSubscribeTask {
    type Output = xa11y::Subscription;
    type JsValue = NativeSubscription;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let options = self.options.clone().map_err(map_err)?;
        xa11y::App::from_data(self.provider.clone(), self.data.clone())
            .subscribe_with(&options)
            .map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        // `compute` only ran past `?` on `Ok` options.
        let options = self.options.clone().unwrap_or_default();
        Ok(NativeSubscription::with_options(
            output,
            self.provider.clone(),
            &options,
        ))
    }
}

pub struct EnableWebAccessibilityTask {
    data: xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
    timeout_ms: Option<f64>,
}

impl Task for EnableWebAccessibilityTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let timeout = effective_timeout_ms(self.timeout_ms)?;
        xa11y::App::from_data(self.provider.clone(), self.data.clone())
            .enable_web_accessibility(timeout)
            .map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(())
    }
}
