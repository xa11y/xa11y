//! UIPI from a medium-integrity sender to an elevated window (issue #503).
//!
//! CI runners are elevated, so the AccessKit test app the integ script starts
//! runs at high integrity. This test re-launches its own binary as a
//! UAC-style filtered process: a restricted copy of its token with the
//! Administrators group made deny-only (`LUA_TOKEN`) and its integrity label
//! lowered to medium, which is what a non-elevated program of the same user
//! runs with. That child aims `pointer_move` at the test app's window and
//! writes down what the input backend did. The parent asserts the child was
//! refused with `PermissionDenied`, which only happens when a medium-integrity
//! process can read the elevated target's integrity level.
//!
//! `#[ignore]`d: it needs the elevated AccessKit test app on a live desktop,
//! so `scripts/run_integ_tests_windows.ps1` runs it with `--ignored`.

#![cfg(target_os = "windows")]

use std::time::Duration;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, POINT};
use windows::Win32::Security::{
    CreateRestrictedToken, CreateWellKnownSid, GetTokenInformation, SetTokenInformation,
    TokenElevation, TokenIntegrityLevel, WinBuiltinAdministratorsSid, WinMediumLabelSid, LUA_TOKEN,
    PSID, SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY,
    TOKEN_DUPLICATE, TOKEN_ELEVATION, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
    WaitForSingleObject, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetAncestor, GetWindowRect, GetWindowThreadProcessId, WindowFromPoint, GA_ROOT,
};
use xa11y_core::input::{InputProvider, Point};
use xa11y_core::Error;
use xa11y_windows::WindowsInputProvider;

const TEST_NAME: &str = "a_medium_integrity_sender_is_refused_input_to_an_elevated_window";
const CHILD_ENV: &str = "XA11Y_UIPI_CHILD_RESULT_FILE";
const TARGET_TITLE: &str = "xa11y Test App";
/// `SE_GROUP_INTEGRITY` (winnt.h), spelled out to avoid the SystemServices bindings.
const SE_GROUP_INTEGRITY: u32 = 0x20;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn target_window() -> HWND {
    let title = wide(TARGET_TITLE);
    unsafe { FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) }.unwrap_or_else(|e| {
        panic!(
            "the AccessKit test app window {TARGET_TITLE:?} is not open ({e}); run this \
             through scripts/run_integ_tests_windows.ps1"
        )
    })
}

/// Whether this process can open the target's token for `TOKEN_QUERY`, the
/// read the UIPI check makes: `"OK"`, or the failing call and its HRESULT.
fn token_readable(pid: u32) -> String {
    let process = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(p) => p,
        Err(e) => return format!("OpenProcess failed: {:#010x} {e}", e.code().0),
    };
    let mut token = HANDLE::default();
    let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
    // Diagnostic-only handles; a failed close changes nothing we report.
    let _ = unsafe { CloseHandle(process) };
    match opened {
        Ok(()) => {
            let _ = unsafe { CloseHandle(token) };
            "OK".to_string()
        }
        Err(e) => format!("OpenProcessToken failed: {:#010x} {e}", e.code().0),
    }
}

/// Child half: running at medium integrity, send input at the elevated
/// window and record the outcome, plus the two facts that explain a "sent":
/// whether the target's token was readable from here, and whether the
/// point was over the target window at all.
fn child(result_file: &str) {
    let hwnd = target_window();
    let mut rect = Default::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.expect("GetWindowRect of the target");
    let centre = Point::new((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);

    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let token = token_readable(pid);
    let under = unsafe {
        WindowFromPoint(POINT {
            x: centre.x,
            y: centre.y,
        })
    };
    let root = unsafe { GetAncestor(under, GA_ROOT) };
    let hit = format!(
        "{} (WindowFromPoint {under:?}, root {root:?}, target {hwnd:?})",
        if root == hwnd { "inside" } else { "outside" }
    );

    let outcome = match WindowsInputProvider::new()
        .expect("input backend")
        .pointer_move(centre)
    {
        Err(Error::PermissionDenied { instructions }) => format!("refused: {instructions}"),
        Ok(()) => "sent".to_string(),
        Err(e) => format!("error: {e}"),
    };
    std::fs::write(
        result_file,
        format!("{outcome}\ntarget token (pid {pid}): {token}\ncentre is {hit}\n"),
    )
    .expect("write the child result");
}

fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.expect("own token");
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    let _ = unsafe { CloseHandle(token) };
    read.expect("TokenElevation");
    elevation.TokenIsElevated != 0
}

