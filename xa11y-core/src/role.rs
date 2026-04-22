use serde::{Deserialize, Serialize};

/// A normalized enum covering UI element types across all platforms.
/// Derived from ARIA roles, scoped to roles commonly surfaced by real desktop applications.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
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
    /// Scroll thumb — the draggable indicator inside a scroll bar
    ScrollThumb,
}

impl Role {
    /// Parse a snake_case role name into a Role enum variant.
    /// Returns `None` if the name doesn't match any known role.
    pub fn from_snake_case(s: &str) -> Option<Self> {
        s.parse::<Role>().ok()
    }

    /// Convert a Role to its snake_case string representation.
    pub fn to_snake_case(self) -> &'static str {
        self.into()
    }
}

/// Returns `Role::Unknown` normally, but panics with a descriptive message when
/// the `strict-roles` feature is enabled. Platform backends call this in their
/// catch-all mapping arms so that integration tests surface unmapped roles.
#[inline]
pub fn unknown_role(context: &str) -> Role {
    if cfg!(feature = "strict-roles") {
        panic!("strict-roles: unmapped platform role — {context}");
    }
    Role::Unknown
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_snake_case())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_thumb_roundtrips() {
        assert_eq!(Role::ScrollThumb.to_snake_case(), "scroll_thumb");
        assert_eq!(
            Role::from_snake_case("scroll_thumb"),
            Some(Role::ScrollThumb)
        );
        assert_eq!(format!("{}", Role::ScrollThumb), "scroll_thumb");
    }

    #[test]
    fn all_roles_roundtrip() {
        // Every role must parse back from its own snake_case representation.
        let roles = [
            Role::Unknown,
            Role::Window,
            Role::Application,
            Role::Button,
            Role::CheckBox,
            Role::RadioButton,
            Role::TextField,
            Role::TextArea,
            Role::StaticText,
            Role::ComboBox,
            Role::List,
            Role::ListItem,
            Role::Menu,
            Role::MenuItem,
            Role::MenuBar,
            Role::Tab,
            Role::TabGroup,
            Role::Table,
            Role::TableRow,
            Role::TableCell,
            Role::Toolbar,
            Role::ScrollBar,
            Role::ScrollThumb,
            Role::Slider,
            Role::Image,
            Role::Link,
            Role::Group,
            Role::Dialog,
            Role::Alert,
            Role::ProgressBar,
            Role::TreeItem,
            Role::WebArea,
            Role::Heading,
            Role::Separator,
            Role::SplitGroup,
            Role::Switch,
            Role::SpinButton,
            Role::Tooltip,
            Role::Status,
            Role::Navigation,
        ];
        for role in roles {
            let s = role.to_snake_case();
            assert_eq!(
                Role::from_snake_case(s),
                Some(role),
                "roundtrip failed for {s}"
            );
        }
    }
}
