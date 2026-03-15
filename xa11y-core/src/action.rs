use serde::{Deserialize, Serialize};

/// Normalized enum of interactions that can be performed on accessibility elements.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    /// Click / tap / invoke
    Press,
    /// Set keyboard focus
    Focus,
    /// Set text content or numeric value
    SetValue,
    /// Toggle CheckBox, Switch
    Toggle,
    /// Expand a collapsible element
    Expand,
    /// Collapse an expanded element
    Collapse,
    /// Selection in a list/table
    Select,
    /// Context menu / dropdown
    ShowMenu,
    /// Scroll element into visible area
    ScrollIntoView,
    /// Increment slider / spinner
    Increment,
    /// Decrement slider / spinner
    Decrement,
}

/// Direction for scroll actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Data payload for actions that require additional parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionData {
    /// String value for SetValue on text fields
    Value(String),
    /// Numeric value for SetValue on sliders
    NumericValue(f64),
    /// Scroll amount and direction
    ScrollAmount {
        direction: ScrollDirection,
        amount: f64,
    },
    /// Point coordinates
    Point { x: f64, y: f64 },
}
