use xa11y_core::{Action, Role, StateSet, Toggled};

// AT-SPI2 role constants
const ROLE_ALERT: u32 = 2;
const ROLE_CHECK_BOX: u32 = 7;
const ROLE_CHECK_MENU_ITEM: u32 = 8;
const ROLE_COMBO_BOX: u32 = 11;
const ROLE_DESKTOP_ICON: u32 = 13;
const ROLE_DIALOG: u32 = 16;
const ROLE_FILE_CHOOSER: u32 = 19;
const ROLE_FILLER: u32 = 20;
const ROLE_FRAME: u32 = 23;
const ROLE_ICON: u32 = 26;
const ROLE_IMAGE: u32 = 27;
const ROLE_LABEL: u32 = 29;
const ROLE_LIST: u32 = 31;
const ROLE_LIST_ITEM: u32 = 32;
const ROLE_MENU: u32 = 33;
const ROLE_MENU_BAR: u32 = 34;
const ROLE_MENU_ITEM: u32 = 35;
const ROLE_PAGE_TAB: u32 = 37;
const ROLE_PAGE_TAB_LIST: u32 = 38;
const ROLE_PANEL: u32 = 39;
const ROLE_PASSWORD_TEXT: u32 = 40;
const ROLE_POPUP_MENU: u32 = 41;
const ROLE_PROGRESS_BAR: u32 = 42;
const ROLE_PUSH_BUTTON: u32 = 43;
const ROLE_RADIO_BUTTON: u32 = 44;
const ROLE_RADIO_MENU_ITEM: u32 = 45;
const ROLE_SCROLL_BAR: u32 = 48;
const ROLE_SEPARATOR: u32 = 50;
const ROLE_SLIDER: u32 = 51;
const ROLE_SPIN_BUTTON: u32 = 52;
const ROLE_SPLIT_PANE: u32 = 53;
const ROLE_TABLE: u32 = 55;
const ROLE_TABLE_CELL: u32 = 56;
const ROLE_TABLE_COLUMN_HEADER: u32 = 57;
const ROLE_TABLE_ROW_HEADER: u32 = 58;
const ROLE_TEAROFF_MENU_ITEM: u32 = 59;
const ROLE_TEXT: u32 = 61;
const ROLE_TOGGLE_BUTTON: u32 = 62;
const ROLE_TOOL_BAR: u32 = 63;
const ROLE_TREE: u32 = 65;
const ROLE_TREE_TABLE: u32 = 66;
const ROLE_WINDOW: u32 = 69;
const ROLE_APPLICATION: u32 = 75;
const ROLE_ENTRY: u32 = 79;
const ROLE_CAPTION: u32 = 81;
const ROLE_DOCUMENT_FRAME: u32 = 82;
const ROLE_HEADING: u32 = 83;
const ROLE_SECTION: u32 = 85;
const ROLE_FORM: u32 = 87;
const ROLE_LINK: u32 = 88;
const ROLE_TABLE_ROW: u32 = 90;
const ROLE_TREE_ITEM: u32 = 91;
const ROLE_DOCUMENT_WEB: u32 = 95;
const ROLE_LIST_BOX: u32 = 98;
const ROLE_GROUPING: u32 = 99;
const ROLE_NOTIFICATION: u32 = 101;
const ROLE_STATIC: u32 = 116;

// AT-SPI2 state bit positions
const STATE_BUSY: u32 = 3;
const STATE_CHECKED: u32 = 4;
const STATE_EDITABLE: u32 = 7;
const STATE_ENABLED: u32 = 8;
const STATE_EXPANDABLE: u32 = 9;
const STATE_EXPANDED: u32 = 10;
const STATE_FOCUSED: u32 = 12;
const STATE_SELECTED: u32 = 23;
const STATE_SHOWING: u32 = 25;
const STATE_VISIBLE: u32 = 30;
// States in second u32 (bit positions 32+)
const STATE_INDETERMINATE: u32 = 32;
const STATE_REQUIRED: u32 = 33;
const STATE_CHECKABLE: u32 = 41;

