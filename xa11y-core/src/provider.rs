use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionData};
use crate::error::Result;
use crate::node::NodeData;
use crate::role::Role;
use crate::tree::Tree;

/// Platform backend trait for accessibility tree access.
pub trait Provider: Send + Sync {
    /// Snapshot a specific application's accessibility tree.
    fn get_app_tree(&self, target: &AppTarget, opts: &QueryOptions) -> Result<Tree>;

    /// Snapshot all running applications (shallow).
    fn get_all_apps(&self, opts: &QueryOptions) -> Result<Tree>;

    /// Perform an action on an element from a specific snapshot.
    ///
    /// `Ok(())` means the platform API accepted the request without error.
    /// It does **not** guarantee the action had an observable effect — use
    /// tree queries or `Locator::wait_*` methods to verify state changes.
    fn perform_action(
        &self,
        tree: &Tree,
        node: &NodeData,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()>;

    /// Check if accessibility permissions are granted.
    fn check_permissions(&self) -> Result<PermissionStatus>;

    /// List running applications with their PIDs.
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
