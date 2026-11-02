//! Regression for issue #500: UIA's first touch in a process, made from
//! several threads at once.
//!
//! Its own test binary with a single test, so this really is the process's
//! first UIA creation and first `GetRootElement`. Inside the unit-test binary
//! another test would usually have warmed UIA already.

#![cfg(target_os = "windows")]

use xa11y_core::Provider;
use xa11y_windows::WindowsProvider;

#[test]
fn concurrent_first_touch_does_not_fail() {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                // Line the threads up so their first touches overlap.
                barrier.wait();
                let provider = WindowsProvider::new()?;
                provider.get_children(None).map(|_| ())
            })
        })
        .collect();
    for (i, t) in threads.into_iter().enumerate() {
        t.join()
            .expect("provider thread panicked")
            .unwrap_or_else(|e| panic!("thread {i}: concurrent first touch failed: {e}"));
    }
}
