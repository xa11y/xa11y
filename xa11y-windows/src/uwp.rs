//! Which app a UWP / packaged app's frame window belongs to (issue #497).
//!
//! Every UWP app is drawn inside an `ApplicationFrameWindow` owned by
//! `ApplicationFrameHost.exe`, so the frame's Win32 owner is the frame host,
//! never the app. Taking that pid merged every UWP app into one
//! "ApplicationFrameHost" app, and `App::by_name("Calculator")` found nothing.
//!
//! The frame is attributed to its app in one of two ways:
//!
//! 1. **Hosted core window.** A running app's frame hosts a
//!    `Windows.UI.Core.CoreWindow` child that the app's own process owns.
//! 2. **AppUserModelID.** A minimized or suspended app's frame hosts no core
//!    window: Windows detaches it. The frame still carries the app's
//!    AppUserModelID (`PKEY_AppUserModel_ID` in its window property store),
//!    and a suspended process still exists and still reports that AUMID
//!    through `GetApplicationUserModelId`, so the frame is matched to the
//!    running process with the same AUMID.
//!
//! A frame neither way resolves (the app has exited) belongs to no app: it is
//! left out of app listings and reported through `Provider::unlisted_apps` so
//! an app lookup that missed it says why.
//!
//! Two entry points, with different costs:
//! - [`Attribution`] answers "whose is this window?" for a desktop-wide
//!   listing. It builds the AUMID-to-process index once, on the first
//!   detached frame it meets.
//! - [`PidFilter`] answers "is this window this pid's?" for one process's
//!   window enumeration, which every app query runs. A non-frame is decided by
//!   its native pid alone; frames are only examined when the target is a
//!   packaged process, and the full index is built only to break a tie
//!   between processes sharing an AUMID.

#![cfg(target_os = "windows")]

use std::collections::{HashMap, HashSet};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, APPMODEL_ERROR_NO_APPLICATION, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HWND,
    LPARAM, PROPERTYKEY,
};
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::ProcessStatus::EnumProcesses;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::System::Variant::VT_EMPTY;
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId,
};

use xa11y_core::{Error, Result};

/// Win32 class of the frame `ApplicationFrameHost.exe` draws around every
/// UWP / packaged app window.
pub const FRAME_CLASS: &str = "ApplicationFrameWindow";
/// Win32 class of the app-owned window a UWP frame hosts.
pub const CORE_WINDOW_CLASS: &str = "Windows.UI.Core.CoreWindow";
const CORE_WINDOW_CLASS_W: PCWSTR = windows::core::w!("Windows.UI.Core.CoreWindow");

/// `PKEY_AppUserModel_ID`, spelled out to avoid pulling in the
/// EnhancedStorage bindings for one constant.
const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
    pid: 5,
};

/// Whom a top-level window belongs to, as an app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowOwner {
    /// The window belongs to this process.
    Process(u32),
    /// A UWP frame whose app could not be found; `note` says why, for a
    /// diagnosis.
    Unresolved { note: String },
}

/// Whether `hwnd` is a UWP frame window.
pub fn is_frame(hwnd: HWND) -> bool {
    window_class(hwnd) == FRAME_CLASS
}

/// Attributes windows to apps for a desktop-wide listing.
#[derive(Default)]
pub struct Attribution {
    index: Option<AumidIndex>,
}

impl Attribution {
    pub fn new() -> Self {
        Self::default()
    }

    /// The app that owns top-level window `hwnd`.
    pub fn owner(&mut self, hwnd: HWND) -> Result<WindowOwner> {
        if !is_frame(hwnd) {
            return Ok(WindowOwner::Process(native_pid(hwnd)));
        }
        if let Some(pid) = hosted_core_window_pid(hwnd)? {
            return Ok(WindowOwner::Process(pid));
        }
        let title = window_title(hwnd);
        let frame = format!("UWP frame HWND {:#x} \"{title}\"", hwnd.0 as usize);
        let Some(aumid) = frame_aumid(hwnd)? else {
            return Ok(WindowOwner::Unresolved {
                note: format!("{frame} hosts no app window and carries no AppUserModelID"),
            });
        };
        let index = match self.index.take() {
            Some(index) => index,
            None => AumidIndex::build()?,
        };
        let index = &*self.index.insert(index);
        Ok(match pick_process(&aumid, index) {
            Some(pid) => WindowOwner::Process(pid),
            None => WindowOwner::Unresolved {
                note: unresolved_note(&frame, &aumid, index),
            },
        })
    }
}

