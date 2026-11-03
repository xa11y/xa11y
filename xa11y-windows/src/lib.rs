//! Windows accessibility backend using UI Automation (UIA).
//!
//! This backend implements the `Provider` trait using the Windows UI Automation API.
//! No special permissions are required for local UIA queries.
//!
//! # Delivery disposition of action failures
//!
//! Every error answers [`xa11y_core::Error::delivery`]: whether the failed
//! action may already have taken effect. A caller retries only on
//! `NotDelivered`. This is how UIA failures map.
//!
//! `NotDelivered`:
//!
//! - `ActionNotSupported`: the element exposes no pattern that implements the
//!   verb (`Invoke`, `Toggle`, `SelectionItem`, `ExpandCollapse`, `Value`,
//!   `RangeValue`, `Window`, ...), or a pattern reports that the operation
//!   is unavailable (`UIA_E_NOTSUPPORTED`, `UIA_E_INVALIDOPERATION` before the
//!   call runs).
//! - `ElementStale`: the xa11y handle is not in the provider's cache, or the
//!   element no longer matches its recorded identity, so there is nothing to
//!   call.
//! - `InvalidActionData`, `InvalidSelector`, `NoElementBounds`,
//!   `Unsupported`: rejected from the arguments before any UIA call.
//!
//! `Unknown`:
//!
//! - `UIA_E_TIMEOUT` from an element-level call, as a `Platform` error. The
//!   provider may have run the pattern method before the transaction timed
//!   out.
//! - Every other `Platform` HRESULT from the pattern call that performs the
//!   action, including `UIA_E_ELEMENTNOTAVAILABLE`, which can also arrive
//!   after the provider acted on an element that then went away.
//! - `Timeout` from a discovery call or from a window verb that was sent and
//!   did not settle. The backend does not split a discovery timeout apart
//!   from a dispatch timeout, so both stay `Unknown`.

mod splice;

#[cfg(target_os = "windows")]
mod coords;
#[cfg(target_os = "windows")]
mod dpi;
#[cfg(target_os = "windows")]
mod input;
#[cfg(target_os = "windows")]
mod uia;
#[cfg(target_os = "windows")]
mod uipi;

mod screenshot;

#[cfg(target_os = "windows")]
pub use input::WindowsInputProvider;
#[cfg(target_os = "windows")]
pub use uia::WindowsProvider;

/// How many times an xa11y call on this thread switched it to Per-Monitor V2
/// DPI awareness. Test support for the system-aware host test; not API.
#[doc(hidden)]
#[cfg(target_os = "windows")]
pub fn __dpi_scope_switches() -> u64 {
    dpi::scope_switches()
}

pub use screenshot::WindowsScreenshot;

#[cfg(not(target_os = "windows"))]
mod stub;

#[cfg(not(target_os = "windows"))]
pub use stub::{WindowsInputProvider, WindowsProvider};

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use xa11y_core::*;

    #[test]
    fn create_provider() {
        let result = WindowsProvider::new();
        #[cfg(target_os = "windows")]
        // No E_FAIL skip: the first-touch race it hid is serialized now
        // (issue #500), so any failure is a regression.
        if let Err(e) = &result {
            panic!("WindowsProvider::new failed: {e}");
        }
        #[cfg(not(target_os = "windows"))]
        assert!(result.is_err());
    }
}