/// Map AT-SPI2 role ID to xa11y Role.
pub fn map_role(role_id: u32) -> Role {
    match role_id {
        ROLE_ALERT | ROLE_NOTIFICATION => Role::Alert,
        ROLE_APPLICATION => Role::Application,
        ROLE_PUSH_BUTTON | ROLE_TOGGLE_BUTTON => Role::Button,
        ROLE_CHECK_BOX | ROLE_CHECK_MENU_ITEM => Role::CheckBox,
        ROLE_COMBO_BOX => Role::ComboBox,
        ROLE_DIALOG | ROLE_FILE_CHOOSER => Role::Dialog,
        ROLE_PANEL | ROLE_SECTION | ROLE_FORM | ROLE_FILLER | ROLE_GROUPING => Role::Group,
        ROLE_HEADING => Role::Heading,
        ROLE_IMAGE | ROLE_ICON | ROLE_DESKTOP_ICON => Role::Image,
        ROLE_LINK => Role::Link,
        ROLE_LIST | ROLE_LIST_BOX | ROLE_TREE => Role::List,
        ROLE_LIST_ITEM => Role::ListItem,
        ROLE_MENU | ROLE_POPUP_MENU => Role::Menu,
        ROLE_MENU_BAR => Role::MenuBar,
        ROLE_MENU_ITEM | ROLE_TEAROFF_MENU_ITEM => Role::MenuItem,
        ROLE_PROGRESS_BAR => Role::ProgressBar,
        ROLE_RADIO_BUTTON | ROLE_RADIO_MENU_ITEM => Role::RadioButton,
        ROLE_SCROLL_BAR => Role::ScrollBar,
        ROLE_SEPARATOR => Role::Separator,
        ROLE_SLIDER => Role::Slider,
        ROLE_SPLIT_PANE => Role::SplitGroup,
        ROLE_LABEL | ROLE_CAPTION | ROLE_STATIC => Role::StaticText,
        ROLE_PAGE_TAB => Role::Tab,
        ROLE_PAGE_TAB_LIST => Role::TabGroup,
        ROLE_TABLE | ROLE_TREE_TABLE => Role::Table,
        ROLE_TABLE_CELL | ROLE_TABLE_COLUMN_HEADER | ROLE_TABLE_ROW_HEADER => Role::TableCell,
        ROLE_TABLE_ROW => Role::TableRow,
        ROLE_TEXT => Role::TextArea,
        ROLE_ENTRY | ROLE_PASSWORD_TEXT | ROLE_SPIN_BUTTON => Role::TextField,
        ROLE_TOOL_BAR => Role::Toolbar,
        ROLE_TREE_ITEM => Role::TreeItem,
        ROLE_DOCUMENT_FRAME | ROLE_DOCUMENT_WEB => Role::WebArea,
        ROLE_FRAME | ROLE_WINDOW => Role::Window,
        _ => Role::Unknown,
    }
}

/// Map AT-SPI2 state bitfield to xa11y StateSet.
pub fn map_states(state_bits: &[u32]) -> StateSet {
    let bits: u64 = match state_bits.len() {
        0 => return StateSet::default(),
        1 => state_bits[0] as u64,
        _ => (state_bits[0] as u64) | ((state_bits[1] as u64) << 32),
    };

    let has = |bit: u32| -> bool { bits & (1u64 << bit) != 0 };

    StateSet {
        enabled: has(STATE_ENABLED),
        visible: has(STATE_VISIBLE) || has(STATE_SHOWING),
        focused: has(STATE_FOCUSED),
        checked: if has(STATE_CHECKABLE) || has(STATE_CHECKED) {
            if has(STATE_INDETERMINATE) {
                Some(Toggled::Mixed)
            } else if has(STATE_CHECKED) {
                Some(Toggled::On)
            } else {
                Some(Toggled::Off)
            }
        } else {
            None
        },
        selected: has(STATE_SELECTED),
        expanded: if has(STATE_EXPANDABLE) {
            Some(has(STATE_EXPANDED))
        } else {
            None
        },
        editable: has(STATE_EDITABLE),
        required: has(STATE_REQUIRED),
        busy: has(STATE_BUSY),
    }
}

/// Map an AT-SPI2 action name string to xa11y Action.
pub fn map_action_name(name: &str) -> Option<Action> {
    match name.to_lowercase().as_str() {
        "click" | "activate" | "press" | "invoke" => Some(Action::Press),
        "toggle" | "check" | "uncheck" => Some(Action::Toggle),
        "expand" | "open" => Some(Action::Expand),
        "collapse" | "close" => Some(Action::Collapse),
        "select" => Some(Action::Select),
        "menu" | "showmenu" | "popup" | "show menu" | "show_menu" => Some(Action::ShowMenu),
        "increment" => Some(Action::Increment),
        "decrement" => Some(Action::Decrement),
        _ => None,
    }
}

/// Get AT-SPI2 action names that correspond to an xa11y Action.
pub fn atspi_names_for_action(action: &Action) -> &'static [&'static str] {
    match action {
        Action::Press => &["click", "activate", "press", "invoke"],
        Action::Toggle => &["toggle", "check", "uncheck", "click"],
        Action::Expand => &["expand", "open"],
        Action::Collapse => &["collapse", "close"],
        Action::Select => &["select"],
        Action::ShowMenu => &["menu", "showmenu", "popup", "show menu"],
        Action::Increment => &["increment"],
        Action::Decrement => &["decrement"],
        // Focus, SetValue, ScrollIntoView handled via other interfaces
        _ => &[],
    }
}
