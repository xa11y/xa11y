//! JS `Locator` class: resilient element reference with auto-wait.

use std::time::Duration;

use napi::bindgen_prelude::{AsyncTask, Env, Task};

use crate::element::Element;
use crate::map_err;

/// A resilient element reference that re-queries on each interaction.
///
/// Locators never hold a live reference to a UI element. Instead, they
/// store a selector and resolve it on demand, making them immune to
/// staleness. Action methods (`press`, `typeText`, `toggle`, …) auto-wait
/// for the element to appear (up to 5 seconds by default) before acting.
///
/// Locators are cheap to clone — the chaining methods (`child`, `descendant`,
/// `nth`, `first`) return new locators rather than mutating in place.
///
/// @example
/// ```ts
/// const app = await App.byName('MyApp');
/// const save = app.locator("button[name='Save']");
/// await save.press();                  // auto-waits, then presses
/// await save.waitEnabled(10);          // wait up to 10 seconds
/// ```
#[napi]
pub struct Locator {
    inner: xa11y::Locator,
}

impl Locator {
    pub(crate) fn from_inner(inner: xa11y::Locator) -> Self {
        Self { inner }
    }
}

#[napi]
impl Locator {
    /// The CSS-like selector string for this locator.
    #[napi(getter)]
    pub fn selector(&self) -> String {
        self.inner.selector().to_string()
    }

    /// Return a new Locator that selects the *n*-th match (1-based).
    #[napi]
    pub fn nth(&self, n: u32) -> napi::Result<Self> {
        // Reject n == 0 at the binding boundary instead of forwarding to
        // `Locator::nth`, which asserts and panics (crashes Node).
        if n == 0 {
            return Err(crate::errors::map_err(xa11y::Error::InvalidActionData {
                message: "Locator.nth is 1-based; got 0".to_string(),
            }));
        }
        Ok(Self::from_inner(self.inner.clone().nth(n as usize)))
    }

    /// Return a new Locator that selects the first match.
    #[napi]
    pub fn first(&self) -> Self {
        Self::from_inner(self.inner.clone().first())
    }

    /// Return a new Locator scoped to direct children matching *selector*.
    #[napi]
    pub fn child(&self, selector: String) -> Self {
        Self::from_inner(self.inner.clone().child(&selector))
    }

    /// Return a new Locator scoped to descendants matching *selector*.
    #[napi]
    pub fn descendant(&self, selector: String) -> Self {
        Self::from_inner(self.inner.clone().descendant(&selector))
    }

    // ── Queries ────────────────────────────────────────────────────────

    /// Check whether a matching element exists (does **not** throw on miss).
    #[napi(ts_return_type = "Promise<boolean>")]
    pub fn exists(&self) -> AsyncTask<ExistsTask> {
        AsyncTask::new(ExistsTask {
            inner: self.inner.clone(),
        })
    }

    /// Count matching elements.
    #[napi(ts_return_type = "Promise<number>")]
    pub fn count(&self) -> AsyncTask<CountTask> {
        AsyncTask::new(CountTask {
            inner: self.inner.clone(),
        })
    }

    /// Resolve to a single [`Element`] snapshot. Throws `SelectorNotMatchedError`
    /// if no element matches.
    #[napi(ts_return_type = "Promise<Element>")]
    pub fn element(&self) -> AsyncTask<ElementTask> {
        AsyncTask::new(ElementTask {
            inner: self.inner.clone(),
        })
    }

    /// Resolve to all matching [`Element`] snapshots.
    #[napi(ts_return_type = "Promise<Element[]>")]
    pub fn elements(&self) -> AsyncTask<ElementsTask> {
        AsyncTask::new(ElementsTask {
            inner: self.inner.clone(),
        })
    }

    // ── Actions (each auto-waits, then performs the action) ────────────

