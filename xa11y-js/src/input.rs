//! JS `InputSim` class: synthesised pointer and keyboard input.
//!
//! Mirrors the Python binding surface (`xa11y-python/src/lib.rs`): targets
//! are either `[x, y]` tuples or `Element` instances, and keys are strings
//! (`"a"`, `"Enter"`, `"ArrowUp"`, `"Shift"`, ...). See [`parse_key`] for
//! the full grammar.

use napi::bindgen_prelude::{AsyncTask, Env, Task};
use napi::Either;

use crate::element::Element;
use crate::map_err;

/// Synthesises OS-level pointer and keyboard events.
///
/// Constructed via the module-level `inputSim()` function. Targets are
/// either an `[x, y]` tuple in logical screen coordinates (same space as
/// `Element.bounds`), or an `Element` (centred on its bounds). Each backend
/// converts to physical device pixels at the OS boundary. Key values are
/// strings: printable characters are literal
/// (`"a"`, `"7"`, `";"`); named keys use their Pascal name (`"Enter"`,
/// `"ArrowUp"`, `"F5"`); modifiers are `"Shift"`, `"Ctrl"`, `"Alt"`,
/// `"Meta"`.
///
/// Input simulation is distinct from the accessibility action layer —
/// prefer `Locator.press` / `Locator.typeText` when the target exposes
/// the semantic action. Use `InputSim` for gestures with no a11y
/// equivalent (drag-and-drop, scroll wheels, global shortcuts).
///
/// Methods return `Promise<void>` — the underlying OS input APIs are
/// synchronous but can block briefly, so they run on the napi worker pool.
#[napi]
pub struct InputSim {
    inner: xa11y::InputSim,
}

/// Parse a JS target into an `xa11y::Point`. Accepts either an `[x, y]`
/// tuple (as `Vec<i32>` of length 2) or an `Element` (uses its bounds centre).
fn parse_target(target: Either<Vec<i32>, &Element>) -> napi::Result<xa11y::Point> {
    match target {
        Either::A(tup) => {
            if tup.len() != 2 {
                return Err(napi::Error::from_reason(format!(
                    "XA11Y_INVALID_ACTION_DATA: target tuple must have 2 elements, got {}",
                    tup.len()
                )));
            }
            Ok(xa11y::Point::new(tup[0], tup[1]))
        }
        Either::B(el) => {
            let rect = el.data.bounds.ok_or_else(|| {
                napi::Error::from_reason(
                    "XA11Y_NO_ELEMENT_BOUNDS: element has no bounds; cannot compute a screen point"
                        .to_string(),
                )
            })?;
            Ok(xa11y::Point::new(
                rect.x + (rect.width as i32) / 2,
                rect.y + (rect.height as i32) / 2,
            ))
        }
    }
}

