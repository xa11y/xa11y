//! Fuzz target for xa11y-core serde round-tripping (NOT platform providers).
//! Deserializes random JSON as Tree and exercises tree methods.
#![no_main]

use libfuzzer_sys::fuzz_target;
use xa11y_core::{Role, Tree};

const ROLES: [Role; 33] = [
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
];

fuzz_target!(|data: &[u8]| {
    // Strategy 1: Try to deserialize raw bytes as JSON into a Tree.
    if let Ok(mut tree) = serde_json::from_slice::<Tree>(data) {
        // Rebuild the index since it's skipped during deserialization.
        tree.rebuild_index();

        // Exercise tree methods on the deserialized tree.
        let _ = tree.len();
        let _ = tree.is_empty();

        if !tree.is_empty() {
            let root = tree.root();
            let _ = root.id;

            // Exercise get on all nodes.
            for node in tree.iter() {
                let _ = tree.get(node.id);
                let _ = tree.children(node.id);
            }

            let _ = tree.subtree(root.id);
            let _ = tree.dump();

            // Try find_by_role with a few roles.
            for role in &ROLES[..5] {
                let _ = tree.find_by_role(*role);
            }

            let _ = tree.find_by_name("test");

            // Try a query.
            let _ = tree.query("button");
            let _ = tree.query("[name*=\"a\"]");
        }

        // Round-trip: serialize back and verify it doesn't panic.
        let _ = serde_json::to_string(&tree);
    }

    // Strategy 2: Try to interpret as a UTF-8 JSON string.
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(mut tree) = serde_json::from_str::<Tree>(s) {
            tree.rebuild_index();
            let _ = tree.len();
            if !tree.is_empty() {
                let _ = tree.root();
                let _ = tree.dump();
            }
        }
    }
});
