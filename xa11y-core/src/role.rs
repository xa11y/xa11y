use serde::{Deserialize, Serialize};

/// A normalized enum covering UI element types across all platforms.
/// Derived from ARIA roles, scoped to roles commonly surfaced by real desktop applications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "python", derive(xa11y_macros::PyBindable))]
#[cfg_attr(feature = "python", pyo3::pyclass(frozen, eq, hash))]
#[repr(u8)]
pub enum Role {
    Unknown,
    Window,
    Application,
    Button,
    CheckBox,
    RadioButton,
    /// Single-line text input
    TextField,
    /// Multi-line text input
    TextArea,
    /// Non-editable text / label
    StaticText,
    ComboBox,
    List,
    ListItem,
    Menu,
    MenuItem,
    MenuBar,
    Tab,
    TabGroup,
    Table,
    TableRow,
    TableCell,
    Toolbar,
    ScrollBar,
    Slider,
    Image,
    Link,
    /// Generic container
    Group,
    Dialog,
    Alert,
    ProgressBar,
    TreeItem,
    /// Web content area / document
    WebArea,
    Heading,
    Separator,
    SplitGroup,
    /// Toggle switch (distinct from CheckBox)
    Switch,
    /// Numeric spinner input
    SpinButton,
    /// Tooltip popup
    Tooltip,
    /// Status bar or live region
    Status,
    /// Navigation landmark
    Navigation,
}

impl Role {
    /// Parse a snake_case role name into a Role enum variant.
    /// Returns `None` if the name doesn't match any known role.
    pub fn from_snake_case(s: &str) -> Option<Self> {
        match s {
            "unknown" => Some(Role::Unknown),
            "window" => Some(Role::Window),
            "application" => Some(Role::Application),
            "button" => Some(Role::Button),
            "check_box" => Some(Role::CheckBox),
            "radio_button" => Some(Role::RadioButton),
            "text_field" => Some(Role::TextField),
            "text_area" => Some(Role::TextArea),
            "static_text" => Some(Role::StaticText),
            "combo_box" => Some(Role::ComboBox),
            "list" => Some(Role::List),
            "list_item" => Some(Role::ListItem),
            "menu" => Some(Role::Menu),
            "menu_item" => Some(Role::MenuItem),
            "menu_bar" => Some(Role::MenuBar),
            "tab" => Some(Role::Tab),
            "tab_group" => Some(Role::TabGroup),
            "table" => Some(Role::Table),
            "table_row" => Some(Role::TableRow),
            "table_cell" => Some(Role::TableCell),
            "toolbar" => Some(Role::Toolbar),
            "scroll_bar" => Some(Role::ScrollBar),
            "slider" => Some(Role::Slider),
            "image" => Some(Role::Image),
            "link" => Some(Role::Link),
            "group" => Some(Role::Group),
            "dialog" => Some(Role::Dialog),
            "alert" => Some(Role::Alert),
            "progress_bar" => Some(Role::ProgressBar),
            "tree_item" => Some(Role::TreeItem),
            "web_area" => Some(Role::WebArea),
            "heading" => Some(Role::Heading),
            "separator" => Some(Role::Separator),
            "split_group" => Some(Role::SplitGroup),
            "switch" => Some(Role::Switch),
            "spin_button" => Some(Role::SpinButton),
            "tooltip" => Some(Role::Tooltip),
            "status" => Some(Role::Status),
            "navigation" => Some(Role::Navigation),
            _ => None,
        }
    }

    /// Convert a Role to its snake_case string representation.
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Role::Unknown => "unknown",
            Role::Window => "window",
            Role::Application => "application",
            Role::Button => "button",
            Role::CheckBox => "check_box",
            Role::RadioButton => "radio_button",
            Role::TextField => "text_field",
            Role::TextArea => "text_area",
            Role::StaticText => "static_text",
            Role::ComboBox => "combo_box",
            Role::List => "list",
            Role::ListItem => "list_item",
            Role::Menu => "menu",
            Role::MenuItem => "menu_item",
            Role::MenuBar => "menu_bar",
            Role::Tab => "tab",
            Role::TabGroup => "tab_group",
            Role::Table => "table",
            Role::TableRow => "table_row",
            Role::TableCell => "table_cell",
            Role::Toolbar => "toolbar",
            Role::ScrollBar => "scroll_bar",
            Role::Slider => "slider",
            Role::Image => "image",
            Role::Link => "link",
            Role::Group => "group",
            Role::Dialog => "dialog",
            Role::Alert => "alert",
            Role::ProgressBar => "progress_bar",
            Role::TreeItem => "tree_item",
            Role::WebArea => "web_area",
            Role::Heading => "heading",
            Role::Separator => "separator",
            Role::SplitGroup => "split_group",
            Role::Switch => "switch",
            Role::SpinButton => "spin_button",
            Role::Tooltip => "tooltip",
            Role::Status => "status",
            Role::Navigation => "navigation",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_snake_case())
    }
}