    /// Click / invoke the matched element.
    ///
    /// Auto-waits for the element to exist before acting. For elements whose
    /// primary activation is `toggle` or `select` (checkbox, tab, radio),
    /// `press` dispatches to that semantic — there is no need to distinguish.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn press(&self) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::nullary(self.inner.clone(), ActionKind::Press))
    }

    /// Move keyboard focus to the matched element.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn focus(&self) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::nullary(self.inner.clone(), ActionKind::Focus))
    }

    /// Remove keyboard focus from the matched element.
    ///
    /// Not supported on Linux or Windows — on those platforms this rejects
    /// with `ActionNotSupportedError`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn blur(&self) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::nullary(self.inner.clone(), ActionKind::Blur))
    }

    /// Toggle a two- or three-state control (checkbox, switch, toggle button).
    #[napi(ts_return_type = "Promise<void>")]
    pub fn toggle(&self) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::nullary(self.inner.clone(), ActionKind::Toggle))
    }

    /// Expand a disclosure, menu, or tree item.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn expand(&self) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::nullary(self.inner.clone(), ActionKind::Expand))
    }

    /// Collapse a disclosure, menu, or tree item.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn collapse(&self) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::nullary(
            self.inner.clone(),
            ActionKind::Collapse,
        ))
    }

    /// Select the matched element (list item, tab, row).
    #[napi(js_name = "select", ts_return_type = "Promise<void>")]
    pub fn select_(&self) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::nullary(self.inner.clone(), ActionKind::Select))
    }

    /// Open the element's context menu.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn show_menu(&self) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::nullary(
            self.inner.clone(),
            ActionKind::ShowMenu,
        ))
    }

    /// Scroll the element into the visible area.
    ///
    /// No-op on macOS — the macOS accessibility API has no equivalent. Uses
    /// `Component.ScrollTo` on Linux and `ScrollItemPattern` on Windows.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn scroll_into_view(&self) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::nullary(
            self.inner.clone(),
            ActionKind::ScrollIntoView,
        ))
    }

    /// Increment a numeric value (slider, spin button) by its platform step.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn increment(&self) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::nullary(
            self.inner.clone(),
            ActionKind::Increment,
        ))
    }

    /// Decrement a numeric value (slider, spin button) by its platform step.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn decrement(&self) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::nullary(
            self.inner.clone(),
            ActionKind::Decrement,
        ))
    }

    /// Set the text value of the matched element. Replaces the entire value
    /// rather than inserting at the caret — use `typeText` for insertion.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn set_value(&self, value: String) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::with_text(
            self.inner.clone(),
            ActionKind::SetValue,
            value,
        ))
    }

    /// Set the numeric value of the matched element (slider, spin button).
    #[napi(ts_return_type = "Promise<void>")]
    pub fn set_numeric_value(&self, value: f64) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::with_num(
            self.inner.clone(),
            ActionKind::SetNumericValue,
            value,
        ))
    }

    /// Type `text` at the current caret position.
    ///
    /// Uses the platform accessibility API — never simulates keyboard events.
    /// For synthesised keystrokes (global shortcuts, drag gestures), use the
    /// `InputSim` surface instead.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn type_text(&self, text: String) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::with_text(
            self.inner.clone(),
            ActionKind::TypeText,
            text,
        ))
    }

    /// Select the text range from `start` to `end` (0-based character offsets).
    /// Rejects with `InvalidActionDataError` if `start > end`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn select_text(&self, start: u32, end: u32) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::with_range(
            self.inner.clone(),
            ActionKind::SelectText,
            start,
            end,
        ))
    }

    /// Perform a custom action by its snake_case name.
    ///
    /// Use this for actions the element advertises in its `actions` list
    /// that don't have a dedicated method. Rejects with
    /// `ActionNotSupportedError` if the element does not advertise `action`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn perform_action(&self, action: String) -> AsyncTask<ActionTask> {
        AsyncTask::new(ActionTask::with_text(
            self.inner.clone(),
            ActionKind::PerformAction,
            action,
        ))
    }

    // ── Waits ──────────────────────────────────────────────────────────
    //
    // Each wait polls the provider until its condition is satisfied or
    // `timeoutSeconds` elapses (default: 5s). Waits that expect the element
    // to be present resolve with the matched `Element`; waits that expect it
    // to be gone resolve with `undefined`.

    /// Wait for a matching element to become visible.
    /// Rejects with `TimeoutError` if still hidden after `timeoutSeconds`.
    #[napi(
        ts_args_type = "timeoutSeconds?: number",
        ts_return_type = "Promise<Element>"
    )]
    pub fn wait_visible(&self, timeout_seconds: Option<f64>) -> AsyncTask<WaitTask> {
        AsyncTask::new(WaitTask::returning(
            self.inner.clone(),
            WaitKind::Visible,
            timeout_seconds.unwrap_or(5.0),
        ))
    }

    /// Wait for a matching element to exist in the tree (may not be visible).
    /// Rejects with `TimeoutError` if no match appears within `timeoutSeconds`.
    #[napi(
        ts_args_type = "timeoutSeconds?: number",
        ts_return_type = "Promise<Element>"
    )]
    pub fn wait_attached(&self, timeout_seconds: Option<f64>) -> AsyncTask<WaitTask> {
        AsyncTask::new(WaitTask::returning(
            self.inner.clone(),
            WaitKind::Attached,
            timeout_seconds.unwrap_or(5.0),
        ))
    }

    /// Wait for the matching element to be removed from the tree.
    #[napi(
        ts_args_type = "timeoutSeconds?: number",
        ts_return_type = "Promise<void>"
    )]
    pub fn wait_detached(&self, timeout_seconds: Option<f64>) -> AsyncTask<WaitTask> {
        AsyncTask::new(WaitTask::absent(
            self.inner.clone(),
            WaitKind::Detached,
            timeout_seconds.unwrap_or(5.0),
        ))
    }

    /// Wait for the matching element to become enabled (interactive).
    #[napi(
        ts_args_type = "timeoutSeconds?: number",
        ts_return_type = "Promise<Element>"
    )]
    pub fn wait_enabled(&self, timeout_seconds: Option<f64>) -> AsyncTask<WaitTask> {
        AsyncTask::new(WaitTask::returning(
            self.inner.clone(),
            WaitKind::Enabled,
            timeout_seconds.unwrap_or(5.0),
        ))
    }

    /// Wait for the matching element to be hidden or removed.
    #[napi(
        ts_args_type = "timeoutSeconds?: number",
        ts_return_type = "Promise<void>"
    )]
    pub fn wait_hidden(&self, timeout_seconds: Option<f64>) -> AsyncTask<WaitTask> {
        AsyncTask::new(WaitTask::absent(
            self.inner.clone(),
            WaitKind::Hidden,
            timeout_seconds.unwrap_or(5.0),
        ))
    }

    /// Wait for the matching element to become disabled (non-interactive).
    #[napi(
        ts_args_type = "timeoutSeconds?: number",
        ts_return_type = "Promise<Element>"
    )]
    pub fn wait_disabled(&self, timeout_seconds: Option<f64>) -> AsyncTask<WaitTask> {
        AsyncTask::new(WaitTask::returning(
            self.inner.clone(),
            WaitKind::Disabled,
            timeout_seconds.unwrap_or(5.0),
        ))
    }

    /// Wait for the matching element to receive keyboard focus.
    #[napi(
        ts_args_type = "timeoutSeconds?: number",
        ts_return_type = "Promise<Element>"
    )]
    pub fn wait_focused(&self, timeout_seconds: Option<f64>) -> AsyncTask<WaitTask> {
        AsyncTask::new(WaitTask::returning(
            self.inner.clone(),
            WaitKind::Focused,
            timeout_seconds.unwrap_or(5.0),
        ))
    }

    /// Wait for the matching element to lose keyboard focus.
    #[napi(
        ts_args_type = "timeoutSeconds?: number",
        ts_return_type = "Promise<Element>"
    )]
    pub fn wait_unfocused(&self, timeout_seconds: Option<f64>) -> AsyncTask<WaitTask> {
        AsyncTask::new(WaitTask::returning(
            self.inner.clone(),
            WaitKind::Unfocused,
            timeout_seconds.unwrap_or(5.0),
        ))
    }
}

