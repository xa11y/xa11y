use crate::action::{Action, ActionData};
use crate::element::Element;
use crate::error::Result;
use crate::event_provider::Subscription;

use serde::{Deserialize, Serialize};

/// Platform backend trait for accessibility tree access.
pub trait Provider: Send + Sync {
    /// Resolve an application name to a PID.
    ///
    /// Name matching is case-insensitive substring. Returns the PID of the
    /// first matching application.
    fn resolve_pid_by_name(&self, name: &str) -> Result<u32>;

    /// Snapshot a specific application's accessibility tree by PID.
    ///
    /// Returns the root [`Element`] of the snapshot.
    fn get_elements(&self, pid: u32) -> Result<Element>;

    /// Snapshot all running applications (shallow).
    ///
    /// Returns the root [`Element`] of the apps tree.
    fn get_apps(&self) -> Result<Element>;

    /// Perform an action on an element from a specific snapshot.
    ///
    /// `Ok(())` means the platform API accepted the request without error.
    /// It does **not** guarantee the action had an observable effect — use
    /// `Locator::wait_*` methods to verify state changes.
    fn perform_action(
        &self,
        element: &Element,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()>;

    /// Check if accessibility permissions are granted.
    fn check_permissions(&self) -> Result<PermissionStatus>;

    /// Subscribe to all accessibility events for an application by PID.
    ///
    /// Returns a [`Subscription`] that receives events until dropped.
    /// Use [`Subscription::try_recv`], [`Subscription::recv`], or
    /// [`Subscription::wait_for`] to consume events.
    fn subscribe(&self, pid: u32) -> Result<Subscription>;
}

/// Result of a permission check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionStatus {
    /// Accessibility permissions are granted.
    Granted,
    /// Permissions denied, with platform-specific instructions.
    Denied { instructions: String },
}