/// Parse a JS key-name string into an [`xa11y::Key`]. Grammar matches the
/// Python binding: single characters are literal; named keys use their
/// Pascal name (`"Enter"`, `"ArrowUp"`, `"F5"`); modifiers are `"Shift"`,
/// `"Ctrl"`, `"Alt"`, `"Meta"`.
fn parse_key(name: &str) -> napi::Result<xa11y::Key> {
    let k = match name {
        "Shift" => xa11y::Key::Shift,
        "Ctrl" | "Control" => xa11y::Key::Ctrl,
        "Alt" | "Option" => xa11y::Key::Alt,
        "Meta" | "Cmd" | "Command" | "Super" | "Win" => xa11y::Key::Meta,
        "Enter" | "Return" => xa11y::Key::Enter,
        "Escape" | "Esc" => xa11y::Key::Escape,
        "Backspace" => xa11y::Key::Backspace,
        "Tab" => xa11y::Key::Tab,
        "Space" => xa11y::Key::Space,
        "Delete" => xa11y::Key::Delete,
        "Insert" => xa11y::Key::Insert,
        "ArrowUp" | "Up" => xa11y::Key::ArrowUp,
        "ArrowDown" | "Down" => xa11y::Key::ArrowDown,
        "ArrowLeft" | "Left" => xa11y::Key::ArrowLeft,
        "ArrowRight" | "Right" => xa11y::Key::ArrowRight,
        "Home" => xa11y::Key::Home,
        "End" => xa11y::Key::End,
        "PageUp" => xa11y::Key::PageUp,
        "PageDown" => xa11y::Key::PageDown,
        s if s.starts_with('F') && s.len() >= 2 && s[1..].chars().all(|c| c.is_ascii_digit()) => {
            let n: u8 = s[1..].parse().map_err(|_| {
                napi::Error::from_reason(format!(
                    "XA11Y_INVALID_ACTION_DATA: invalid function key: {s}"
                ))
            })?;
            xa11y::Key::F(n)
        }
        s if s.chars().count() == 1 => xa11y::Key::Char(s.chars().next().unwrap()),
        _ => {
            return Err(napi::Error::from_reason(format!(
                "XA11Y_INVALID_ACTION_DATA: unknown key name: {name}"
            )))
        }
    };
    Ok(k)
}

#[napi]
impl InputSim {
    /// Left-click once at `target`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn click(&self, target: Either<Vec<i32>, &Element>) -> napi::Result<AsyncTask<MouseTask>> {
        let pt = parse_target(target)?;
        Ok(AsyncTask::new(MouseTask {
            inner: self.inner.clone(),
            op: MouseOp::Click(pt),
        }))
    }

    /// Left double-click at `target`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn double_click(
        &self,
        target: Either<Vec<i32>, &Element>,
    ) -> napi::Result<AsyncTask<MouseTask>> {
        let pt = parse_target(target)?;
        Ok(AsyncTask::new(MouseTask {
            inner: self.inner.clone(),
            op: MouseOp::DoubleClick(pt),
        }))
    }

    /// Right-click at `target`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn right_click(
        &self,
        target: Either<Vec<i32>, &Element>,
    ) -> napi::Result<AsyncTask<MouseTask>> {
        let pt = parse_target(target)?;
        Ok(AsyncTask::new(MouseTask {
            inner: self.inner.clone(),
            op: MouseOp::RightClick(pt),
        }))
    }

    /// Move the pointer to `target` without pressing any button.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn move_to(
        &self,
        target: Either<Vec<i32>, &Element>,
    ) -> napi::Result<AsyncTask<MouseTask>> {
        let pt = parse_target(target)?;
        Ok(AsyncTask::new(MouseTask {
            inner: self.inner.clone(),
            op: MouseOp::MoveTo(pt),
        }))
    }

    /// Left-drag from `start` to `end`. Default duration (150 ms).
    #[napi(ts_return_type = "Promise<void>")]
    pub fn drag(
        &self,
        start: Either<Vec<i32>, &Element>,
        end: Either<Vec<i32>, &Element>,
    ) -> napi::Result<AsyncTask<MouseTask>> {
        let from = parse_target(start)?;
        let to = parse_target(end)?;
        Ok(AsyncTask::new(MouseTask {
            inner: self.inner.clone(),
            op: MouseOp::Drag(from, to),
        }))
    }

    /// Scroll at `target`. `dx` positive → right, `dy` positive → content
    /// scrolls down. Defaults: `0`, `0` (a no-op).
    #[napi(ts_return_type = "Promise<void>")]
    pub fn scroll(
        &self,
        target: Either<Vec<i32>, &Element>,
        dx: Option<i32>,
        dy: Option<i32>,
    ) -> napi::Result<AsyncTask<MouseTask>> {
        let pt = parse_target(target)?;
        Ok(AsyncTask::new(MouseTask {
            inner: self.inner.clone(),
            op: MouseOp::Scroll(pt, dx.unwrap_or(0), dy.unwrap_or(0)),
        }))
    }

    /// Tap a key (press + release).
    #[napi(ts_return_type = "Promise<void>")]
    pub fn press(&self, key: String) -> napi::Result<AsyncTask<KeyboardTask>> {
        let k = parse_key(&key)?;
        Ok(AsyncTask::new(KeyboardTask {
            inner: self.inner.clone(),
            op: KeyboardOp::Press(k),
        }))
    }

    /// Tap `key` while the keys in `held` are held down.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn chord(
        &self,
        key: String,
        held: Option<Vec<String>>,
    ) -> napi::Result<AsyncTask<KeyboardTask>> {
        let k = parse_key(&key)?;
        let held: Result<Vec<_>, _> = held
            .unwrap_or_default()
            .iter()
            .map(|s| parse_key(s))
            .collect();
        Ok(AsyncTask::new(KeyboardTask {
            inner: self.inner.clone(),
            op: KeyboardOp::Chord(k, held?),
        }))
    }

    /// Type literal text into the currently focused control.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn type_text(&self, text: String) -> AsyncTask<KeyboardTask> {
        AsyncTask::new(KeyboardTask {
            inner: self.inner.clone(),
            op: KeyboardOp::TypeText(text),
        })
    }
}