// ── Query tasks ────────────────────────────────────────────────────────

pub struct ExistsTask {
    inner: xa11y::Locator,
}
impl Task for ExistsTask {
    type Output = bool;
    type JsValue = bool;
    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.inner.exists().map_err(map_err)
    }
    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct CountTask {
    inner: xa11y::Locator,
}
impl Task for CountTask {
    type Output = u32;
    type JsValue = u32;
    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.inner.count().map_err(map_err).map(|n| n as u32)
    }
    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct ElementTask {
    inner: xa11y::Locator,
}
impl Task for ElementTask {
    type Output = xa11y::Element;
    type JsValue = Element;
    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.inner.element().map_err(map_err)
    }
    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        let data = output.data().clone();
        let provider = output.provider().clone();
        Ok(Element::new(data, provider))
    }
}

pub struct ElementsTask {
    inner: xa11y::Locator,
}
impl Task for ElementsTask {
    type Output = Vec<xa11y::Element>;
    type JsValue = Vec<Element>;
    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.inner.elements().map_err(map_err)
    }
    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output
            .into_iter()
            .map(|el| {
                let data = el.data().clone();
                let provider = el.provider().clone();
                Element::new(data, provider)
            })
            .collect())
    }
}

// ── Action task ────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub enum ActionKind {
    Press,
    Focus,
    Blur,
    Toggle,
    Expand,
    Collapse,
    Select,
    ShowMenu,
    ScrollIntoView,
    Increment,
    Decrement,
    SetValue,
    SetNumericValue,
    TypeText,
    SelectText,
    PerformAction,
}