/// Decides which top-level windows belong to one process.
pub struct PidFilter {
    pid: u32,
    /// The target's packaging, read once on first need. Only a packaged
    /// process can own a detached frame.
    packaging: Option<Packaging>,
    core_window_owners: Option<HashSet<u32>>,
    index: Option<AumidIndex>,
}

impl PidFilter {
    pub fn for_pid(pid: u32) -> Self {
        Self {
            pid,
            packaging: None,
            core_window_owners: None,
            index: None,
        }
    }

    /// Whether the target process is a packaged app, and its
    /// AppUserModelID if so. Read once: an enumeration that meets no detached
    /// frame never asks.
    pub fn packaging(&mut self) -> Result<&Packaging> {
        let packaging = match self.packaging.take() {
            Some(packaging) => packaging,
            None => process_packaging(self.pid)?,
        };
        Ok(self.packaging.insert(packaging))
    }

    /// Whether `hwnd` is one of the target process's windows.
    pub fn owns(&mut self, hwnd: HWND) -> Result<bool> {
        if !is_frame(hwnd) {
            return Ok(native_pid(hwnd) == self.pid);
        }
        if let Some(core) = hosted_core_window_pid(hwnd)? {
            return Ok(core == self.pid);
        }
        let pid = self.pid;
        // An unpackaged process owns no detached frame, and one that has
        // exited owns nothing.
        let Packaging::Packaged(aumid) = self.packaging()? else {
            return Ok(false);
        };
        let aumid = aumid.clone();
        if frame_aumid(hwnd)?.as_deref() != Some(aumid.as_str()) {
            return Ok(false);
        }
        // The frame is this AUMID's. If our process owns a detached core
        // window it is the app's UI process; otherwise ask the index whether
        // it is the only candidate.
        if self.core_window_owners.is_none() {
            self.core_window_owners = Some(top_level_core_window_owners()?);
        }
        if self
            .core_window_owners
            .as_ref()
            .is_some_and(|o| o.contains(&pid))
        {
            return Ok(true);
        }
        if self.index.is_none() {
            self.index = Some(AumidIndex::build()?);
        }
        Ok(self
            .index
            .as_ref()
            .is_some_and(|index| pick_process(&aumid, index) == Some(pid)))
    }
}

/// Running processes by AppUserModelID, plus which processes own a top-level
/// core window (a suspended or minimized UWP app's detached one).
#[derive(Default, Debug)]
pub struct AumidIndex {
    pub by_aumid: HashMap<String, Vec<u32>>,
    pub core_window_owners: HashSet<u32>,
    /// Processes whose AUMID could not be read, so a "no process has it"
    /// answer is not certain.
    pub uninspectable: usize,
}

impl AumidIndex {
    fn build() -> Result<Self> {
        let mut index = Self::default();
        for pid in process_ids()? {
            match process_packaging(pid) {
                Ok(Packaging::Packaged(aumid)) => {
                    index.by_aumid.entry(aumid).or_default().push(pid)
                }
                Ok(Packaging::Unpackaged | Packaging::Exited) => {}
                Err(_) => index.uninspectable += 1,
            }
        }
        index.core_window_owners = top_level_core_window_owners()?;
        Ok(index)
    }
}

fn unresolved_note(frame: &str, aumid: &str, index: &AumidIndex) -> String {
    match (index.by_aumid.get(aumid).map(Vec::len), index.uninspectable) {
        (Some(n), _) if n > 1 => format!(
            "{frame} (AppUserModelID {aumid}): {n} processes share that AppUserModelID and \
             none of them alone owns a core window, so its app is ambiguous"
        ),
        (_, 0) => format!(
            "{frame} (AppUserModelID {aumid}): no running process has that AppUserModelID, so \
             the app has exited"
        ),
        (_, n) => format!(
            "{frame} (AppUserModelID {aumid}): no inspectable process has that \
             AppUserModelID; {n} processes could not be inspected"
        ),
    }
}

