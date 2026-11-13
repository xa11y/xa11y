//! Stub backend for non-Linux platforms (allows compilation on all targets).

use xa11y_core::input::{InputProvider, Key, MouseButton, Point, ScrollDelta};
use xa11y_core::{
    ElementData, Error, Provider, Rect, Result, Screenshot, ScreenshotProvider, ShellSurfaceKind,
    Subscription,
};

pub struct LinuxProvider;

impl LinuxProvider {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

pub struct LinuxInputProvider;

impl LinuxInputProvider {
    pub fn new() -> Result<Self> {
        Err(Error::platform(
            -1,
            "Linux input backend only available on Linux",
        ))
    }
}

impl LinuxInputProvider {
    pub fn selected_backend_uses_screen_coordinates() -> Result<bool> {
        Err(unavailable("selected_backend_uses_screen_coordinates"))
    }
}

impl InputProvider for LinuxInputProvider {
    fn pointer_move(&self, _: Point) -> Result<()> {
        Err(unavailable("pointer_move"))
    }
    fn pointer_down(&self, _: MouseButton) -> Result<()> {
        Err(unavailable("pointer_down"))
    }
    fn pointer_up(&self, _: MouseButton) -> Result<()> {
        Err(unavailable("pointer_up"))
    }
    fn pointer_click(&self, _: Point, _: MouseButton, _: u32) -> Result<()> {
        Err(unavailable("pointer_click"))
    }
    fn pointer_scroll(&self, _: Point, _: ScrollDelta) -> Result<()> {
        Err(unavailable("pointer_scroll"))
    }
    fn key_down(&self, _: &Key) -> Result<()> {
        Err(unavailable("key_down"))
    }
    fn key_up(&self, _: &Key) -> Result<()> {
        Err(unavailable("key_up"))
    }
    fn type_text(&self, _: &str) -> Result<()> {
        Err(unavailable("type_text"))
    }
}

pub struct LinuxScreenshot;

impl LinuxScreenshot {
    pub fn new() -> Result<Self> {
        Err(Error::platform(
            -1,
            "Linux screenshot backend only available on Linux",
        ))
    }
}

impl ScreenshotProvider for LinuxScreenshot {
    fn capture_full(&self) -> Result<(Screenshot, Point)> {
        Err(unavailable("capture_full"))
    }
    fn capture_region(&self, _: Rect) -> Result<Screenshot> {
        Err(unavailable("capture_region"))
    }
}

/// Every trait method on a stub returns this. The types are public unit
/// structs, so a constructor returning an error does not stop a caller from
/// building one (`LinuxProvider`, `LinuxInputProvider` or `LinuxScreenshot`)
/// and calling into it; those calls must fail through
/// their `Result`, never panic. The message names the operation and
/// the host OS so the failure explains itself (tenet 6).
fn unavailable(op: &str) -> Error {
    Error::platform(
        -1,
        format!(
            "Linux backend unavailable: `{op}` called on {}",
            std::env::consts::OS
        ),
    )
}

impl Provider for LinuxProvider {
    fn get_children(&self, _: Option<&ElementData>) -> Result<Vec<ElementData>> {
        Err(unavailable("get_children"))
    }

    fn get_parent(&self, _: &ElementData) -> Result<Option<ElementData>> {
        Err(unavailable("get_parent"))
    }

    fn list_apps(&self) -> Result<Vec<ElementData>> {
        Err(unavailable("list_apps"))
    }

    fn focused_app(&self) -> Result<ElementData> {
        Err(unavailable("focused_app"))
    }

    fn enable_web_accessibility(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("enable_web_accessibility"))
    }
    fn list_shell_surfaces(&self) -> Result<Vec<(ShellSurfaceKind, ElementData)>> {
        Err(unavailable("list_shell_surfaces"))
    }

