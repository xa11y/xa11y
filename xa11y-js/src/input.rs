//! JS `InputSim` class: synthesised pointer and keyboard input.
//!
//! Mirrors the Python binding surface (`xa11y-python/src/lib.rs`): targets
//! are either `[x, y]` tuples or `Element` instances, and keys are strings
//! (`"a"`, `"Enter"`, `"ArrowUp"`, `"Shift"`, ...). See [`parse_key`] for
//! the full grammar.
//!
//! This module is the worked example for "Binding Shape Conventions" in
//! AGENTS.md — core's `click_with` / `drag_with` folded into the options
//! object of `click` / `drag`, enum values as identically-spelled snake_case
//! strings in both bindings, durations in milliseconds on the JS side, and
//! every argument parsed before an OS event is posted. Read that section
//! before adding a method here.

use std::time::Duration;

use napi::bindgen_prelude::{AsyncTask, Env, Task};
use napi::Either;

use crate::element::Element;
use crate::map_err;

/// A target for a pointer operation: an `[x, y]` tuple in logical screen
/// coordinates, or an `Element` (anchored inside its bounds).
type Target<'a> = Either<Vec<i32>, &'a Element>;

/// Options for `InputSim.click()` — core's `ClickOptions`.
#[napi(object)]
#[derive(Default)]
pub struct ClickOptions {
    /// `'left'` (default), `'right'`, or `'middle'`.
    pub button: Option<String>,
    /// Number of consecutive clicks. Default `1`; `2` is a double-click.
    pub count: Option<u32>,
    /// Keys held down for the duration of the click, e.g. `['Shift']`.
    pub held: Option<Vec<String>>,
    /// Where inside an `Element`'s bounds to click: one of `'center'`
    /// (default), `'top_left'`, `'top_right'`, `'bottom_left'`,
    /// `'bottom_right'`, or an `[dx, dy]` pixel offset from the top-left
    /// corner. Ignored when the target is a raw `[x, y]` tuple.
    pub anchor: Option<Either<String, Vec<i32>>>,
}

/// Options for `InputSim.drag()` — core's `DragOptions`.
#[napi(object)]
#[derive(Default)]
pub struct DragOptions {
    /// `'left'` (default), `'right'`, or `'middle'`.
    pub button: Option<String>,
    /// Keys held down for the duration of the drag.
    pub held: Option<Vec<String>>,
    /// Total drag time in **milliseconds** (the unit every other JS timing
    /// option uses). Default `150`.
    pub duration: Option<u32>,
}

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
fn parse_target(target: Target<'_>) -> napi::Result<xa11y::Point> {
    parse_target_anchored(target, xa11y::Anchor::Center)
}

/// [`parse_target`] with an explicit anchor for `Element` targets.
///
/// `anchor` is ignored for raw points, matching core's `ClickTarget::Point`
/// arm.
fn parse_target_anchored(target: Target<'_>, anchor: xa11y::Anchor) -> napi::Result<xa11y::Point> {
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
            Ok(xa11y::anchor_point(&rect, anchor))
        }
    }
}

/// Parse a mouse-button name into an [`xa11y::MouseButton`]. Defaults to the
/// left button when unset.
fn parse_button(name: Option<String>) -> napi::Result<xa11y::MouseButton> {
    match name.as_deref().unwrap_or("left") {
        "left" => Ok(xa11y::MouseButton::Left),
        "right" => Ok(xa11y::MouseButton::Right),
        "middle" => Ok(xa11y::MouseButton::Middle),
        other => Err(napi::Error::from_reason(format!(
            "XA11Y_INVALID_ACTION_DATA: unknown mouse button: {other}. \
             Expected 'left', 'right', or 'middle'"
        ))),
    }
}

/// Parse an optional list of key names, defaulting to "none held".
fn parse_keys(keys: Option<Vec<String>>) -> napi::Result<Vec<xa11y::Key>> {
    keys.unwrap_or_default()
        .iter()
        .map(|s| parse_key(s))
        .collect()
}