/// The process a frame with AppUserModelID `aumid` belongs to.
///
/// One process with that AUMID is the app. Several (an app plus its
/// background task host, which share an AUMID) are narrowed to the ones that
/// own a core window, since only the app's UI process has one; if that does
/// not leave exactly one, the frame is not guessed at.
pub fn pick_process(aumid: &str, index: &AumidIndex) -> Option<u32> {
    let candidates = index.by_aumid.get(aumid)?;
    match candidates.as_slice() {
        [only] => Some(*only),
        _ => {
            let with_ui: Vec<u32> = candidates
                .iter()
                .copied()
                .filter(|p| index.core_window_owners.contains(p))
                .collect();
            match with_ui.as_slice() {
                [only] => Some(*only),
                _ => None,
            }
        }
    }
}

fn native_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: `pid` outlives the call; a stale HWND yields 0.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// The pid owning the core window a UWP frame hosts, or `None` when it hosts
/// none.
fn hosted_core_window_pid(frame: HWND) -> Result<Option<u32>> {
    // FindWindowExW reports "no such child" as null with the last error
    // untouched, so clear it first: a stale code from an earlier call must
    // not read as this call's failure.
    // SAFETY: sets this thread's last-error value only.
    unsafe { windows::Win32::Foundation::SetLastError(windows::Win32::Foundation::WIN32_ERROR(0)) };
    let core =
        match unsafe { FindWindowExW(Some(frame), None, CORE_WINDOW_CLASS_W, PCWSTR::null()) } {
            Ok(core) if !core.0.is_null() => core,
            Ok(_) => return Ok(None),
            // "No such child" is a null result with no error code set.
            Err(e) if e.code().is_ok() => return Ok(None),
            Err(e) => {
                return Err(Error::Platform {
                    code: i64::from(e.code().0),
                    message: format!(
                        "FindWindowExW(CoreWindow) under UWP frame HWND {:#x} failed: {e}",
                        frame.0 as usize
                    ),
                })
            }
        };
    let pid = native_pid(core);
    Ok((pid != 0).then_some(pid))
}

/// The frame window's AppUserModelID from its property store.
fn frame_aumid(frame: HWND) -> Result<Option<String>> {
    let fail = |what: &str, e: windows::core::Error| Error::Platform {
        code: i64::from(e.code().0),
        message: format!(
            "{what} for UWP frame HWND {:#x} failed: {e}",
            frame.0 as usize
        ),
    };
    // SAFETY: plain shell call on a window handle.
    let store: IPropertyStore = unsafe { SHGetPropertyStoreForWindow(frame) }
        .map_err(|e| fail("SHGetPropertyStoreForWindow", e))?;
    // SAFETY: the key is a valid PROPERTYKEY for the call's duration.
    let mut value = unsafe { store.GetValue(&PKEY_APP_USER_MODEL_ID) }
        .map_err(|e| fail("IPropertyStore.GetValue(PKEY_AppUserModel_ID)", e))?;
    // SAFETY: reading the discriminant of the PROPVARIANT GetValue returned.
    let empty = unsafe { value.Anonymous.Anonymous.vt } == VT_EMPTY;
    let text = if empty {
        None
    } else {
        // SAFETY: `value` is the PROPVARIANT GetValue returned.
        Some(unsafe { PropVariantToStringAlloc(&value) })
    };
    // SAFETY: clears the PROPVARIANT we own. A failed clear leaks one
    // variant and changes nothing about the value already read.
    let _ = unsafe { PropVariantClear(&mut value) };
    let Some(text) = text else {
        return Ok(None);
    };
    let text = text.map_err(|e| fail("converting PKEY_AppUserModel_ID to a string", e))?;
    // SAFETY: PropVariantToStringAlloc returned a NUL-terminated CoTaskMem
    // string we own; it is freed right after being copied.
    let aumid = String::from_utf16_lossy(unsafe { text.as_wide() });
    unsafe { CoTaskMemFree(Some(text.0 as *const _)) };
    Ok((!aumid.is_empty()).then_some(aumid))
}

/// Whether a process is a packaged app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packaging {
    /// A packaged app, with its AppUserModelID.
    Packaged(String),
    /// An ordinary Win32 process.
    Unpackaged,
    /// No process has this pid any more.
    Exited,
}

