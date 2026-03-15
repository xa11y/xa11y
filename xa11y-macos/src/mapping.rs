use xa11y_core::{Action, Role, StateSet, Toggled};

/// Map an AX role string to xa11y Role.
pub fn map_role(ax_role: &str, ax_subrole: Option<&str>) -> Role {
    match ax_role {
        "AXApplication" => Role::Application,
        "AXWindow" | "AXSheet" | "AXDrawer" => Role::Window,
        "AXButton" => Role::Button,
        "AXCheckBox" => Role::CheckBox,
        "AXRadioButton" => Role::RadioButton,
        "AXTextField" | "AXSecureTextField" | "AXSearchField" | "AXComboBox" => {
            if ax_role == "AXComboBox" {
                Role::ComboBox
            } else {
                Role::TextField
            }
        }
        "AXTextArea" => Role::TextArea,
        "AXStaticText" => Role::StaticText,
        "AXList" | "AXOutline" => {
            if ax_role == "AXOutline" {
                // AXOutline is a tree view
                Role::List
            } else {
                Role::List
            }
        }
        "AXRow" => {
            if ax_subrole == Some("AXOutlineRow") {
                Role::TreeItem
            } else {
                Role::TableRow
            }
        }
        "AXCell" => Role::TableCell,
        "AXTable" => Role::Table,
        "AXMenu" => Role::Menu,
        "AXMenuBar" => Role::MenuBar,
        "AXMenuItem" | "AXMenuBarItem" => Role::MenuItem,
        "AXTabGroup" => Role::TabGroup,
        "AXRadioGroup" => Role::Group,
        "AXGroup" | "AXSplitGroup" | "AXScrollArea" | "AXLayoutArea" | "AXMatte" => {
            if ax_role == "AXSplitGroup" {
                Role::SplitGroup
            } else {
                Role::Group
            }
        }
        "AXToolbar" => Role::Toolbar,
        "AXScrollBar" => Role::ScrollBar,
        "AXSlider" => Role::Slider,
        "AXImage" => Role::Image,
        "AXLink" => Role::Link,
        "AXProgressIndicator" | "AXBusyIndicator" | "AXRelevanceIndicator" => Role::ProgressBar,
        "AXValueIndicator" => Role::Slider,
        "AXPopUpButton" => Role::ComboBox,
        "AXDialog" => Role::Dialog,
        "AXHeading" => Role::Heading,
        "AXSplitter" | "AXRuler" => Role::Separator,
        "AXWebArea" | "AXBrowser" => Role::WebArea,
        "AXDisclosureTriangle" => Role::Button,
        "AXColorWell" | "AXIncrementor" | "AXStepper" => Role::Slider,
        "AXTab" => Role::Tab,
        _ => {
            // Check subrole for additional hints
            match ax_subrole.unwrap_or("") {
                "AXDialog" | "AXSystemDialog" => Role::Dialog,
                "AXAlert" | "AXSystemFloatingWindow" => Role::Alert,
                _ => Role::Unknown,
            }
        }
    }
}

/// Build StateSet from AX attributes on an element.
pub struct AXAttributes {
    pub enabled: Option<bool>,
    pub focused: Option<bool>,
    pub selected: Option<bool>,
    pub value: Option<AXValueState>,
    pub expanded: Option<bool>,
    pub is_expandable: bool,
    pub required: Option<bool>,
    pub busy: Option<bool>,
    pub role: &'static str,
}

pub enum AXValueState {
    Bool(bool),
    Mixed,
}

pub fn map_states(
    enabled: bool,
    focused: bool,
    selected: bool,
    value: Option<i64>,
    expanded: Option<bool>,
    role: Role,
) -> StateSet {
    let checked = match role {
        Role::CheckBox | Role::RadioButton => match value {
            Some(2) => Some(Toggled::Mixed),
            Some(1) => Some(Toggled::On),
            Some(0) => Some(Toggled::Off),
            _ => Some(Toggled::Off),
        },
        _ => None,
    };

    StateSet {
        enabled,
        visible: true, // AX only returns visible elements by default
        focused,
        checked,
        selected,
        expanded,
        editable: false, // Will be set based on role in platform.rs
        required: false,
        busy: false,
    }
}

/// Map an AX action name to xa11y Action.
pub fn map_action_name(name: &str) -> Option<Action> {
    match name {
        "AXPress" => Some(Action::Press),
        "AXShowMenu" => Some(Action::ShowMenu),
        "AXIncrement" => Some(Action::Increment),
        "AXDecrement" => Some(Action::Decrement),
        "AXConfirm" => Some(Action::Press),
        "AXCancel" => None,
        "AXRaise" => Some(Action::Focus),
        "AXPick" => Some(Action::Select),
        _ => None,
    }
}

/// Get the AX action name for an xa11y Action.
pub fn ax_action_for(action: &Action) -> Option<&'static str> {
    match action {
        Action::Press => Some("AXPress"),
        Action::Toggle => Some("AXPress"),
        Action::ShowMenu => Some("AXShowMenu"),
        Action::Increment => Some("AXIncrement"),
        Action::Decrement => Some("AXDecrement"),
        Action::Expand => Some("AXPress"),
        Action::Collapse => Some("AXPress"),
        Action::Select => Some("AXPick"),
        _ => None,
    }
}