/// Parse an anchor: one of the named corners/centre as a string, or an
/// `[dx, dy]` pixel offset from the element's top-left.
///
/// The strings are identical in the Python binding — like key names and mouse
/// buttons, input-layer string values are shared across bindings rather than
/// spelled per-language.
fn parse_anchor(anchor: Option<Either<String, Vec<i32>>>) -> napi::Result<xa11y::Anchor> {
    match anchor {
        None => Ok(xa11y::Anchor::Center),
        Some(Either::A(name)) => match name.as_str() {
            "center" => Ok(xa11y::Anchor::Center),
            "top_left" => Ok(xa11y::Anchor::TopLeft),
            "top_right" => Ok(xa11y::Anchor::TopRight),
            "bottom_left" => Ok(xa11y::Anchor::BottomLeft),
            "bottom_right" => Ok(xa11y::Anchor::BottomRight),
            other => Err(napi::Error::from_reason(format!(
                "XA11Y_INVALID_ACTION_DATA: unknown anchor: {other}. Expected \
                 'center', 'top_left', 'top_right', 'bottom_left', \
                 'bottom_right', or an [dx, dy] offset"
            ))),
        },
        Some(Either::B(offset)) => {
            if offset.len() != 2 {
                return Err(napi::Error::from_reason(format!(
                    "XA11Y_INVALID_ACTION_DATA: anchor offset must have 2 elements, got {}",
                    offset.len()
                )));
            }
            Ok(xa11y::Anchor::Offset {
                dx: offset[0],
                dy: offset[1],
            })
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
    /// Click at `target`.
    ///
    /// `options` is core's `ClickOptions`: `button` selects the mouse button,
    /// `count` repeats the click (`2` = double-click), `held` lists keys held
    /// down for the duration, and `anchor` picks the point inside an
    /// `Element`'s bounds. With no options this is a single left click at the
    /// element's centre.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn click(
        &self,
        target: Either<Vec<i32>, &Element>,
        options: Option<ClickOptions>,
    ) -> napi::Result<AsyncTask<MouseTask>> {
        let options = options.unwrap_or_default();
        let pt = parse_target_anchored(target, parse_anchor(options.anchor)?)?;
        let opts = xa11y::ClickOptions {
            button: parse_button(options.button)?,
            count: options.count.unwrap_or(1),
            held: parse_keys(options.held)?,
            ..Default::default()
        };
        Ok(AsyncTask::new(MouseTask {
            inner: self.inner.clone(),
            op: MouseOp::Click(pt, opts),
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

    /// Press a mouse button at the current pointer location, without
    /// releasing it. Pair with `mouseUp()`; for a whole click use `click()`.
    ///
    /// Takes no target: the button is pressed wherever the pointer already
    /// is, matching the OS primitive. Call `moveTo()` first to position it.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn mouse_down(&self, button: Option<String>) -> napi::Result<AsyncTask<MouseTask>> {
        Ok(AsyncTask::new(MouseTask {
            inner: self.inner.clone(),
            op: MouseOp::Down(parse_button(button)?),
        }))
    }

    /// Release a mouse button at the current pointer location.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn mouse_up(&self, button: Option<String>) -> napi::Result<AsyncTask<MouseTask>> {
        Ok(AsyncTask::new(MouseTask {
            inner: self.inner.clone(),
            op: MouseOp::Up(parse_button(button)?),
        }))
    }

    /// Drag from `start` to `end`.
    ///
    /// `options` is core's `DragOptions`: `button` selects the button held
    /// during the drag, `held` lists keys held for its duration, and
    /// `duration` is the total drag time in milliseconds (default `150`).
    #[napi(ts_return_type = "Promise<void>")]
    pub fn drag(
        &self,
        start: Either<Vec<i32>, &Element>,
        end: Either<Vec<i32>, &Element>,
        options: Option<DragOptions>,
    ) -> napi::Result<AsyncTask<MouseTask>> {
        let from = parse_target(start)?;
        let to = parse_target(end)?;
        let options = options.unwrap_or_default();
        let opts = xa11y::DragOptions {
            button: parse_button(options.button)?,
            held: parse_keys(options.held)?,
            duration: Duration::from_millis(options.duration.unwrap_or(150).into()),
        };
        Ok(AsyncTask::new(MouseTask {
            inner: self.inner.clone(),
            op: MouseOp::Drag(from, to, opts),
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

    /// Press `key` without releasing it. Pair with `keyUp()`.
    ///
    /// For a whole tap use `press()`; to hold modifiers around one tap use
    /// `chord()`. This is the primitive for sequences neither expresses, such
    /// as holding a key across several other actions.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn key_down(&self, key: String) -> napi::Result<AsyncTask<KeyboardTask>> {
        let k = parse_key(&key)?;
        Ok(AsyncTask::new(KeyboardTask {
            inner: self.inner.clone(),
            op: KeyboardOp::Down(k),
        }))
    }

    /// Release a key previously pressed with `keyDown()`.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn key_up(&self, key: String) -> napi::Result<AsyncTask<KeyboardTask>> {
        let k = parse_key(&key)?;
        Ok(AsyncTask::new(KeyboardTask {
            inner: self.inner.clone(),
            op: KeyboardOp::Up(k),
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
    Click(xa11y::Point, xa11y::ClickOptions),
    DoubleClick(xa11y::Point),
    RightClick(xa11y::Point),
    MoveTo(xa11y::Point),
    Down(xa11y::MouseButton),
    Up(xa11y::MouseButton),
    Drag(xa11y::Point, xa11y::Point, xa11y::DragOptions),
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
            MouseOp::Click(p, opts) => m.click_with(xa11y::ClickTarget::Point(*p), opts.clone()),
            MouseOp::DoubleClick(p) => m.double_click(*p),
            MouseOp::RightClick(p) => m.right_click(*p),
            MouseOp::MoveTo(p) => m.move_to(*p),
            MouseOp::Down(b) => m.down(*b),
            MouseOp::Up(b) => m.up(*b),
            MouseOp::Drag(a, b, opts) => m.drag_with(*a, *b, opts.clone()),
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
    Down(xa11y::Key),
    Up(xa11y::Key),
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
            KeyboardOp::Down(key) => k.down(key.clone()),
            KeyboardOp::Up(key) => k.up(key.clone()),
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
