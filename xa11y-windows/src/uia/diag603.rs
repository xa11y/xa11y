//! TEMPORARY (#603 investigation, not for merge): logs where a freshly
//! frozen window drops out of the desktop listing. Enabled by
//! `XA11Y_DIAG_603`; prints to stderr so a failing test shows it.

use super::*;

pub(super) fn enabled() -> bool {
    std::env::var_os("XA11Y_DIAG_603").is_some()
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { windows::Win32::UI::WindowsAndMessaging::GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..usize::try_from(len).unwrap_or(0).min(buf.len())])
}

pub(super) fn describe(hwnd: HWND) -> String {
    format!(
        "hwnd={:#x} pid={} class={:?} title={:?} hung={}",
        hwnd.0 as usize,
        hwnd_pid(hwnd),
        class_of(hwnd),
        hwnd_title(hwnd),
        window_is_hung(hwnd)
    )
}

fn el_line(el: &IUIAutomationElement) -> String {
    let hwnd = unsafe { el.CachedNativeWindowHandle() }
        .map(|h| format!("{:#x}", h.0 as usize))
        .unwrap_or_else(|e| format!("<err {e}>"));
    let pid = unsafe { el.CachedProcessId() }
        .map(|p| p.to_string())
        .unwrap_or_else(|e| format!("<err {e}>"));
    let ct = unsafe { el.CachedControlType() }
        .map(|c| c.0.to_string())
        .unwrap_or_else(|e| format!("<err {e}>"));
    let name = unsafe { el.CachedName() }
        .map(|n| format!("{:?}", n.to_string()))
        .unwrap_or_else(|e| format!("<err {e}>"));
    let class = unsafe { el.CachedClassName() }
        .map(|n| format!("{:?}", n.to_string()))
        .unwrap_or_else(|e| format!("<err {e}>"));
    format!("hwnd={hwnd} pid={pid} ct={ct} name={name} class={class}")
}

pub(super) fn hwnds_of(arr: &IUIAutomationElementArray) -> Vec<usize> {
    let mut out = Vec::new();
    for i in 0..uia_len(arr) {
        if let Ok(el) = unsafe { arr.GetElement(i) } {
            if let Ok(h) = unsafe { el.CachedNativeWindowHandle() } {
                out.push(h.0 as usize);
            }
        }
    }
    out
}

pub(super) fn log_array(
    label: &str,
    arr: &IUIAutomationElementArray,
    elapsed: std::time::Duration,
) {
    eprintln!(
        "[diag603] {label}: {} element(s) in {elapsed:?}",
        uia_len(arr)
    );
    for i in 0..uia_len(arr) {
        match unsafe { arr.GetElement(i) } {
            Ok(el) => eprintln!("[diag603]   #{i} {}", el_line(&el)),
            Err(e) => eprintln!("[diag603]   #{i} <GetElement err {e}>"),
        }
    }
}

/// Everything after the real listing's FindAll: an unfiltered FindAll, and
/// a direct ElementFromHandle on each interactive window UIA left out.
pub(super) fn after_listing(
    autom: &IUIAutomation,
    root: &IUIAutomationElement,
    cache: &IUIAutomationCacheRequest,
    interactive: &[HWND],
    listed: &[usize],
) {
    let missing: Vec<HWND> = interactive
        .iter()
        .copied()
        .filter(|h| !listed.contains(&(h.0 as usize)))
        .collect();
    eprintln!(
        "[diag603] interactive windows missing from the Window-type FindAll: {}",
        missing.len()
    );
    for h in &missing {
        eprintln!("[diag603]   {}", describe(*h));
    }
    let start = std::time::Instant::now();
    let all = unsafe {
        autom
            .CreateTrueCondition()
            .and_then(|c| root.FindAllBuildCache(TreeScope_Children, &c, cache))
    };
    match all {
        Ok(arr) => {
            log_array("unfiltered FindAll (TrueCondition)", &arr, start.elapsed());
            let got = hwnds_of(&arr);
            for h in &missing {
                eprintln!(
                    "[diag603]   missing {:#x} in unfiltered FindAll: {}",
                    h.0 as usize,
                    got.contains(&(h.0 as usize))
                );
            }
        }
        Err(e) => eprintln!(
            "[diag603] unfiltered FindAll failed after {:?}: {e}",
            start.elapsed()
        ),
    }
    for h in &missing {
        let start = std::time::Instant::now();
        let el = unsafe { autom.ElementFromHandleBuildCache(*h, cache) };
        let took = start.elapsed();
        match el {
            Ok(el) => eprintln!(
                "[diag603]   ElementFromHandleBuildCache({:#x}) ok in {took:?}: {}",
                h.0 as usize,
                el_line(&el)
            ),
            Err(e) => eprintln!(
                "[diag603]   ElementFromHandleBuildCache({:#x}) failed in {took:?}: {e}",
                h.0 as usize
            ),
        }
    }
}
