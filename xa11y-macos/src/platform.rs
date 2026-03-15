use xa11y_core::*;

/// macOS accessibility provider using AXUIElement APIs.
pub struct MacOSProvider {
    // Internal state for caching element handles
}

impl MacOSProvider {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for MacOSProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for MacOSProvider {
    fn get_app_tree(&self, _target: &AppTarget, _opts: &QueryOptions) -> Result<Tree> {
        // TODO: Implement AXUIElement traversal
        // 1. Find the target app via NSWorkspace or AXUIElementCreateApplication(pid)
        // 2. DFS traverse the accessibility tree
        // 3. Map AX roles/attributes to xa11y types
        // 4. Cache live AXUIElementRef handles for action dispatch
        Err(Error::Platform("macOS provider not yet implemented".into()))
    }

    fn get_all_apps(&self, _opts: &QueryOptions) -> Result<Tree> {
        Err(Error::Platform("macOS provider not yet implemented".into()))
    }

    fn perform_action(
        &self,
        _node_id: NodeId,
        _action: Action,
        _data: Option<ActionData>,
    ) -> Result<()> {
        Err(Error::Platform("macOS provider not yet implemented".into()))
    }

    fn check_permissions(&self) -> Result<PermissionStatus> {
        // TODO: Call AXIsProcessTrusted()
        Err(Error::Platform("macOS provider not yet implemented".into()))
    }

    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        Err(Error::Platform("macOS provider not yet implemented".into()))
    }
}