/// Whether process `pid` is a packaged app. A pid that names no process
/// (`OpenProcess` fails with `ERROR_INVALID_PARAMETER`) is
/// [`Packaging::Exited`], not an error, so a lookup of an app that just quit
/// says "not found" rather than "platform error".
pub fn process_packaging(pid: u32) -> Result<Packaging> {
    use windows::Win32::Foundation::ERROR_INVALID_PARAMETER;
    // SAFETY: plain Win32 call; the handle is closed below.
    let process = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(process) => process,
        Err(e) if e.code() == ERROR_INVALID_PARAMETER.to_hresult() => return Ok(Packaging::Exited),
        Err(e) => {
            return Err(Error::Platform {
                code: i64::from(e.code().0),
                message: format!("OpenProcess(pid {pid}) to read its AppUserModelID failed: {e}"),
            })
        }
    };
    let result = handle_aumid(process, pid);
    // SAFETY: we own the handle; a failed close leaks one handle and changes
    // nothing about the AUMID already read.
    let _ = unsafe { CloseHandle(process) };
    Ok(match result? {
        Some(aumid) => Packaging::Packaged(aumid),
        None => Packaging::Unpackaged,
    })
}

/// The display name the shell shows for the packaged app `aumid` (Start
/// menu, taskbar), e.g. "Calculator": the `shell:AppsFolder` item for the
/// AppUserModelID, which resolves the manifest's `DisplayName` resource.
///
/// `Ok(None)` when the AUMID has no Apps-folder entry (an app that opts out
/// of the app list, `AppListEntry="none"`): the caller names it by its
/// executable and records that it did.
pub fn app_display_name(aumid: &str) -> Result<Option<String>> {
    /// Successful lookups, by AUMID. Every `list_apps` names every packaged
    /// process, and the name changes only when the package is updated, so a
    /// small cache saves a shell round trip per app per listing. Bounded by
    /// clearing when full; errors are never cached.
    static CACHE: std::sync::Mutex<Option<HashMap<String, Option<String>>>> =
        std::sync::Mutex::new(None);
    const CACHE_LIMIT: usize = 256;

    if let Some(hit) = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|c| c.get(aumid))
    {
        return Ok(hit.clone());
    }
    let name = lookup_app_display_name(aumid)?;
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(aumid.to_owned(), name.clone());
    Ok(name)
}

/// [`app_display_name`] without the cache.
fn lookup_app_display_name(aumid: &str) -> Result<Option<String>> {
    use windows::Win32::UI::Shell::{
        FOLDERID_AppsFolder, IShellItem, SHCreateItemInKnownFolder, KF_FLAG_DEFAULT,
        SIGDN_NORMALDISPLAY,
    };
    let fail = |what: &str, e: windows::core::Error| Error::Platform {
        code: i64::from(e.code().0),
        message: format!("{what} for AppUserModelID {aumid} failed: {e}"),
    };
    let wide: Vec<u16> = aumid.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let item: IShellItem = match unsafe {
        SHCreateItemInKnownFolder(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, PCWSTR(wide.as_ptr()))
    } {
        Ok(item) => item,
        Err(e) if apps_folder_entry_absent(e.code()) => return Ok(None),
        Err(e) => return Err(fail("SHCreateItemInKnownFolder(FOLDERID_AppsFolder)", e)),
    };
    // SAFETY: plain COM call on a live shell item.
    let name = unsafe { item.GetDisplayName(SIGDN_NORMALDISPLAY) }
        .map_err(|e| fail("IShellItem.GetDisplayName", e))?;
    // SAFETY: GetDisplayName returned a NUL-terminated CoTaskMem string we
    // own; it is freed right after being copied.
    let text = String::from_utf16_lossy(unsafe { name.as_wide() });
    unsafe { CoTaskMemFree(Some(name.0 as *const _)) };
    Ok((!text.is_empty()).then_some(text))
}

fn apps_folder_entry_absent(code: windows::core::HRESULT) -> bool {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NOT_FOUND, ERROR_PATH_NOT_FOUND};
    // Windows 11 returns ERROR_NOT_FOUND for shell-host AUMIDs with no
    // Apps-folder item (observed for Client.CBS's runtime broker). This is
    // absence, like FILE/PATH_NOT_FOUND; unrelated shell failures propagate.
    [ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_NOT_FOUND]
        .iter()
        .any(|error| code == error.to_hresult())
}

