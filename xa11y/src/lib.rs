//! xa11y — Cross-Platform Accessibility Client Library
//!
//! Provides a unified API for reading and interacting with accessibility trees
//! across desktop platforms (macOS, Windows, Linux).
//!
//! # Quick Start
//!
//! ```no_run
//! use xa11y::*;
//!
//! let provider = create_provider().expect("Failed to create provider");
//! let status = provider.check_permissions().expect("Permission check failed");
//!
//! match status {
//!     PermissionStatus::Granted => {
//!         let tree = provider.get_app_tree(
//!             &AppTarget::ByName("Safari".to_string()),
//!             &QueryOptions::default(),
//!         ).expect("Failed to get tree");
//!
//!         let buttons = tree.query("button").expect("Query failed");
//!         println!("Found {} buttons", buttons.len());
//!     }
//!     PermissionStatus::Denied { instructions } => {
//!         eprintln!("Accessibility not enabled: {}", instructions);
//!     }
//! }
//! ```

// Re-export all core types
pub use xa11y_core::*;

// Platform-specific provider creation

/// Create a platform-appropriate accessibility provider.
///
/// Returns a boxed `Provider` trait object for the current platform.
/// On unsupported platforms, returns a `Platform` error.
pub fn create_provider() -> Result<Box<dyn Provider>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(xa11y_macos::MacOSProvider::new()?))
    }

    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(xa11y_windows::WindowsProvider::new()?))
    }

    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(xa11y_linux::LinuxProvider::new()?))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err(Error::Platform {
            code: -1,
            message: format!("Unsupported platform: {}", std::env::consts::OS),
        })
    }
}

/// Create a platform-appropriate event provider (supports subscribe/wait).
///
/// Returns a boxed `EventProvider` trait object for the current platform.
/// EventProvider extends Provider with event subscription capabilities.
pub fn create_event_provider() -> Result<Box<dyn EventProvider>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(xa11y_macos::MacOSProvider::new()?))
    }

    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(xa11y_windows::WindowsProvider::new()?))
    }

    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(xa11y_linux::LinuxProvider::new()?))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err(Error::Platform {
            code: -1,
            message: format!("Unsupported platform: {}", std::env::consts::OS),
        })
    }
}
