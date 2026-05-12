//! Stub backend for non-Linux platforms (allows compilation on all targets).

use xa11y_core::input::{InputProvider, Key, MouseButton, Point, ScrollDelta};
use xa11y_core::{
    ElementData, Error, Provider, Rect, Result, Screenshot, ScreenshotProvider, Subscription,
};

#[derive(Default)]
pub struct LinuxProvider;

impl LinuxProvider {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

#[derive(Default)]
pub struct LinuxInputProvider;

impl LinuxInputProvider {
    pub fn new() -> Result<Self> {
        Err(Error::Platform {
            code: -1,
            message: "Linux input backend only available on Linux".to_string(),
        })
    }
}

impl InputProvider for LinuxInputProvider {
    fn pointer_move(&self, _: Point) -> Result<()> {
        unreachable!()
    }
    fn pointer_down(&self, _: MouseButton) -> Result<()> {
        unreachable!()
    }
    fn pointer_up(&self, _: MouseButton) -> Result<()> {
        unreachable!()
    }
    fn pointer_click(&self, _: Point, _: MouseButton, _: u32) -> Result<()> {
        unreachable!()
    }
    fn pointer_scroll(&self, _: Point, _: ScrollDelta) -> Result<()> {
        unreachable!()
    }
    fn key_down(&self, _: &Key) -> Result<()> {
        unreachable!()
    }
    fn key_up(&self, _: &Key) -> Result<()> {
        unreachable!()
    }
    fn type_text(&self, _: &str) -> Result<()> {
        unreachable!()
    }
}

pub struct LinuxScreenshot;

impl LinuxScreenshot {
    pub fn new() -> Result<Self> {
        Err(Error::Platform {
            code: -1,
            message: "Linux screenshot backend only available on Linux".to_string(),
        })
    }
}

impl ScreenshotProvider for LinuxScreenshot {
    fn capture_full(&self) -> Result<Screenshot> {
        unreachable!()
    }
    fn capture_region(&self, _: Rect) -> Result<Screenshot> {
        unreachable!()
    }
}

const NOT_AVAILABLE: &str = "Linux backend not available on this platform";

fn unavailable() -> Error {
    Error::Platform {
        code: -1,
        message: NOT_AVAILABLE.to_string(),
    }
}

impl Provider for LinuxProvider {
    fn get_children(&self, _: Option<&ElementData>) -> Result<Vec<ElementData>> {
        Err(unavailable())
    }

    fn get_parent(&self, _: &ElementData) -> Result<Option<ElementData>> {
        Err(unavailable())
    }

    fn list_apps(&self) -> Result<Vec<ElementData>> {
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

    fn subscribe(&self, _: &ElementData) -> Result<Subscription> {
        Err(unavailable())
    }
}
