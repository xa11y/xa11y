use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A normalized enum of interactions that can be performed on accessibility elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Action {
    /// Click / tap / invoke
    Press,
    /// Set keyboard focus
    Focus,
    /// Set text content or numeric value.
    ///
    /// Accepts `ActionData::Value(String)` for text or
    /// `ActionData::NumericValue(f64)` for numeric values.
    ///
    /// **Platform note:** On Linux AT-SPI, the Value interface only supports
    /// numeric values (`f64`). Setting text requires the Text interface.
    SetValue,
    /// Toggle a checkbox or switch
    Toggle,
    /// Expand a collapsible element
    Expand,
    /// Collapse an expanded element
    Collapse,
    /// Select an item in a list or table
    Select,
    /// Show context menu or dropdown
    ShowMenu,
    /// Scroll element into visible area.
    ///
    /// **Platform note:** No-op on macOS (no direct AX equivalent).
    ScrollIntoView,
    /// Scroll by a given amount and direction.
    ///
    /// Accepts `ActionData::ScrollAmount { direction, amount }`.
    /// Amount is in logical scroll units (≈ one mouse wheel notch).
    ///
    /// **Platform mapping:**
    /// - macOS: `CGEventCreateScrollWheelEvent` with pixel delta (1 unit ≈ 10px)
    /// - Windows: `IUIAutomationScrollPattern.Scroll()` with `SmallIncrement` repeated
    /// - Linux: AT-SPI `Component.ScrollTo` with edge alignment
    Scroll,
    /// Increment a slider or spinner
    Increment,
    /// Decrement a slider or spinner
    Decrement,
    /// Remove keyboard focus from element
    Blur,
    /// Select a text range within an editable element.
    ///
    /// Accepts `ActionData::TextSelection { start, end }`.
    SetTextSelection,
    /// Insert text at the current cursor position via the accessibility API.
    ///
    /// Uses platform text-editing interfaces (AXSelectedText on macOS,
    /// EditableText.InsertText on Linux, ValuePattern on Windows) — never
    /// synthetic keyboard events.
    ///
    /// Accepts `ActionData::Value(String)`.
    TypeText,
}

impl Action {
    /// Return the snake_case string representation of this action.
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Press => "press",
            Action::Focus => "focus",
            Action::SetValue => "set_value",
            Action::Toggle => "toggle",
            Action::Expand => "expand",
            Action::Collapse => "collapse",
            Action::Select => "select",
            Action::ShowMenu => "show_menu",
            Action::ScrollIntoView => "scroll_into_view",
            Action::Scroll => "scroll",
            Action::Increment => "increment",
            Action::Decrement => "decrement",
            Action::Blur => "blur",
            Action::SetTextSelection => "set_text_selection",
            Action::TypeText => "type_text",
        }
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::Press => write!(f, "Press"),
            Action::Focus => write!(f, "Focus"),
            Action::SetValue => write!(f, "SetValue"),
            Action::Toggle => write!(f, "Toggle"),
            Action::Expand => write!(f, "Expand"),
            Action::Collapse => write!(f, "Collapse"),
            Action::Select => write!(f, "Select"),
            Action::ShowMenu => write!(f, "ShowMenu"),
            Action::ScrollIntoView => write!(f, "ScrollIntoView"),
            Action::Scroll => write!(f, "Scroll"),
            Action::Increment => write!(f, "Increment"),
            Action::Decrement => write!(f, "Decrement"),
            Action::Blur => write!(f, "Blur"),
            Action::SetTextSelection => write!(f, "SetTextSelection"),
            Action::TypeText => write!(f, "TypeText"),
        }
    }
}

/// Data associated with an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionData {
    /// Text value for SetValue action
    Value(String),
    /// Numeric value for SetValue action
    NumericValue(f64),
    /// Scroll amount and direction
    ScrollAmount {
        direction: ScrollDirection,
        amount: f64,
    },
    /// Text selection range (character offsets, 0-based)
    TextSelection { start: u32, end: u32 },
}

impl ActionData {
    /// Validate that this ActionData has valid values for the given action.
    ///
    /// Checks constraints that are always wrong regardless of element state:
    /// - `TextSelection`: `start` must be ≤ `end`
    /// - `NumericValue`: must be finite (not NaN or infinity)
    ///
    /// Does NOT query the element (e.g., does not check if indices are within
    /// text length or if numeric value is within min/max range).
    pub fn validate(&self, action: Action) -> Result<()> {
        match self {
            ActionData::TextSelection { start, end } => {
                if start > end {
                    return Err(Error::InvalidActionData {
                        message: format!(
                            "TextSelection start ({}) must be <= end ({})",
                            start, end
                        ),
                    });
                }
            }
            ActionData::NumericValue(v) => {
                if !v.is_finite() {
                    return Err(Error::InvalidActionData {
                        message: format!("{} requires a finite numeric value, got {}", action, v),
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Direction for scroll actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}
