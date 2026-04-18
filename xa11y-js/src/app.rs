//! JS `App` class: the entry point for accessibility queries.

use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::{AsyncTask, Env, Task};

use crate::element::Element;
use crate::locator::Locator;
use crate::map_err;
use crate::subscription::NativeSubscription;

#[napi]
pub struct App {
    name: String,
    pid: Option<u32>,
    data: xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
}

impl App {
    fn from_core(app: xa11y::App) -> Self {
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
    /// If set, poll the accessibility API until the app appears or this
    /// many milliseconds elapse. Useful when the app may not yet be
    /// registered (e.g. just-launched). Only "not found" errors trigger a
    /// retry; other errors fail fast.
    pub timeout: Option<u32>,
}

#[napi]
impl App {
    /// Find an application by exact name.
    #[napi(ts_return_type = "Promise<App>")]
    pub fn by_name(name: String, options: Option<AppLookupOptions>) -> AsyncTask<FindByNameTask> {
        AsyncTask::new(FindByNameTask {
            name,
            timeout: timeout_from(options),
        })
    }

    /// Find an application by process ID.
    #[napi(ts_return_type = "Promise<App>")]
    pub fn by_pid(pid: u32, options: Option<AppLookupOptions>) -> AsyncTask<FindByPidTask> {
        AsyncTask::new(FindByPidTask {
            pid,
            timeout: timeout_from(options),
        })
    }

    /// List all running applications with an accessibility tree.
    #[napi(ts_return_type = "Promise<App[]>")]
    pub fn list() -> AsyncTask<ListAppsTask> {
        AsyncTask::new(ListAppsTask {})
    }

    #[napi(getter)]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    #[napi(getter)]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Create a [`Locator`] scoped to this application's accessibility tree.
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

    /// Subscribe to accessibility events from this application.
    #[napi(ts_return_type = "Promise<_NativeSubscription>")]
    pub fn subscribe(&self) -> AsyncTask<AppSubscribeTask> {
        AsyncTask::new(AppSubscribeTask {
            data: self.data.clone(),
            provider: self.provider.clone(),
        })
    }
}

// ── Tasks ──────────────────────────────────────────────────────────────

fn timeout_from(options: Option<AppLookupOptions>) -> Duration {
    options
        .and_then(|o| o.timeout)
        .map(|ms| Duration::from_millis(ms.into()))
        .unwrap_or(Duration::ZERO)
}

pub struct FindByNameTask {
    name: String,
    timeout: Duration,
}

impl Task for FindByNameTask {
    type Output = xa11y::App;
    type JsValue = App;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let provider = crate::provider()?;
        xa11y::App::by_name_with_timeout(provider, &self.name, self.timeout).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(App::from_core(output))
    }
}

pub struct FindByPidTask {
    pid: u32,
    timeout: Duration,
}

impl Task for FindByPidTask {
    type Output = xa11y::App;
    type JsValue = App;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let provider = crate::provider()?;
        xa11y::App::by_pid_with_timeout(provider, self.pid, self.timeout).map_err(map_err)
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