    fn press(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("press"))
    }

    fn focus(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("focus"))
    }

    fn blur(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("blur"))
    }

    fn toggle(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("toggle"))
    }

    fn select(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("select"))
    }

    fn expand(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("expand"))
    }

    fn collapse(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("collapse"))
    }

    fn show_menu(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("show_menu"))
    }

    fn increment(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("increment"))
    }

    fn decrement(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("decrement"))
    }

    fn scroll_into_view(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("scroll_into_view"))
    }

    fn set_value(&self, _: &ElementData, _: &str) -> Result<()> {
        Err(unavailable("set_value"))
    }

    fn set_numeric_value(&self, _: &ElementData, _: f64) -> Result<()> {
        Err(unavailable("set_numeric_value"))
    }

    fn type_text(&self, _: &ElementData, _: &str) -> Result<()> {
        Err(unavailable("type_text"))
    }

    fn set_text_selection(&self, _: &ElementData, _: u32, _: u32) -> Result<()> {
        Err(unavailable("set_text_selection"))
    }

    fn perform_action(&self, _: &ElementData, _: &str) -> Result<()> {
        Err(unavailable("perform_action"))
    }

    fn activate(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("activate"))
    }

    fn minimize(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("minimize"))
    }

    fn maximize(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("maximize"))
    }

    fn enter_fullscreen(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("enter_fullscreen"))
    }

    fn restore(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("restore"))
    }

    fn close(&self, _: &ElementData) -> Result<()> {
        Err(unavailable("close"))
    }

    fn move_to(&self, _: &ElementData, _: i32, _: i32) -> Result<()> {
        Err(unavailable("move_to"))
    }

    fn resize_to(&self, _: &ElementData, _: u32, _: u32) -> Result<()> {
        Err(unavailable("resize_to"))
    }

    fn subscribe(&self, _: &ElementData) -> Result<Subscription> {
        Err(unavailable("subscribe"))
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

    /// The stub's error must be the platform-unavailable one, not merely
    /// some error.
    fn assert_platform<T>(result: Result<T>) {
        match result {
            Err(Error::Platform { code: -1, .. }) => {}
            Err(other) => panic!("expected Error::Platform, got {other:?}"),
            Ok(_) => panic!("expected Error::Platform, got Ok"),
        }
    }

    fn el() -> ElementData {
        ElementData::for_role(Role::Button)
    }

    #[test]
    fn provider_stub_methods_return_errors() {
        // A public unit struct: constructible without `new()`.
        let p = LinuxProvider;
        let e = el();
        assert_platform(p.get_children(None));
        assert_platform(p.get_children(Some(&e)));
        assert_platform(p.get_parent(&e));
        assert_platform(p.list_apps());
        assert_platform(p.app_windows(&e));
        assert_platform(p.app_by_pid(1));
        assert_platform(p.focused_app());
        assert_platform(p.list_shell_surfaces());
        let sel = Selector::parse("button").expect("valid selector");
        assert_platform(p.find_elements(&e, &sel, None, None));
        assert_platform(p.press(&e));
        assert_platform(p.focus(&e));
        assert_platform(p.blur(&e));
        assert_platform(p.toggle(&e));
        assert_platform(p.select(&e));
        assert_platform(p.expand(&e));
        assert_platform(p.collapse(&e));
        assert_platform(p.show_menu(&e));
        assert_platform(p.increment(&e));
        assert_platform(p.decrement(&e));
        assert_platform(p.scroll_into_view(&e));
        assert_platform(p.set_value(&e, "x"));
        assert_platform(p.set_numeric_value(&e, 1.0));
        assert_platform(p.type_text(&e, "x"));
        assert_platform(p.set_text_selection(&e, 0, 1));
        assert_platform(p.perform_action(&e, "press"));
        assert_platform(p.activate(&e));
        assert_platform(p.minimize(&e));
        assert_platform(p.maximize(&e));
        assert_platform(p.enter_fullscreen(&e));
        assert_platform(p.restore(&e));
        assert_platform(p.close(&e));
        assert_platform(p.move_to(&e, 0, 0));
        assert_platform(p.resize_to(&e, 1, 1));
        assert_platform(p.subscribe(&e));
    }

    #[test]
    fn input_stub_methods_return_errors() {
        use xa11y_core::input::{InputProvider, Key, MouseButton, Point, ScrollDelta};
        // A public unit struct: constructible without `new()`.
        let i = LinuxInputProvider;
        let at = Point { x: 0, y: 0 };
        assert_platform(i.pointer_move(at));
        assert_platform(i.pointer_down(MouseButton::Left));
        assert_platform(i.pointer_up(MouseButton::Left));
        assert_platform(i.pointer_click(at, MouseButton::Left, 1));
        assert_platform(i.pointer_scroll(at, ScrollDelta { dx: 0, dy: 1 }));
        assert_platform(i.key_down(&Key::Enter));
        assert_platform(i.key_up(&Key::Enter));
        assert_platform(i.type_text("x"));
    }

    #[test]
    fn screenshot_stub_methods_return_errors() {
        use xa11y_core::{Rect, ScreenshotProvider};
        let s = LinuxScreenshot;
        assert_platform(s.capture_full());
        assert_platform(s.capture_region(Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        }));
    }
}
