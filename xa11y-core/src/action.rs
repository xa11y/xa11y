use serde::{Deserialize, Serialize};

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
    /// Type text character-by-character (input simulation).
    ///
    /// Accepts `ActionData::Value(String)`.
    TypeText,
    /// Drag this element to a target point.
    ///
    /// Accepts `ActionData::Point { x, y }` for the drop destination.
    DragTo,
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
            Action::DragTo => write!(f, "DragTo"),
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
    /// Screen coordinate point
    Point { x: f64, y: f64 },
    /// Text selection range (character offsets, 0-based)
    TextSelection { start: u32, end: u32 },
}

/// Direction for scroll actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}
