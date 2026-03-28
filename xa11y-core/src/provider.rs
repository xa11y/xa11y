use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionData};
use crate::error::Result;
use crate::node::NodeData;
use crate::tree::Tree;

/// Platform backend trait for accessibility tree access.
pub trait Provider: Send + Sync {
    /// Snapshot a specific application's accessibility tree.
    fn get_app_tree(&self, target: &AppTarget) -> Result<Tree>;

    /// Snapshot all running applications (shallow).
    fn get_apps(&self) -> Result<Tree>;

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

/// Result of a permission check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionStatus {
    /// Accessibility permissions are granted.
    Granted,
    /// Permissions denied, with platform-specific instructions.
    Denied { instructions: String },
}
