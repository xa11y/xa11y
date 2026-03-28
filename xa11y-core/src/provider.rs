use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionData};
use crate::error::Result;
use crate::node::NodeData;
use crate::tree::Tree;

/// Platform backend trait for accessibility tree access.
pub trait Provider: Send + Sync {
    /// Snapshot a specific application's tree by display name.
    ///
    /// Name matching is case-insensitive substring.
    fn get_tree_by_name(&self, name: &str) -> Result<Tree>;

    /// Snapshot a specific application's tree by process ID.
    fn get_tree_by_pid(&self, pid: u32) -> Result<Tree>;

    /// Snapshot a specific application's tree by platform window handle.
    fn get_tree_by_window(&self, handle: &WindowHandle) -> Result<Tree>;

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

/// Internal lookup key for identifying an application across snapshots.
///
/// Used by `App` and `Locator` to dispatch to the correct `Provider` method.
/// Not part of the public API.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub enum AppLookup {
    ByName(String),
    ByPid(u32),
    ByWindow(WindowHandle),
}

impl AppLookup {
    /// Fetch the tree for this lookup target.
    #[doc(hidden)]
    pub fn fetch_tree(&self, provider: &dyn Provider) -> Result<Tree> {
        match self {
            Self::ByName(name) => provider.get_tree_by_name(name),
            Self::ByPid(pid) => provider.get_tree_by_pid(*pid),
            Self::ByWindow(handle) => provider.get_tree_by_window(handle),
        }
    }
}
