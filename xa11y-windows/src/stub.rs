//! Stub backend for non-Windows platforms (allows compilation on all targets).

use xa11y_core::input::{InputProvider, Key, MouseButton, Point, ScrollDelta};
use xa11y_core::{ElementData, Error, Provider, Result, ShellSurfaceKind, Subscription};

/// Every trait method on a stub returns this. The types are public unit
/// structs, so `new()` returning an error does not stop a caller from
/// constructing one (`WindowsProvider` or `WindowsProvider::default()`) and calling into it; those
/// calls must fail through their `Result`, never panic.
fn unavailable() -> Error {
    Error::Platform {
        code: -1,
        message: "Windows backend not available on this platform".to_string(),
    }
}

#[derive(Default)]
pub struct WindowsProvider;

impl WindowsProvider {
    pub fn new() -> Result<Self> {
        Err(Error::Platform {
            code: -1,
            message: "Windows backend only available on Windows".to_string(),
        })
    }
}

#[derive(Default)]
pub struct WindowsInputProvider;

impl WindowsInputProvider {
    pub fn new() -> Result<Self> {
        Err(Error::Platform {
            code: -1,
            message: "Windows input backend only available on Windows".to_string(),
        })
    }
}

impl InputProvider for WindowsInputProvider {
    fn pointer_move(&self, _: Point) -> Result<()> {
        Err(unavailable())
    }
    fn pointer_down(&self, _: MouseButton) -> Result<()> {
        Err(unavailable())
    }
    fn pointer_up(&self, _: MouseButton) -> Result<()> {
        Err(unavailable())
    }
    fn pointer_click(&self, _: Point, _: MouseButton, _: u32) -> Result<()> {
        Err(unavailable())
    }
    fn pointer_scroll(&self, _: Point, _: ScrollDelta) -> Result<()> {
        Err(unavailable())
    }
    fn key_down(&self, _: &Key) -> Result<()> {
        Err(unavailable())
    }
    fn key_up(&self, _: &Key) -> Result<()> {
        Err(unavailable())
    }
    fn type_text(&self, _: &str) -> Result<()> {
        Err(unavailable())
    }
}

impl Provider for WindowsProvider {
    fn get_children(&self, _: Option<&ElementData>) -> Result<Vec<ElementData>> {
        Err(unavailable())
    }
    fn get_parent(&self, _: &ElementData) -> Result<Option<ElementData>> {
        Err(unavailable())
    }
    fn list_apps(&self) -> Result<Vec<ElementData>> {
        Err(unavailable())
    }
    fn focused_app(&self) -> Result<ElementData> {
        Err(unavailable())
    }
    fn list_shell_surfaces(&self) -> Result<Vec<(ShellSurfaceKind, ElementData)>> {
        Err(unavailable())
    }
    fn press(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn focus(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn blur(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn toggle(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn select(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn expand(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn collapse(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn show_menu(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn increment(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn decrement(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn scroll_into_view(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn set_value(&self, _: &ElementData, _: &str) -> Result<()> {
        Err(unavailable())
    }
    fn set_numeric_value(&self, _: &ElementData, _: f64) -> Result<()> {
        Err(unavailable())
    }
    fn type_text(&self, _: &ElementData, _: &str) -> Result<()> {
        Err(unavailable())
    }
    fn set_text_selection(&self, _: &ElementData, _: u32, _: u32) -> Result<()> {
        Err(unavailable())
    }
    fn perform_action(&self, _: &ElementData, _: &str) -> Result<()> {
        Err(unavailable())
    }
    fn activate(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn minimize(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn maximize(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn enter_fullscreen(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn restore(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn close(&self, _: &ElementData) -> Result<()> {
        Err(unavailable())
    }
    fn move_to(&self, _: &ElementData, _: i32, _: i32) -> Result<()> {
        Err(unavailable())
    }
    fn resize_to(&self, _: &ElementData, _: u32, _: u32) -> Result<()> {
        Err(unavailable())
    }
    fn subscribe(&self, _: &ElementData) -> Result<Subscription> {
        Err(unavailable())
    }
}

#[cfg(test)]
mod tests {
    //! The stubs are public unit structs, so a caller can construct them
    //! without going through `new()`. Every trait method must then fail
    //! through its `Result` rather than panic (issue #473).
    use super::*;
    use xa11y_core::selector::Selector;
    use xa11y_core::Role;

    fn el() -> ElementData {
        ElementData::for_role(Role::Button)
    }

    #[test]
    fn provider_stub_methods_return_errors() {
        // A public unit struct: constructible without `new()`.
        let p = WindowsProvider;
        let e = el();
        assert!(p.get_children(None).is_err());
        assert!(p.get_children(Some(&e)).is_err());
        assert!(p.get_parent(&e).is_err());
        assert!(p.list_apps().is_err());
        assert!(p.app_windows(&e).is_err());
        assert!(p.app_by_pid(1).is_err());
        assert!(p.focused_app().is_err());
        assert!(p.list_shell_surfaces().is_err());
        let sel = Selector::parse("button").expect("valid selector");
        assert!(p.find_elements(&e, &sel, None, None).is_err());
        assert!(p.press(&e).is_err());
        assert!(p.focus(&e).is_err());
        assert!(p.blur(&e).is_err());
        assert!(p.toggle(&e).is_err());
        assert!(p.select(&e).is_err());
        assert!(p.expand(&e).is_err());
        assert!(p.collapse(&e).is_err());
        assert!(p.show_menu(&e).is_err());
        assert!(p.increment(&e).is_err());
        assert!(p.decrement(&e).is_err());
        assert!(p.scroll_into_view(&e).is_err());
        assert!(p.set_value(&e, "x").is_err());
        assert!(p.set_numeric_value(&e, 1.0).is_err());
        assert!(p.type_text(&e, "x").is_err());
        assert!(p.set_text_selection(&e, 0, 1).is_err());
        assert!(p.perform_action(&e, "press").is_err());
        assert!(p.activate(&e).is_err());
        assert!(p.minimize(&e).is_err());
        assert!(p.maximize(&e).is_err());
        assert!(p.enter_fullscreen(&e).is_err());
        assert!(p.restore(&e).is_err());
        assert!(p.close(&e).is_err());
        assert!(p.move_to(&e, 0, 0).is_err());
        assert!(p.resize_to(&e, 1, 1).is_err());
        assert!(p.subscribe(&e).is_err());
    }

    #[test]
    fn input_stub_methods_return_errors() {
        use xa11y_core::input::{InputProvider, Key, MouseButton, Point, ScrollDelta};
        // A public unit struct: constructible without `new()`.
        let i = WindowsInputProvider;
        let at = Point { x: 0, y: 0 };
        assert!(i.pointer_move(at).is_err());
        assert!(i.pointer_down(MouseButton::Left).is_err());
        assert!(i.pointer_up(MouseButton::Left).is_err());
        assert!(i.pointer_click(at, MouseButton::Left, 1).is_err());
        assert!(i.pointer_scroll(at, ScrollDelta { dx: 0, dy: 1 }).is_err());
        assert!(i.key_down(&Key::Enter).is_err());
        assert!(i.key_up(&Key::Enter).is_err());
        assert!(i.type_text("x").is_err());
    }

    #[test]
    fn screenshot_stub_methods_return_errors() {
        use xa11y_core::{Rect, ScreenshotProvider};
        let s = crate::WindowsScreenshot;
        assert!(s.capture_full().is_err());
        assert!(s
            .capture_region(Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            })
            .is_err());
    }
}
