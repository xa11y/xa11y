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
//! the provider's ordinary tree-reading path.

#![cfg(target_os = "windows")]

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use xa11y_core::Provider;
use xa11y_windows::WindowsProvider;

const WORKERS: usize = 4;
const ROUNDS: usize = 3;
/// Far beyond what an app listing takes; reaching it means a worker is stuck.
const DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn provider_created_on_an_sta_thread_is_usable_from_other_threads() {
    let (provider_tx, provider_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel::<()>();

    // The host thread: STA first, then the provider, then busy (no pumping)
    // until the workers finish.
    let host = std::thread::spawn(move || {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .expect("the host thread becomes an STA");
        let provider = WindowsProvider::new().map(Arc::new);
        let created = provider.is_ok();
        provider_tx
            .send(provider)
            .expect("the test thread is waiting for the provider");
        if created {
            // Block without a message loop, as a busy host thread would.
            let _ = done_rx.recv();
        }
        unsafe { CoUninitialize() };
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
                        windows += provider.app_windows(&app).map(|w| w.len()).unwrap_or_else(
                            // An app can exit between the listing and this
                            // read; only the listing itself must succeed.
                            |_| 0,
                        );
                    }
                }
                Ok(windows)
            })();
            let _ = result_tx.send((worker, outcome));
        });
    }
    drop(result_tx);

    for _ in 0..WORKERS {
        let (worker, outcome) = result_rx.recv_timeout(DEADLINE).unwrap_or_else(|_| {
            panic!(
                "a worker made no progress within {DEADLINE:?}: a UIA object created on a \
                 non-pumping STA thread deadlocks when called from another thread (#499)"
            )
        });
        outcome.unwrap_or_else(|e| {
            panic!("worker {worker}: reading UIA through an STA-created provider failed: {e}")
        });
    }

    let _ = done_tx.send(());
    host.join().expect("host thread exits");
}
