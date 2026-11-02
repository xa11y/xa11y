//! User Interface Privilege Isolation (UIPI) pre-flight for `SendInput`.
//!
//! Windows discards synthesized input aimed at a window whose process runs at
//! a higher integrity level than the sender (an elevated app, for a normal
//! caller), and `SendInput` still reports every event as sent. Without a check
//! the input backend would report success for input that never arrived
//! (tenet 1), so [`check_target`] compares the two integrity levels before
//! anything is posted and refuses with [`Error::PermissionDenied`] naming both.

#![cfg(target_os = "windows")]

use std::sync::OnceLock;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel,
    TokenUIAccess, TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, GetWindowThreadProcessId};

use xa11y_core::{Error, Result};

/// The calling process's integrity level and whether it holds `uiAccess`
/// (which exempts it from UIPI). Read once: a process token's integrity level
/// cannot change after the process starts.
#[derive(Clone, Copy)]
struct Sender {
    level: u32,
    ui_access: bool,
}

/// The sender's levels, once read successfully. A failed read is not cached,
/// so a transient failure does not disable every later check.
static SENDER: OnceLock<Sender> = OnceLock::new();

fn sender() -> Result<Sender> {
    if let Some(sender) = SENDER.get() {
        return Ok(*sender);
    }
    // Keep the HRESULT with the message (tenet 6).
    let fail = |e: windows::core::Error| Error::Platform {
        code: i64::from(e.code().0),
        message: format!(
            "reading this process's integrity level for the UIPI check failed: {} \
             (HRESULT {:#010x})",
            e.message(),
            e.code().0
        ),
    };
    let token = ProcessToken::current().map_err(fail)?;
    let sender = Sender {
        level: token.integrity_level().map_err(fail)?,
        ui_access: token.ui_access().map_err(fail)?,
    };
    Ok(*SENDER.get_or_init(|| sender))
}

/// Refuse input that UIPI would silently discard.
///
/// `hwnd` is the window the input will reach: the foreground window for
/// keyboard input, the window under the pointer for mouse input. A null HWND
/// (no foreground window, nothing under the point) has no process to compare
/// against, so there is nothing for UIPI to block.
///
/// Input is refused only when the target is *positively known* to run at a
/// higher integrity level. `tests/uipi_medium.rs` checks that a filtered,
/// medium-integrity process can read an elevated same-user process's level
/// and is refused. When the target's level cannot be read (its token refuses
/// `TOKEN_QUERY`, as another user's process can) the input is sent as
/// before. Refusing there would break
/// legitimate same-integrity targets (`runas`, service accounts), so the
/// remaining gap is explicit: for an unreadable target, UIPI may still drop
/// the input silently. A target that holds `uiAccess` is not checked either:
/// UIPI may protect it from a same-integrity sender without `uiAccess`, and
/// that input may also be dropped silently.
pub fn check_target(hwnd: HWND, input: &str) -> Result<()> {
    if hwnd.0.is_null() {
        return Ok(());
    }
    let sender = sender()?;
    if sender.ui_access {
        return Ok(());
    }

    let mut pid = 0u32;
    // SAFETY: `pid` outlives the call; a stale HWND yields pid 0.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    // pid 0: the window closed between finding it and this read, so there is
    // no process left for UIPI to protect. Our own process is never above us.
    // SAFETY: no arguments.
    if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
        return Ok(());
    }
    // TENET-BREAK(1): target integrity level unreadable (e.g. another user's process) → send as before; maintainer-approved (#503): refusing would break legitimate same-integrity targets (runas, service accounts); UIPI may still drop the input silently.
    let Ok(target) = ProcessToken::of_pid(pid).and_then(|t| t.integrity_level()) else {
        // See the function docs for the gap this leaves.
        return Ok(());
    };
    if blocked_by_uipi(sender.level, sender.ui_access, target) {
        return Err(Error::PermissionDenied {
            instructions: format!(
                "Windows UIPI discards {input} sent from this process ({} integrity) to \
                 HWND {:#x} \"{}\" (pid {pid}, {} integrity), and SendInput would still \
                 report success. Run the automation at the target's integrity level or \
                 higher (for an elevated app, run it elevated), or target a non-elevated \
                 window.",
                integrity_name(sender.level),
                hwnd.0 as usize,
                window_text(hwnd),
                integrity_name(target),
            ),
        });
    }
    Ok(())
}

/// UIPI drops input sent to a higher integrity level, unless the sender
/// holds `uiAccess`.
pub(crate) fn blocked_by_uipi(sender: u32, ui_access: bool, target: u32) -> bool {
    !ui_access && target > sender
}

