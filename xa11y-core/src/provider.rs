use crate::action::{Action, ActionData};
use crate::element::ElementData;
use crate::error::Result;
use crate::tree::Tree;

use serde::{Deserialize, Serialize};

/// Platform backend trait for accessibility tree access.
pub trait Provider: Send + Sync {
    /// Resolve an application name to a PID.
    ///
    /// Name matching is case-insensitive substring. Returns the PID of the
    /// first matching application.
    fn resolve_pid_by_name(&self, name: &str) -> Result<u32>;

    /// Snapshot a specific application's accessibility tree by PID.
    fn get_tree(&self, pid: u32) -> Result<Tree>;

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
        element: &ElementData,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()>;

    /// Check if accessibility permissions are granted.
    fn check_permissions(&self) -> Result<PermissionStatus>;
}

/// Result of a permission check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionStatus {
    /// Accessibility permissions are granted.
    Granted,
    /// Permissions denied, with platform-specific instructions.
    Denied { instructions: String },
}
