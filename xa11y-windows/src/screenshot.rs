//! Windows screen capture — stub for this spike.
//!
//! A real implementation would use `Windows.Graphics.Capture` (UWP, DWM-aware,
//! works on composited desktops) or GDI `BitBlt` (simpler, but breaks with
//! per-monitor DPI and hardware-accelerated windows). Tracked as follow-up.

use xa11y_core::{Error, Rect, Result, Screenshot, ScreenshotProvider};

pub struct WindowsScreenshot;

impl WindowsScreenshot {
    pub fn new() -> Result<Self> {
        Err(Error::Unsupported {
            feature: "screenshot on Windows (not yet implemented)".into(),
        })
    }
}

impl ScreenshotProvider for WindowsScreenshot {
    fn capture_full(&self) -> Result<Screenshot> {
        unreachable!()
    }
    fn capture_region(&self, _: Rect) -> Result<Screenshot> {
        unreachable!()
    }
}
