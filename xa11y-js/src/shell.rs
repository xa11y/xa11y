//! JS `ShellSurface` class: the entry point for OS shell-surface queries.
//!
//! Shaped on [`crate::app::App`] — the same static factories, the same
//! `AsyncTask` blocking model, the same `locator` / `children` / `tree` /
//! `dump` / `asElement` surface. A consumer who knows `App` should be able to
//! guess this class.

use std::sync::Arc;

use napi::bindgen_prelude::{AsyncTask, Env, Task};

use crate::app::effective_timeout_ms;
use crate::element::{ChildrenTask, DumpTask, Element, TreeTask};
use crate::locator::Locator;
use crate::map_err;

/// An OS shell surface — the taskbar, a desktop panel or dock, the macOS menu
/// bar or a process's status items, the desktop, or a transient shell flyout.
///
/// Construct via {@link ShellSurface.list} or {@link ShellSurface.byKind}. A
/// `ShellSurface` is **not** an `Element` — it represents the surface as a
/// whole and provides {@link ShellSurface.locator} to search its accessibility
/// tree. Every surface wraps a real platform element; xa11y adds a tag, never
/// a node.
///
/// Enumerating surfaces and reading their trees never opens, closes, focuses,
/// or presses anything. Where content only exists after a press (the Windows
/// tray overflow, macOS Control Center), press the real, advertised element
/// and then look the surface up again:
///
/// ```js
/// const taskbar = await ShellSurface.byKind('taskbar', { timeout: 0 });
/// await taskbar.locator("button[name='Show Hidden Icons']").press();
/// const flyout = await ShellSurface.byKind('flyout', { timeout: 3000 });
/// ```
#[napi]
pub struct ShellSurface {
    kind: xa11y::ShellSurfaceKind,
    name: String,
    pid: Option<u32>,
    data: xa11y::ElementData,
    provider: Arc<dyn xa11y::Provider>,
}

impl ShellSurface {
    pub(crate) fn from_core(surface: xa11y::ShellSurface) -> Self {
        // Core keeps its provider handle private; the surface's own root
        // element is the supported way to reach it.
        let root = surface.as_element();
        let provider = Arc::clone(root.provider());
        Self {
            kind: surface.kind,
            name: surface.name.clone(),
            pid: surface.pid,
            data: surface.data,
            provider,
        }
    }
}

/// Parse a snake_case shell-surface kind name into an
/// [`xa11y::ShellSurfaceKind`].
///
/// Kinds cross the boundary as the identically-spelled snake_case strings the
/// Python binding, the CLI's `--shell` flag and MCP's `shell` parameter use.
/// Parsing happens before any provider call, so a bad argument can never reach
/// the accessibility API.
///
/// The kinds the error names are derived from `ShellSurfaceKind::ALL` rather
/// than written out: the enum is `#[non_exhaustive]`, so nothing here would
/// fail to compile when a variant is added, and a hand-written list would keep
/// naming eight kinds out of nine in the one message that says what is
/// accepted.
pub(crate) fn parse_kind(name: &str) -> napi::Result<xa11y::ShellSurfaceKind> {
    xa11y::ShellSurfaceKind::from_snake_case(name).ok_or_else(|| {
        let expected = xa11y::ShellSurfaceKind::ALL
            .iter()
            .map(|k| format!("'{}'", k.to_snake_case()))
            .collect::<Vec<_>>()
            .join(", ");
        napi::Error::from_reason(format!(
            "XA11Y_INVALID_ACTION_DATA: unknown shell surface kind: {name}. Expected one of \
             {expected}"
        ))
    })
}

/// Options for `ShellSurface.byKind`.
#[napi(object)]
pub struct ShellSurfaceLookupOptions {
    /// Poll the accessibility API until exactly one surface of the kind
    /// appears or this many milliseconds elapse. When omitted, the
    /// process-wide default applies — 5000ms (5 seconds) unless overridden via
    /// `setDefaultTimeout()` or the `XA11Y_DEFAULT_TIMEOUT` environment
    /// variable. Pass `0` for a single attempt with no waiting. Only "no such
    /// surface yet" triggers a retry; an enumeration failure, and an ambiguous
    /// shell, fail fast.
    pub timeout: Option<u32>,
}

#[napi]
impl ShellSurface {
    /// List the OS shell surfaces currently on screen.
    ///
    /// The listing is live: `flyout` surfaces appear only while they are open,
    /// and enumerating never opens, closes, or presses anything. A platform
    /// with no surface of a given kind simply returns none.
    #[napi(ts_return_type = "Promise<ShellSurface[]>")]
    pub fn list() -> AsyncTask<ListShellSurfacesTask> {
        AsyncTask::new(ListShellSurfacesTask {})
    }

