//! Stub backend for non-Linux platforms (allows compilation on all targets).

use xa11y_core::{ElementData, Error, Provider, Result, Subscription};

#[derive(Default)]
pub struct LinuxProvider;

impl LinuxProvider {
    pub fn new() -> Result<Self> {
        Ok(Self)
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
