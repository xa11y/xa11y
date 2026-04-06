//! Stub backend for non-Windows platforms (allows compilation on all targets).

use xa11y_core::{ElementData, Error, Provider, Result, Subscription};

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

impl Provider for WindowsProvider {
    fn get_children(&self, _: Option<&ElementData>) -> Result<Vec<ElementData>> {
        unreachable!()
    }
    fn get_parent(&self, _: &ElementData) -> Result<Option<ElementData>> {
        unreachable!()
    }
    fn press(&self, _: &ElementData) -> Result<()> {
        unreachable!()
    }
    fn focus(&self, _: &ElementData) -> Result<()> {
        unreachable!()
    }
    fn blur(&self, _: &ElementData) -> Result<()> {
        unreachable!()
    }
    fn toggle(&self, _: &ElementData) -> Result<()> {
        unreachable!()
    }
    fn select(&self, _: &ElementData) -> Result<()> {
        unreachable!()
    }
    fn expand(&self, _: &ElementData) -> Result<()> {
        unreachable!()
    }
    fn collapse(&self, _: &ElementData) -> Result<()> {
        unreachable!()
    }
    fn show_menu(&self, _: &ElementData) -> Result<()> {
        unreachable!()
    }
    fn increment(&self, _: &ElementData) -> Result<()> {
        unreachable!()
    }
    fn decrement(&self, _: &ElementData) -> Result<()> {
        unreachable!()
    }
    fn scroll_into_view(&self, _: &ElementData) -> Result<()> {
        unreachable!()
    }
    fn set_value(&self, _: &ElementData, _: &str) -> Result<()> {
        unreachable!()
    }
    fn set_numeric_value(&self, _: &ElementData, _: f64) -> Result<()> {
        unreachable!()
    }
    fn type_text(&self, _: &ElementData, _: &str) -> Result<()> {
        unreachable!()
    }
    fn set_text_selection(&self, _: &ElementData, _: u32, _: u32) -> Result<()> {
        unreachable!()
    }
    fn scroll_down(&self, _: &ElementData, _: f64) -> Result<()> {
        unreachable!()
    }
    fn scroll_up(&self, _: &ElementData, _: f64) -> Result<()> {
        unreachable!()
    }
    fn scroll_right(&self, _: &ElementData, _: f64) -> Result<()> {
        unreachable!()
    }
    fn scroll_left(&self, _: &ElementData, _: f64) -> Result<()> {
        unreachable!()
    }
    fn perform_action(&self, _: &ElementData, _: &str) -> Result<()> {
        unreachable!()
    }
    fn subscribe(&self, _: &ElementData) -> Result<Subscription> {
        unreachable!()
    }
}