    /// Wait for **exactly one** surface of `kind`.
    ///
    /// `kind` is the snake_case name — `'menu_bar'`, `'status_items'`,
    /// `'taskbar'`, `'panel'`, `'dock'`, `'desktop'`, `'flyout'`,
    /// `'unknown'` — and is rejected with `InvalidActionDataError` before any
    /// accessibility call when it names no known kind.
    ///
    /// Polls until the surface exists or `options.timeout` (ms) elapses; see
    /// {@link App.byName} for the default-timeout behaviour. Pass
    /// `{ timeout: 0 }` for a single attempt with no waiting.
    ///
    /// Rejects with `SelectorNotMatchedError` both when no surface of `kind`
    /// is present and when **several** are — ambiguity is refused rather than
    /// first-matched, and the error's `candidates` name the surfaces that were
    /// found so the caller can disambiguate with {@link ShellSurface.list} and
    /// a pid.
    #[napi(ts_return_type = "Promise<ShellSurface>")]
    pub fn by_kind(
        kind: String,
        options: Option<ShellSurfaceLookupOptions>,
    ) -> napi::Result<AsyncTask<ByKindTask>> {
        Ok(AsyncTask::new(ByKindTask {
            kind: parse_kind(&kind)?,
            timeout_ms: options.and_then(|o| o.timeout),
        }))
    }

    /// What this surface is, as a snake_case string (e.g. `"taskbar"`,
    /// `"status_items"`).
    #[napi(getter)]
    pub fn kind(&self) -> String {
        self.kind.to_snake_case().to_string()
    }

    /// Human-readable name: the owning app for per-app surfaces (`"Safari"`
    /// menu bar, `"Arq"` status items), the platform's own name otherwise
    /// (`"Taskbar"`, `"Dock"`). Falls back to the kind's spelling when the
    /// platform vends no name for the root.
    #[napi(getter)]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// Owning process where the platform reports one honestly, else `null`.
    ///
    /// On macOS this is the true owner. On Windows it is the *host*
    /// (explorer.exe / ShellHost.exe) because UIA carries no per-icon owner —
    /// reported as the host, never faked. On Linux it is the panel process.
    #[napi(getter)]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Create a `Locator` scoped to this surface's accessibility tree.
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

    /// Get direct children of the surface root.
    #[napi(ts_return_type = "Promise<Element[]>")]
    pub fn children(&self) -> AsyncTask<ChildrenTask> {
        AsyncTask::new(ChildrenTask::new(self.data.clone(), self.provider.clone()))
    }

    /// Get an `Element` handle for the surface root.
    ///
    /// Useful for invoking Element-level methods (`children()`, `parent()`,
    /// etc.) without going through a locator. Synchronous — the surface
    /// already holds the root's accessibility data.
    #[napi]
    pub fn as_element(&self) -> Element {
        Element::new(self.data.clone(), self.provider.clone())
    }

    /// Capture this surface's accessibility tree as a recursive snapshot,
    /// rooted at the surface element.
    ///
    /// `maxDepth` limits traversal depth: `0` = only the surface node, `1` =
    /// surface + direct children, and so on. Omit for the full subtree.
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

    /// Render this surface's accessibility tree as an indented string.
    ///
    /// Returns the string without printing it. The primary inspection helper
    /// — call `console.log(await surface.dump())` to discover the role and
    /// name of every element in the surface before writing selectors.
    ///
    /// For the same output from the shell, use `xa11y tree --shell KIND`.
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

pub struct ListShellSurfacesTask {}

impl Task for ListShellSurfacesTask {
    type Output = Vec<xa11y::ShellSurface>;
    type JsValue = Vec<ShellSurface>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let provider = crate::provider()?;
        xa11y::ShellSurface::list_with(provider).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output.into_iter().map(ShellSurface::from_core).collect())
    }
}

pub struct ByKindTask {
    kind: xa11y::ShellSurfaceKind,
    timeout_ms: Option<u32>,
}

impl Task for ByKindTask {
    type Output = xa11y::ShellSurface;
    type JsValue = ShellSurface;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let timeout = effective_timeout_ms(self.timeout_ms)?;
        let provider = crate::provider()?;
        xa11y::ShellSurface::by_kind_with(provider, self.kind, timeout).map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(ShellSurface::from_core(output))
    }
}