/// A UAC-style filtered, medium-integrity primary token from our own.
fn medium_token() -> HANDLE {
    let mut own = HANDLE::default();
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY,
            &mut own,
        )
    }
    .expect("own token for duplication");

    // Administrators deny-only, as in the filtered half of a UAC split token.
    let mut admins = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut admins_len = admins.len() as u32;
    unsafe {
        CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            None,
            Some(PSID(admins.as_mut_ptr().cast())),
            &mut admins_len,
        )
    }
    .expect("Administrators SID");
    let disable = [SID_AND_ATTRIBUTES {
        Sid: PSID(admins.as_mut_ptr().cast()),
        Attributes: 0,
    }];
    let mut restricted = HANDLE::default();
    unsafe { CreateRestrictedToken(own, LUA_TOKEN, Some(&disable), None, None, &mut restricted) }
        .expect("CreateRestrictedToken");
    let _ = unsafe { CloseHandle(own) };

    let mut medium = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut medium_len = medium.len() as u32;
    unsafe {
        CreateWellKnownSid(
            WinMediumLabelSid,
            None,
            Some(PSID(medium.as_mut_ptr().cast())),
            &mut medium_len,
        )
    }
    .expect("medium label SID");
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: PSID(medium.as_mut_ptr().cast()),
            Attributes: SE_GROUP_INTEGRITY,
        },
    };
    unsafe {
        SetTokenInformation(
            restricted,
            TokenIntegrityLevel,
            (&raw const label).cast(),
            (std::mem::size_of::<TOKEN_MANDATORY_LABEL>() + medium_len as usize) as u32,
        )
    }
    .expect("lower the token to medium integrity");
    restricted
}

#[test]
#[ignore = "needs the elevated AccessKit test app; run via scripts/run_integ_tests_windows.ps1"]
fn a_medium_integrity_sender_is_refused_input_to_an_elevated_window() {
    if let Ok(result_file) = std::env::var(CHILD_ENV) {
        child(&result_file);
        return;
    }
    assert!(
        is_elevated(),
        "this test needs an elevated runner, so the AccessKit app is high integrity"
    );
    let _ = target_window();

    let result_file = std::env::temp_dir().join(format!("xa11y-uipi-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&result_file);
    std::env::set_var(CHILD_ENV, &result_file);

    let exe = std::env::current_exe().expect("own test binary");
    let mut cmdline = wide(&format!(
        "\"{}\" --ignored --exact {TEST_NAME} --test-threads=1",
        exe.display()
    ));
    let token = medium_token();
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessAsUserW(
            Some(token),
            PCWSTR::null(),
            Some(PWSTR(cmdline.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(0),
            None,
            PCWSTR::null(),
            &startup,
            &mut info,
        )
    }
    .expect("launch the medium-integrity child");
    std::env::remove_var(CHILD_ENV);
    let waited = unsafe { WaitForSingleObject(info.hProcess, 60_000) };
    let mut code = 0u32;
    let _ = unsafe { GetExitCodeProcess(info.hProcess, &mut code) };
    unsafe {
        let _ = CloseHandle(info.hThread);
        let _ = CloseHandle(info.hProcess);
        let _ = CloseHandle(token);
    }
    assert_eq!(waited.0, 0, "the medium-integrity child did not finish");
    std::thread::sleep(Duration::from_millis(100));
    let outcome = std::fs::read_to_string(&result_file).unwrap_or_else(|e| {
        panic!("the medium-integrity child (exit code {code}) wrote no result: {e}")
    });
    let _ = std::fs::remove_file(&result_file);
    // Always printed (the integ script runs this with --nocapture), so a
    // passing run also records whether the token was readable and where the
    // point landed.
    eprintln!("medium-integrity child reported:\n{outcome}");
    let first = outcome.lines().next().unwrap_or("");
    assert!(
        first.starts_with("refused:") && first.contains("high integrity"),
        "a medium-integrity sender aimed at the elevated test app must be refused naming \
         the levels; the child reported: {outcome}"
    );
}
