use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionData};
use crate::error::Result;
use crate::node::RawNode;
use crate::role::Role;
use crate::tree::Tree;

/// Platform backend trait for accessibility tree access.
pub trait Provider: Send + Sync {
    /// Snapshot a specific application's accessibility tree.
    fn get_app_tree(&self, target: &AppTarget, opts: &QueryOptions) -> Result<Tree>;

    /// Snapshot all running applications (shallow).
    fn get_all_apps(&self, opts: &QueryOptions) -> Result<Tree>;

    /// Perform an action on an element identified by its index in the tree.
    ///
    /// `Ok(())` means the platform API accepted the request without error.
    /// It does **not** guarantee the action had an observable effect.
    fn perform_action_raw(
        &self,
        tree: &Tree,
        node_index: u32,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()>;

    /// Check if accessibility permissions are granted.
    fn check_permissions(&self) -> Result<PermissionStatus>;

    /// List running applications with their PIDs.
    ///
    /// Deprecated: use `get_all_apps` + query for `application` nodes instead.
    fn list_apps(&self) -> Result<Vec<AppInfo>>;
}

/// Target for identifying an application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppTarget {
    /// Match by human-readable display name (case-insensitive, substring match).
    ByName(String),
    /// Match by process ID.
    ByPid(u32),
    /// Target a specific window by platform-specific handle.
    ByWindow(WindowHandle),
}

/// Platform-specific window handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WindowHandle {
    /// macOS CGWindowID
    MacOS(u32),
    /// Windows HWND (as usize for pointer-sized value)
    Windows(usize),
    /// Linux X11 window ID
    X11(u64),
}

/// Options controlling tree traversal and content.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueryOptions {
    /// Maximum tree depth to traverse. `None` = unlimited.
    pub max_depth: Option<u32>,
    /// Maximum number of elements to collect. `None` = unlimited.
    pub max_elements: Option<u32>,
    /// Only include visible elements.
    pub visible_only: bool,
    /// Filter to specific roles. Empty = no filter.
    pub roles: Vec<Role>,
}

/// Result of a permission check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionStatus {
    /// Accessibility permissions are granted.
    Granted,
    /// Permissions denied, with platform-specific instructions.
    Denied { instructions: String },
}

/// Information about a running application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub pid: u32,
    /// macOS bundle identifier
    pub bundle_id: Option<String>,
}

// ── Compatibility shim ──────────────────────────────────────────────────────

/// Extension trait providing the old `perform_action(&Tree, &RawNode, ...)` signature
/// by delegating to `perform_action_raw`.
///
/// This allows backends that haven't migrated yet to keep working through
/// a blanket impl, and also allows callers with `&RawNode` to dispatch easily.
pub trait ProviderExt: Provider {
    fn perform_action(
        &self,
        tree: &Tree,
        node: &RawNode,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()>;
}

impl<T: Provider + ?Sized> ProviderExt for T {
    fn perform_action(
        &self,
        tree: &Tree,
        node: &RawNode,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()> {
        self.perform_action_raw(tree, node.index, action, data)
    }
}