pub struct ActionTask {
    inner: xa11y::Locator,
    kind: ActionKind,
    text: Option<String>,
    num: Option<f64>,
    range: Option<(u32, u32)>,
}

impl ActionTask {
    fn nullary(inner: xa11y::Locator, kind: ActionKind) -> Self {
        Self {
            inner,
            kind,
            text: None,
            num: None,
            range: None,
        }
    }
    fn with_text(inner: xa11y::Locator, kind: ActionKind, text: String) -> Self {
        Self {
            inner,
            kind,
            text: Some(text),
            num: None,
            range: None,
        }
    }
    fn with_num(inner: xa11y::Locator, kind: ActionKind, num: f64) -> Self {
        Self {
            inner,
            kind,
            text: None,
            num: Some(num),
            range: None,
        }
    }
    fn with_range(inner: xa11y::Locator, kind: ActionKind, start: u32, end: u32) -> Self {
        Self {
            inner,
            kind,
            text: None,
            num: None,
            range: Some((start, end)),
        }
    }
}

impl Task for ActionTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let r = match self.kind {
            ActionKind::Press => self.inner.press(),
            ActionKind::Focus => self.inner.focus(),
            ActionKind::Blur => self.inner.blur(),
            ActionKind::Toggle => self.inner.toggle(),
            ActionKind::Expand => self.inner.expand(),
            ActionKind::Collapse => self.inner.collapse(),
            ActionKind::Select => self.inner.select(),
            ActionKind::ShowMenu => self.inner.show_menu(),
            ActionKind::ScrollIntoView => self.inner.scroll_into_view(),
            ActionKind::Increment => self.inner.increment(),
            ActionKind::Decrement => self.inner.decrement(),
            ActionKind::SetValue => self.inner.set_value(self.text.as_deref().unwrap_or("")),
            ActionKind::SetNumericValue => self.inner.set_numeric_value(self.num.unwrap_or(0.0)),
            ActionKind::TypeText => self.inner.type_text(self.text.as_deref().unwrap_or("")),
            ActionKind::SelectText => {
                let (s, e) = self.range.unwrap_or((0, 0));
                self.inner.select_text(s, e)
            }
            ActionKind::PerformAction => self
                .inner
                .perform_action(self.text.as_deref().unwrap_or("")),
        };
        r.map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(())
    }
}

// ── Wait task ──────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub enum WaitKind {
    Visible,
    Attached,
    Detached,
    Enabled,
    Hidden,
    Disabled,
    Focused,
    Unfocused,
}

pub struct WaitTask {
    inner: xa11y::Locator,
    kind: WaitKind,
    timeout: Duration,
    /// Whether the JS caller expects the resolved value to be an Element or
    /// undefined. Absent-state waits (Detached/Hidden) always resolve to
    /// undefined.
    returns_element: bool,
}

impl WaitTask {
    fn returning(inner: xa11y::Locator, kind: WaitKind, secs: f64) -> Self {
        Self {
            inner,
            kind,
            timeout: Duration::from_secs_f64(secs),
            returns_element: true,
        }
    }
    fn absent(inner: xa11y::Locator, kind: WaitKind, secs: f64) -> Self {
        Self {
            inner,
            kind,
            timeout: Duration::from_secs_f64(secs),
            returns_element: false,
        }
    }
}

impl Task for WaitTask {
    type Output = Option<xa11y::Element>;
    type JsValue = Option<Element>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let r = match self.kind {
            WaitKind::Visible => self.inner.wait_visible(self.timeout).map(Some),
            WaitKind::Attached => self.inner.wait_attached(self.timeout).map(Some),
            WaitKind::Detached => self.inner.wait_detached(self.timeout).map(|_| None),
            WaitKind::Enabled => self.inner.wait_enabled(self.timeout).map(Some),
            WaitKind::Hidden => self.inner.wait_hidden(self.timeout).map(|_| None),
            WaitKind::Disabled => self.inner.wait_disabled(self.timeout).map(Some),
            WaitKind::Focused => self.inner.wait_focused(self.timeout).map(Some),
            WaitKind::Unfocused => self.inner.wait_unfocused(self.timeout).map(Some),
        };
        r.map_err(map_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        if !self.returns_element {
            return Ok(None);
        }
        Ok(output.map(|el| {
            let data = el.data().clone();
            let provider = el.provider().clone();
            Element::new(data, provider)
        }))
    }
}