// ── Async tasks ─────────────────────────────────────────────────────────

pub enum MouseOp {
    Click(xa11y::Point),
    DoubleClick(xa11y::Point),
    RightClick(xa11y::Point),
    MoveTo(xa11y::Point),
    Drag(xa11y::Point, xa11y::Point),
    Scroll(xa11y::Point, i32, i32),
}

pub struct MouseTask {
    inner: xa11y::InputSim,
    op: MouseOp,
}

impl Task for MouseTask {
    type Output = ();
    type JsValue = ();
    fn compute(&mut self) -> napi::Result<Self::Output> {
        let m = self.inner.mouse();
        match &self.op {
            MouseOp::Click(p) => m.click(*p),
            MouseOp::DoubleClick(p) => m.double_click(*p),
            MouseOp::RightClick(p) => m.right_click(*p),
            MouseOp::MoveTo(p) => m.move_to(*p),
            MouseOp::Drag(a, b) => m.drag(*a, *b),
            MouseOp::Scroll(p, dx, dy) => m.scroll(*p, xa11y::ScrollDelta::new(*dx, *dy)),
        }
        .map_err(map_err)
    }
    fn resolve(&mut self, _env: Env, _: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(())
    }
}

pub enum KeyboardOp {
    Press(xa11y::Key),
    Chord(xa11y::Key, Vec<xa11y::Key>),
    TypeText(String),
}

pub struct KeyboardTask {
    inner: xa11y::InputSim,
    op: KeyboardOp,
}

impl Task for KeyboardTask {
    type Output = ();
    type JsValue = ();
    fn compute(&mut self) -> napi::Result<Self::Output> {
        let k = self.inner.keyboard();
        match &self.op {
            KeyboardOp::Press(key) => k.press(key.clone()),
            KeyboardOp::Chord(key, held) => k.chord(key.clone(), held),
            KeyboardOp::TypeText(s) => k.type_text(s),
        }
        .map_err(map_err)
    }
    fn resolve(&mut self, _env: Env, _: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(())
    }
}

/// Construct an `InputSim` backed by the platform's native input path
/// (CGEvent on macOS, SendInput on Windows, XTest on X11).
///
/// Throws `PlatformError` on a Wayland-only Linux session (no XTest
/// available). `InputSim` is cheap to hold; construct one and reuse.
#[napi(js_name = "inputSim")]
#[allow(
    dead_code,
    reason = "Exported via napi-derive; clippy on the Rust-only build can't see the JS-side caller"
)]
pub fn make_input_sim() -> napi::Result<InputSim> {
    let sim = xa11y::input_sim().map_err(map_err)?;
    Ok(InputSim { inner: sim })
}