/// The AppUserModelID of the process `process` (pid `pid`, for messages)
/// opened with at least `PROCESS_QUERY_LIMITED_INFORMATION`: `Ok(None)` for
/// a process that is not a packaged app.
pub fn handle_aumid(
    process: windows::Win32::Foundation::HANDLE,
    pid: u32,
) -> Result<Option<String>> {
    let mut buf = vec![0u16; 256];
    loop {
        let mut len = buf.len() as u32;
        // SAFETY: `buf` holds `len` UTF-16 units.
        let status = unsafe {
            GetApplicationUserModelId(
                process,
                &mut len,
                Some(windows::core::PWSTR(buf.as_mut_ptr())),
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER && len as usize > buf.len() {
            buf.resize(len as usize, 0);
            continue;
        }
        break match status {
            // `len` counts the terminating NUL.
            ERROR_SUCCESS => Ok(Some(String::from_utf16_lossy(
                &buf[..(len as usize).saturating_sub(1).min(buf.len())],
            ))),
            APPMODEL_ERROR_NO_APPLICATION => Ok(None),
            other => Err(Error::Platform {
                code: i64::from(other.0),
                message: format!(
                    "GetApplicationUserModelId(pid {pid}) failed with Win32 error {}",
                    other.0
                ),
            }),
        };
    }
}

/// Every process id on the system.
fn process_ids() -> Result<Vec<u32>> {
    let mut pids = vec![0u32; 4096];
    loop {
        let mut needed = 0u32;
        let bytes = (pids.len() * 4) as u32;
        // SAFETY: `pids` holds `bytes` bytes.
        unsafe { EnumProcesses(pids.as_mut_ptr(), bytes, &mut needed) }.map_err(|e| {
            Error::Platform {
                code: i64::from(e.code().0),
                message: format!("EnumProcesses failed while finding UWP apps: {e}"),
            }
        })?;
        if needed < bytes {
            pids.truncate(needed as usize / 4);
            return Ok(pids);
        }
        pids.resize(pids.len() * 2, 0);
    }
}

/// The pids owning a top-level `Windows.UI.Core.CoreWindow`.
fn top_level_core_window_owners() -> Result<HashSet<u32>> {
    unsafe extern "system" fn collect(hwnd: HWND, param: LPARAM) -> windows::core::BOOL {
        let owners = unsafe { &mut *(param.0 as *mut HashSet<u32>) };
        if window_class(hwnd) == CORE_WINDOW_CLASS {
            owners.insert(native_pid(hwnd));
        }
        true.into()
    }
    let mut owners = HashSet::new();
    // SAFETY: the callback receives the pointer unchanged and `owners`
    // outlives the call.
    unsafe { EnumWindows(Some(collect), LPARAM((&raw mut owners) as isize)) }.map_err(|e| {
        Error::Platform {
            code: i64::from(e.code().0),
            message: format!("EnumWindows failed while finding core windows: {e}"),
        }
    })?;
    Ok(owners)
}

/// A window's Win32 class name; empty when it cannot be read.
pub fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: `buf` is valid and writable for the call.
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..usize::try_from(len).unwrap_or(0).min(buf.len())])
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: `buf` is valid and writable; GetWindowTextW does not message
    // another process's window.
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..usize::try_from(len).unwrap_or(0).min(buf.len())])
}

#[cfg(test)]
mod tests {
    #[test]
    fn apps_folder_absence_preserves_other_shell_failures() {
        use windows::Win32::Foundation::{
            ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_NOT_FOUND, ERROR_PATH_NOT_FOUND,
        };
        for error in [ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_NOT_FOUND] {
            assert!(super::apps_folder_entry_absent(error.to_hresult()));
        }
        assert!(!super::apps_folder_entry_absent(
            ERROR_ACCESS_DENIED.to_hresult()
        ));
        assert!(!super::apps_folder_entry_absent(windows::core::HRESULT(
            0x80004005u32 as i32
        )));
    }

    use super::*;

