//! Small plain-data types exposed to JS: `Rect`, `EventType`, etc.

/// A bounding rectangle in screen coordinates (pixels).
#[napi(object)]
#[derive(Clone)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl From<xa11y::Rect> for Rect {
    fn from(r: xa11y::Rect) -> Self {
        Self {
            x: r.x,
            y: r.y,
            width: r.width as i32,
            height: r.height as i32,
        }
    }
}

/// Convert an `xa11y::EventType` into a camelCase string used across the JS API.
pub fn event_type_to_str(event_type: xa11y::EventType) -> &'static str {
    match event_type {
        xa11y::EventType::FocusChanged => "focusChanged",
        xa11y::EventType::ValueChanged => "valueChanged",
        xa11y::EventType::NameChanged => "nameChanged",
        xa11y::EventType::StateChanged => "stateChanged",
        xa11y::EventType::StructureChanged => "structureChanged",
        xa11y::EventType::WindowOpened => "windowOpened",
        xa11y::EventType::WindowClosed => "windowClosed",
        xa11y::EventType::WindowActivated => "windowActivated",
        xa11y::EventType::WindowDeactivated => "windowDeactivated",
        xa11y::EventType::SelectionChanged => "selectionChanged",
        xa11y::EventType::MenuOpened => "menuOpened",
        xa11y::EventType::MenuClosed => "menuClosed",
        xa11y::EventType::Alert => "alert",
        xa11y::EventType::TextChanged => "textChanged",
    }
}

/// Convert an `xa11y::Toggled` into a lower-case string (`"on"`/`"off"`/`"mixed"`).
pub fn toggled_to_str(t: xa11y::Toggled) -> &'static str {
    match t {
        xa11y::Toggled::Off => "off",
        xa11y::Toggled::On => "on",
        xa11y::Toggled::Mixed => "mixed",
    }
}
