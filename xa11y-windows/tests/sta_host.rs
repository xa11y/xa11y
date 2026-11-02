//! Repro for issue #499: a host whose thread is already STA creates the
//! provider, then other threads use it.
//!
//! `WindowsProvider::new` asks for the MTA and accepts `RPC_E_CHANGED_MODE`,
//! which means the calling thread was already initialised as a
//! single-threaded apartment (Python with comtypes / pywinauto imported first,
//! Electron's main thread). The UIA object is then created in that STA and
//! shared across threads through `unsafe impl Send + Sync`. If UIA hands back
//! an apartment-bound object, a call from another thread needs the STA to pump
//! messages; this test's STA thread deliberately does not pump while the
//! workers run, as a host's busy main thread would not, so that shape shows
//! up as a deadlock (caught by the timeout) or a COM error.
//!
//! Each worker reads the desktop's app list and every app's windows, which is
//! the provider's ordinary tree-reading path. The host thread also checks
//! the claim the fix rests on: a `CUIAutomation8` and an element created in
//! an STA are agile (`IAgileObject`, or the free-threaded marshaler).
//!
//! `#[ignore]`d: it needs a desktop with at least one window (the AccessKit
//! test app), so `scripts/run_integ_tests_windows.ps1` runs it with
//! `--ignored`.

#![cfg(target_os = "windows")]

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use windows::core::{Interface, GUID};
use windows::Win32::System::Com::Marshal::IMarshal;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IAgileObject, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Accessibility::{CUIAutomation8, IUIAutomation};
use xa11y_core::{Error, Provider};
use xa11y_windows::WindowsProvider;

const WORKERS: usize = 4;
const ROUNDS: usize = 3;
/// Far beyond what an app listing takes; reaching it means a worker is stuck.
const DEADLINE: Duration = Duration::from_secs(60);
/// `CLSID_InProcFreeMarshaler`: the unmarshal class of an object that
/// aggregates the free-threaded marshaler.
const FREE_THREADED_MARSHALER: GUID = GUID::from_u128(0x0000033a_0000_0000_c000_000000000046);
/// `MSHCTX_INPROC`.
const MSHCTX_INPROC: u32 = 3;

/// Whether `object` can be called from any apartment without a proxy:
/// `IAgileObject`, or an `IMarshal` whose unmarshal class is the
/// free-threaded marshaler. The `Err` says what it is instead.
fn agility<T: Interface>(what: &str, object: &T) -> Result<(), String> {
    if object.cast::<IAgileObject>().is_ok() {
        return Ok(());
    }
    let marshal = object
        .cast::<IMarshal>()
        .map_err(|e| format!("{what}: neither IAgileObject nor IMarshal ({e})"))?;
    let class = unsafe { marshal.GetUnmarshalClass(&T::IID, None, MSHCTX_INPROC, None, 0) }
        .map_err(|e| format!("{what}: IMarshal.GetUnmarshalClass failed ({e})"))?;
    if class == FREE_THREADED_MARSHALER {
        Ok(())
    } else {
        Err(format!(
            "{what}: no IAgileObject, and its in-process unmarshal class is {class:?}, not the \
             free-threaded marshaler"
        ))
    }
}

/// On the (STA) calling thread: create a `CUIAutomation8` and its root
/// element, and check that both are agile.
fn check_sta_objects_are_agile() -> Result<(), String> {
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("CoCreateInstance(CUIAutomation8) failed: {e}"))?;
    agility("CUIAutomation8", &automation)?;
    let root = unsafe { automation.GetRootElement() }
        .map_err(|e| format!("GetRootElement failed: {e}"))?;
    agility("the root IUIAutomationElement", &root)
}

#[test]
#[ignore = "needs a desktop with the AccessKit test app; run via scripts/run_integ_tests_windows.ps1"]
fn provider_created_on_an_sta_thread_is_usable_from_other_threads() {
    let (agility_tx, agility_rx) = mpsc::channel();
    let (provider_tx, provider_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel::<()>();

    // The host thread: STA first, then the provider, then busy (no pumping)
    // until the workers finish.
    let host = std::thread::spawn(move || {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .expect("the host thread becomes an STA");
        agility_tx
            .send(check_sta_objects_are_agile())
            .expect("the test thread is waiting for the agility check");
        let provider = WindowsProvider::new().map(Arc::new);
        let created = provider.is_ok();
        provider_tx
            .send(provider)
            .expect("the test thread is waiting for the provider");
        if created {
            // Block without a message loop, as a busy host thread would. An
            // error only means the test thread already finished or panicked;
            // either way this thread is done.
            let _ = done_rx.recv();
        }
        unsafe { CoUninitialize() };
    });

    agility_rx
        .recv()
        .expect("the host thread sends the agility check")
        .unwrap_or_else(|e| {
            panic!("a UIA object created in an STA is not free-threaded, so #499's fix is unsound: {e}")
        });

    let provider = provider_rx
        .recv()
        .expect("the host thread sends its provider")
        .expect("WindowsProvider::new on an STA thread");

    let (result_tx, result_rx) = mpsc::channel();
    for worker in 0..WORKERS {
        let provider = Arc::clone(&provider);
        let result_tx = result_tx.clone();
        std::thread::spawn(move || {
            let outcome = (|| -> xa11y_core::Result<usize> {
                let mut windows = 0;
                for _ in 0..ROUNDS {
                    for app in provider.get_children(None)? {
                        windows += match provider.app_windows(&app) {
                            Ok(w) => w.len(),
                            // An app can exit between the listing and this
                            // read. Only that is tolerated; a COM failure
                            // (Platform) is what this test looks for.
                            Err(Error::ElementStale { .. } | Error::SelectorNotMatched { .. }) => 0,
                            Err(e) => return Err(e),
                        };
                    }
                }
                Ok(windows)
            })();
            // The receiver is gone only if the test already panicked.
            let _ = result_tx.send((worker, outcome));
        });
    }
    drop(result_tx);

    for _ in 0..WORKERS {
        let (worker, outcome) = match result_rx.recv_timeout(DEADLINE) {
            Ok(received) => received,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!(
                "a worker made no progress within {DEADLINE:?}: a UIA object created on a \
                 non-pumping STA thread deadlocks when called from another thread (#499)"
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!(
                "every worker thread ended without reporting; one panicked (see its output above)"
            ),
        };
        let windows = outcome.unwrap_or_else(|e| {
            panic!("worker {worker}: reading UIA through an STA-created provider failed: {e}")
        });
        assert!(
            windows > 0,
            "worker {worker} read no windows at all, so the test proved nothing; is the \
             AccessKit test app running?"
        );
    }

    // The host thread may already have exited if creating the provider failed.
    let _ = done_tx.send(());
    host.join().expect("host thread exits");
}
