//! xa11y — Cross-Platform Accessibility Client Library
//!
//! Provides a unified API over platform-specific accessibility APIs,
//! letting consumers query UI structure and perform actions without
//! writing platform-specific code.
//!
//! # Example
//!
//! ```rust,no_run
//! use xa11y::*;
//!
//! let provider = create_provider();
//! let status = provider.check_permissions().unwrap();
//! let tree = provider.get_app_tree(
//!     &AppTarget::ByName("Safari".into()),
//!     &QueryOptions::default(),
//! ).unwrap();
//!
//! let buttons = tree.find_by_role(Role::Button);
//! ```

// Re-export all core types
pub use xa11y_core::*;

/// Create a platform-appropriate accessibility provider.
///
/// Returns a boxed `Provider` implementation for the current platform.
///
/// # Panics
///
/// Panics on unsupported platforms.
pub fn create_provider() -> Box<dyn Provider> {
    #[cfg(target_os = "macos")]
    {
        Box::new(xa11y_macos::create_provider())
    }

    #[cfg(target_os = "windows")]
    {
        Box::new(xa11y_windows::create_provider())
    }

    #[cfg(target_os = "linux")]
    {
        Box::new(xa11y_linux::create_provider())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        panic!("xa11y: unsupported platform")
    }
}