/// The `SECURITY_MANDATORY_*_RID` name of an integrity level.
pub(crate) fn integrity_name(level: u32) -> String {
    let name = match level {
        0x0000 => "untrusted",
        0x1000 => "low",
        0x2000 => "medium",
        0x2100 => "medium-plus",
        0x3000 => "high",
        0x4000 => "system",
        0x5000 => "protected",
        _ => return format!("level {level:#x}"),
    };
    name.to_string()
}

fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: `buf` is a valid, writable buffer for the call's duration.
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    // 0 is both "untitled" and "failed"; the HWND printed alongside
    // identifies the window either way.
    String::from_utf16_lossy(&buf[..usize::try_from(len).unwrap_or(0).min(buf.len())])
}

/// An open process token, closed on drop.
struct ProcessToken(HANDLE);

impl ProcessToken {
    fn current() -> windows::core::Result<Self> {
        // SAFETY: the pseudo-handle from GetCurrentProcess needs no closing.
        Self::open(unsafe { GetCurrentProcess() })
    }

    fn of_pid(pid: u32) -> windows::core::Result<Self> {
        // SAFETY: plain Win32 call; the handle is closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }?;
        let token = Self::open(process);
        // SAFETY: `process` is a real handle we own. Its close result does not
        // affect the token (which holds its own reference), so it is ignored.
        let _ = unsafe { CloseHandle(process) };
        token
    }

    fn open(process: HANDLE) -> windows::core::Result<Self> {
        let mut token = HANDLE::default();
        // SAFETY: `token` outlives the call.
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }?;
        Ok(Self(token))
    }

    fn info(&self, class: TOKEN_INFORMATION_CLASS) -> windows::core::Result<Vec<u64>> {
        let mut len = 0u32;
        // SAFETY: a size query with no buffer; it fails with
        // ERROR_INSUFFICIENT_BUFFER and fills `len`, which is all we need.
        let _ = unsafe { GetTokenInformation(self.0, class, None, 0, &mut len) };
        // u64 storage keeps the buffer aligned for the structs read out of it.
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        // SAFETY: `buf` holds at least `len` bytes.
        unsafe {
            GetTokenInformation(
                self.0,
                class,
                Some(buf.as_mut_ptr().cast()),
                (buf.len() * 8) as u32,
                &mut len,
            )
        }?;
        Ok(buf)
    }

    fn integrity_level(&self) -> windows::core::Result<u32> {
        let buf = self.info(TokenIntegrityLevel)?;
        // SAFETY: GetTokenInformation(TokenIntegrityLevel) wrote a
        // TOKEN_MANDATORY_LABEL at the start of the aligned buffer, and its
        // SID points into the same buffer, which is alive for these reads.
        // An integrity SID always has exactly one sub-authority, the RID.
        unsafe {
            let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
            let sid = label.Label.Sid;
            let count = *GetSidSubAuthorityCount(sid);
            Ok(*GetSidSubAuthority(sid, u32::from(count.saturating_sub(1))))
        }
    }

    fn ui_access(&self) -> windows::core::Result<bool> {
        let buf = self.info(TokenUIAccess)?;
        // TokenUIAccess is a DWORD flag.
        Ok(buf[0] as u32 != 0)
    }
}

impl Drop for ProcessToken {
    fn drop(&mut self) {
        // SAFETY: we own the token handle. Nothing to do if closing fails.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_target_is_blocked_unless_ui_access() {
        assert!(blocked_by_uipi(0x2000, false, 0x3000));
        assert!(!blocked_by_uipi(0x2000, true, 0x3000));
    }

    #[test]
    fn same_or_lower_target_is_not_blocked() {
        assert!(!blocked_by_uipi(0x2000, false, 0x2000));
        assert!(!blocked_by_uipi(0x3000, false, 0x2000));
        assert!(!blocked_by_uipi(0x2000, false, 0x1000));
    }

    #[test]
    fn integrity_levels_are_named() {
        assert_eq!(integrity_name(0x2000), "medium");
        assert_eq!(integrity_name(0x3000), "high");
        assert_eq!(integrity_name(0x2100), "medium-plus");
        assert_eq!(integrity_name(0x1234), "level 0x1234");
    }

    #[test]
    fn this_process_has_a_readable_integrity_level() {
        let token = ProcessToken::current().expect("own token opens");
        let level = token.integrity_level().expect("own integrity level reads");
        assert!(level <= 0x5000, "{level:#x}");
        token.ui_access().expect("own uiAccess flag reads");
    }

    #[test]
    fn a_null_target_window_passes() {
        assert!(check_target(HWND::default(), "keyboard input").is_ok());
    }
}