    /// Diagnostic for a real desktop, not a check: prints, for every
    /// packaged process that owns a top-level window, its AUMID, what the
    /// display-name lookup returned (the name, "no Apps-folder entry", or the
    /// error with its HRESULT), and which name source the Application node
    /// gets. Run it on Windows 11 to confirm how shell hosts with
    /// `AppListEntry="none"` (TextInputHost, ShellExperienceHost,
    /// StartMenuExperienceHost, SearchHost, LockApp) answer:
    ///
    /// ```text
    /// cargo test -p xa11y-windows --lib -- --ignored --nocapture packaged_app_name_report
    /// ```
    #[test]
    #[ignore = "diagnostic: prints the packaged apps on this desktop"]
    fn packaged_app_name_report() {
        // Same apartment setup as the provider.
        unsafe { windows::Win32::System::Com::CoIncrementMTAUsage() }.expect("MTA usage");
        unsafe extern "system" fn collect(hwnd: HWND, param: LPARAM) -> windows::core::BOOL {
            let pids = unsafe { &mut *(param.0 as *mut std::collections::BTreeSet<u32>) };
            pids.insert(native_pid(hwnd));
            // A running UWP app's own window is the CoreWindow inside its
            // frame, not a top-level one.
            if is_frame(hwnd) {
                if let Ok(Some(pid)) = hosted_core_window_pid(hwnd) {
                    pids.insert(pid);
                }
            }
            true.into()
        }
        let mut pids = std::collections::BTreeSet::new();
        unsafe { EnumWindows(Some(collect), LPARAM((&raw mut pids) as isize)) }
            .expect("EnumWindows");
        let mut packaged = 0;
        for pid in pids {
            let aumid = match process_packaging(pid) {
                Ok(Packaging::Packaged(aumid)) => aumid,
                Ok(Packaging::Unpackaged | Packaging::Exited) => continue,
                Err(e) => {
                    println!("pid {pid:>6}  packaging unreadable: {e}");
                    continue;
                }
            };
            packaged += 1;
            let (lookup, source) = match lookup_app_display_name(&aumid) {
                Ok(Some(name)) => (format!("{name:?}"), "package"),
                Ok(None) => ("no Apps-folder entry".to_string(), "process"),
                Err(Error::Platform { code, message }) => (
                    format!("ERROR {:#010x}: {message}", code as u32),
                    "error (list_apps would fail)",
                ),
                Err(e) => (format!("ERROR {e}"), "error (list_apps would fail)"),
            };
            println!("pid {pid:>6}  {aumid}\n            display name: {lookup}\n            name source: {source}");
        }
        println!("{packaged} packaged process(es) with a top-level window");
    }

    fn index(entries: &[(&str, &[u32])], core: &[u32], uninspectable: usize) -> AumidIndex {
        AumidIndex {
            by_aumid: entries
                .iter()
                .map(|(a, pids)| (a.to_string(), pids.to_vec()))
                .collect(),
            core_window_owners: core.iter().copied().collect(),
            uninspectable,
        }
    }

    const CALC: &str = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";

    #[test]
    fn a_single_process_with_the_aumid_is_the_app() {
        let idx = index(&[(CALC, &[4242])], &[], 0);
        assert_eq!(pick_process(CALC, &idx), Some(4242));
    }

    #[test]
    fn no_process_with_the_aumid_means_the_app_exited() {
        let idx = index(&[("Other!App", &[7])], &[7], 0);
        assert_eq!(pick_process(CALC, &idx), None);
        assert!(unresolved_note("frame", CALC, &idx).contains("the app has exited"));
    }

    #[test]
    fn uninspectable_processes_are_not_called_exited() {
        let idx = index(&[], &[], 3);
        let note = unresolved_note("frame", CALC, &idx);
        assert!(
            note.contains("3 processes could not be inspected"),
            "{note}"
        );
        assert!(!note.contains("exited"), "{note}");
    }

    #[test]
    fn several_processes_narrow_to_the_one_with_a_core_window() {
        // The app plus its background task host share the AUMID.
        let idx = index(&[(CALC, &[100, 200])], &[200], 0);
        assert_eq!(pick_process(CALC, &idx), Some(200));
    }

    #[test]
    fn an_ambiguous_aumid_is_not_guessed() {
        let idx = index(&[(CALC, &[100, 200])], &[], 0);
        assert_eq!(pick_process(CALC, &idx), None);
        assert!(unresolved_note("frame", CALC, &idx).contains("ambiguous"));
        let idx = index(&[(CALC, &[100, 200])], &[100, 200], 0);
        assert_eq!(pick_process(CALC, &idx), None);
    }
}
