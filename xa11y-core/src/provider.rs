use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionData};
use crate::error::Result;
use crate::event::{ElementState, Event, EventFilter};
use crate::node::{Node, NodeId};
use crate::role::Role;
use crate::tree::Tree;

/// Target application for accessibility queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppTarget {
    /// Find application by name
    ByName(String),
    /// Find application by process ID
    ByPid(u32),
    /// Target a specific window by platform-specific handle
    ByWindow(WindowHandle),
}

/// Platform-specific window handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowHandle {
    /// Platform-specific window identifier
    pub id: u64,
}

/// Options controlling tree traversal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryOptions {
    /// Maximum depth to traverse (0 = root only)
    pub max_depth: u32,
    /// Maximum number of elements to return
    pub max_elements: u32,
    /// Only include visible elements
    pub visible_only: bool,
    /// Filter to specific roles
    pub roles: Option<Vec<Role>>,
    /// Include platform-specific raw data
    pub include_raw: bool,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            max_depth: u32::MAX,
            max_elements: u32::MAX,
            visible_only: false,
            roles: None,
            include_raw: false,
        }
    }
}

/// Information about a running application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    /// Application name
    pub name: String,
    /// Process ID
    pub pid: u32,
    /// Bundle identifier (macOS)
    pub bundle_id: Option<String>,
}

/// Accessibility permission status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionStatus {
    /// Accessibility access is granted
    Granted,
    /// Accessibility access is denied
    Denied {
        /// Human-readable instructions for granting access
        instructions: String,
    },
}

/// Platform backend trait for accessing accessibility trees.
pub trait Provider: Send + Sync {
    /// Snapshot a specific application's accessibility tree.
    fn get_app_tree(&self, target: &AppTarget, opts: &QueryOptions) -> Result<Tree>;

    /// Snapshot all running applications (shallow).
    fn get_all_apps(&self, opts: &QueryOptions) -> Result<Tree>;

    /// Perform an action on an element from the last snapshot.
    fn perform_action(
        &self,
        node_id: NodeId,
        action: Action,
        data: Option<ActionData>,
    ) -> Result<()>;

    /// Check if accessibility permissions are granted.
    fn check_permissions(&self) -> Result<PermissionStatus>;

    /// List running applications with their PIDs.
    fn list_apps(&self) -> Result<Vec<AppInfo>>;
}

/// Optional trait for backends that support event subscriptions.
pub trait EventProvider: Provider {
    /// Subscribe to events matching the given filter.
    fn subscribe(&self, target: &AppTarget, filter: EventFilter) -> Result<Subscription>;

    /// Wait for a single event matching the filter, with timeout.
    fn wait_for_event(
        &self,
        target: &AppTarget,
        filter: EventFilter,
        timeout: Duration,
    ) -> Result<Event>;

    /// Wait for an element matching the selector to reach the desired state.
    fn wait_for(
        &self,
        target: &AppTarget,
        selector: &str,
        state: ElementState,
        timeout: Duration,
    ) -> Result<Node>;
}

/// A live event subscription. Drop to unsubscribe.
pub struct Subscription {
    /// Receive events from this channel.
    pub rx: EventReceiver,
    /// Internal cancel handle
    _cancel: Box<dyn Send>,
}

impl Subscription {
    /// Create a new subscription with a receiver and cancel handle.
    pub fn new(rx: EventReceiver, cancel: Box<dyn Send>) -> Self {
        Self {
            rx,
            _cancel: cancel,
        }
    }
}

/// Platform-agnostic event receiver.
pub struct EventReceiver {
    inner: Box<dyn FnMut() -> Option<Event> + Send>,
}

impl EventReceiver {
    /// Create a new event receiver from a polling function.
    pub fn new(poll_fn: Box<dyn FnMut() -> Option<Event> + Send>) -> Self {
        Self { inner: poll_fn }
    }

    /// Try to receive without blocking (returns None if no event ready).
    pub fn try_recv(&mut self) -> Option<Event> {
        (self.inner)()
    }
}
