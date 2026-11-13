//! Explicit MSAA/IA2 source. UIA is never consulted by this provider.
pub(super) mod abi;
mod rich;
mod semantics;
use abi::*;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::{IUnknown, Interface, BOOL, BSTR};
use windows::Win32::Foundation::{E_NOINTERFACE, HWND, LPARAM};
use windows::Win32::System::Com::{
    CoCancelCall, CoDisableCallCancellation, CoEnableCallCancellation, CoInitializeEx,
    CoUninitialize, IDispatch, IServiceProvider, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    GetCurrentThreadId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::{VARIANT, VT_BSTR, VT_DISPATCH, VT_I4};
use windows::Win32::UI::Accessibility::{
    AccessibleChildren, AccessibleObjectFromWindow, IAccessible,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    IsWindow, IsWindowVisible, GA_ROOT,
};
use xa11y_core::event_provider::Subscription;
use xa11y_core::handle_cache::{BoundedHandleCache, MAX_CACHED_ELEMENTS};
use xa11y_core::table::*;
use xa11y_core::text_api::*;
use xa11y_core::{
    ElementData, ElementParts, Error, HiddenReason, Provider, Rect, Result, Role, StateParts,
    Toggled,
};

const LIMIT: usize = 4096;
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
fn platform(message: impl Into<String>) -> Error {
    Error::platform(-1, message)
}
fn native(error: windows::core::Error) -> Error {
    Error::platform(error.code().0 as i64, format!("MSAA/IA2: {error}"))
}
fn stale() -> Error {
    Error::stale(
        xa11y_core::StaleReason::Removed,
        "MSAA/IA2 object identity or owning window",
    )
}
fn unsupported(e: &ElementData, action: &str) -> Error {
    Error::ActionNotSupported {
        action: action.into(),
        role: e.role,
    }
}
fn bounded(n: i32, maximum: usize, what: &str) -> Result<usize> {
    let n = usize::try_from(n)
        .map_err(|_| platform(format!("IA2 {what} returned negative count {n}")))?;
    if n > maximum {
        return Err(Error::Unsupported {
            feature: format!("IA2 {what} exceeds limit {maximum}: {n}"),
        });
    }
    Ok(n)
}
fn integer(v: &VARIANT) -> Result<i32> {
    if v.vt() != VT_I4 {
        return Err(platform("MSAA expected VT_I4"));
    }
    Ok(unsafe { v.Anonymous.Anonymous.Anonymous.lVal })
}
/// What `IAccessible::get_accRole` answered. MSAA lets a server return a
/// role string (VT_BSTR) in place of a `ROLE_SYSTEM_*` id: Firefox does for
/// roles MSAA has no id for (`paragraph`), and older Chromium for some tags.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MsaaRole {
    Id(i32),
    Name(String),
}
fn msaa_role(v: &VARIANT) -> Result<MsaaRole> {
    if v.vt() == VT_I4 {
        return Ok(MsaaRole::Id(unsafe {
            v.Anonymous.Anonymous.Anonymous.lVal
        }));
    }
    if v.vt() == VT_BSTR {
        let name = BSTR::try_from(v).map_err(native)?;
        if name.len() > 256 {
            return Err(platform("MSAA role string exceeds 256 UTF-16 units"));
        }
        return Ok(MsaaRole::Name(name.to_string()));
    }
    Err(platform(format!(
        "MSAA get_accRole returned VARIANT type {}, expected VT_I4 or VT_BSTR",
        v.vt().0
    )))
}
/// MSAA's "this property is not provided": `DISP_E_MEMBERNOTFOUND` or
/// `E_NOTIMPL`. Any other failure is a real error.
fn absent(e: &windows::core::Error) -> bool {
    matches!(e.code().0, -2147352573 | -2147467263)
}
fn bstr(v: windows::core::Result<BSTR>) -> Result<Option<String>> {
    let v = match v {
        Ok(v) => v,
        Err(e) if absent(&e) => return Ok(None),
        Err(e) => return Err(native(e)),
    };
    if v.len() > 65536 {
        return Err(platform("MSAA property exceeds 65536 UTF-16 units"));
    }
    Ok((!v.is_empty()).then(|| v.to_string()))
}
fn query(acc: &IAccessible) -> Result<Option<Accessible2>> {
    let service: IServiceProvider = match acc.cast() {
        Ok(s) => s,
        Err(e) if e.code() == E_NOINTERFACE => return Ok(None),
        Err(e) => return Err(native(e)),
    };
    match unsafe { service.QueryService::<Accessible2>(&IAccessible::IID) } {
        Ok(a) => Ok(Some(a)),
        Err(e) if e.code() == E_NOINTERFACE => Ok(None),
        Err(e) => Err(native(e)),
    }
}
fn capability<T: Interface>(a: &Accessible2) -> Result<Option<T>> {
    match a.cast() {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.code() == E_NOINTERFACE => Ok(None),
        Err(e) => Err(native(e)),
    }
}
#[derive(Clone)]
struct Owner {
    hwnd: usize,
    pid: u32,
    generation: u64,
}
fn generation(pid: u32) -> Result<u64> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME, STILL_ACTIVE};
    use windows::Win32::System::Threading::GetExitCodeProcess;
    // PROCESS_QUERY_LIMITED_INFORMATION covers both GetProcessTimes and
    // GetExitCodeProcess.
    let h =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(native)?;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let times = unsafe { GetProcessTimes(h, &mut creation, &mut exit, &mut kernel, &mut user) };
    let mut code = 0u32;
    let status = unsafe { GetExitCodeProcess(h, &mut code) };
    let close = unsafe { CloseHandle(h) };
    times.map_err(native)?;
    status.map_err(native)?;
    close.map_err(native)?;
    // GetProcessTimes' exit time is undefined while the process runs, so
    // liveness comes from the exit code. A process that exited with code 259
    // (STILL_ACTIVE) reads as running; the creation time below still changes
    // if its pid is reused, so that ambiguity cannot alias a new process.
    if code != STILL_ACTIVE.0 as u32 {
        return Err(stale());
    }
    // The creation time is the generation key: it distinguishes a reused pid.
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}
fn process_name(pid: u32) -> Result<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_WIN32};
    let handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(native)?;
    let mut buf = vec![0u16; 32768];
    let mut size = buf.len() as u32;
    let read = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    let close = unsafe { CloseHandle(handle) };
    read.map_err(native)?;
    close.map_err(native)?;
    let path = String::from_utf16(&buf[..size as usize])
        .map_err(|e| platform(format!("process image path: {e}")))?;
    std::path::Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .ok_or_else(|| platform("IA2 application process image has no UTF-8 file stem"))
}

impl Owner {
    fn validate(&self) -> Result<()> {
        let hwnd = HWND(self.hwnd as _);
        let mut pid = 0;
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return Err(stale());
        }
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        if pid != self.pid || generation(pid)? != self.generation {
            return Err(stale());
        }
        Ok(())
    }
}
#[derive(Clone)]
struct Node {
    object: IAccessible,
    child: i32,
    owner: Owner,
    parent: u64,
    ia2: Option<Accessible2>,
    identity: Option<(i32, usize)>,
    event_owner: Option<Owner>,
    native_parent: Option<IAccessible>,
    root_instance: bool,
}
impl Node {
    fn new(object: IAccessible, child: i32, owner: Owner, parent: u64) -> Result<Self> {
        let ia2 = if child == 0 { query(&object)? } else { None };
        let identity = if let Some(a) = &ia2 {
            let mut id = 0;
            let mut hwnd = HWND::default();
            unsafe {
                (a.vtable().unique_id)(a.as_raw(), &mut id)
                    .ok()
                    .map_err(native)?;
                (a.vtable().window_handle)(a.as_raw(), &mut hwnd)
                    .ok()
                    .map_err(native)?;
            }
            Some((id, hwnd.0 as usize))
        } else {
            None
        };
        let event_owner = if let Some((_, hwnd)) = identity {
            if unsafe { GetAncestor(HWND(hwnd as _), GA_ROOT) }.0 as usize != owner.hwnd {
                return Err(platform(
                    "IA2 event window is not owned by the selected browser window",
                ));
            }
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(HWND(hwnd as _), Some(&mut pid));
            }
            Some(Owner {
                hwnd,
                pid,
                generation: generation(pid)?,
            })
        } else {
            None
        };
        let native_parent = if identity.is_some() {
            match unsafe { object.accParent() } {
                Ok(parent) => Some(parent.cast().map_err(native)?),
                Err(e) if e.code().0 == 0 => None,
                Err(e) => return Err(native(e)),
            }
        } else {
            None
        };
        Ok(Self {
            object,
            child,
            owner,
            parent,
            ia2,
            identity,
            event_owner,
            native_parent,
            root_instance: false,
        })
    }
    fn validate_lookup(&self) -> Result<()> {
        let (Some(a), Some((id, hwnd))) = (&self.ia2, self.identity) else {
            return Ok(());
        };
        let mut raw = std::ptr::null_mut();
        unsafe {
            AccessibleObjectFromWindow(HWND(hwnd as _), 0xffff_fffc, &IAccessible::IID, &mut raw)
        }
        .map_err(native)?;
        if raw.is_null() {
            return Err(stale());
        }
        let root = unsafe { IAccessible::from_raw(raw) };
        if self.root_instance {
            let initial: IUnknown = a.cast().map_err(native)?;
            let current: IUnknown = query(&root)?.ok_or_else(stale)?.cast().map_err(native)?;
            if initial != current {
                return Err(stale());
            }
        }
        let fresh = root_child(&root, id)?;
        let fresh = query(&fresh)?.ok_or_else(stale)?;
        let old_identity: IUnknown = a.cast().map_err(native)?;
        let fresh_identity: IUnknown = fresh.cast().map_err(native)?;
        if old_identity != fresh_identity {
            return Err(stale());
        }
        Ok(())
    }
    fn attached_to_parent(&self, parent: &IAccessible) -> Result<bool> {
        let count = bounded(
            unsafe { parent.accChildCount() }.map_err(native)?,
            LIMIT,
            "stale-object parent children",
        )?;
        let mut values: Vec<VARIANT> = (0..count).map(|_| VARIANT::default()).collect();
        let mut got = 0;
        if count > 0 {
            unsafe { AccessibleChildren(parent, 0, &mut values, &mut got) }.map_err(native)?;
        }
        let got = bounded(got, count, "stale-object returned children")?;
        let original: IUnknown = match &self.ia2 {
            Some(a) => a.cast().map_err(native)?,
            None => self.object.cast().map_err(native)?,
        };
        for value in &values[..got] {
            if value.vt() == VT_DISPATCH {
                let object: IAccessible = IDispatch::try_from(value)
                    .map_err(native)?
                    .cast()
                    .map_err(native)?;
                let candidate: IUnknown = match query(&object)? {
                    Some(a) => a.cast().map_err(native)?,
                    None => object.cast().map_err(native)?,
                };
                if candidate == original {
                    return Ok(true);
                }
            } else if value.vt() != VT_I4 {
                return Err(platform("unexpected variant while checking IA2 attachment"));
            }
        }
        Ok(false)
    }
    fn ensure_attached(&self) -> Result<()> {
        if self.root_instance {
            return Ok(());
        }
        match unsafe { self.object.accParent() } {
            Ok(parent) => {
                let parent: IAccessible = parent.cast().map_err(native)?;
                if self.attached_to_parent(&parent)? {
                    Ok(())
                } else {
                    Err(stale())
                }
            }
            Err(error) => {
                // Refine a failed parent lookup using the original parent's
                // fresh child list. Never continue an action after that
                // lookup failed, even if the original parent still owns it.
                if let Some(parent) = &self.native_parent {
                    if !self.attached_to_parent(parent)? {
                        return Err(stale());
                    }
                }
                Err(native(error))
            }
        }
    }
    fn classify_error(&self, error: Error) -> Error {
        if let Error::Platform { code, message, .. } = error {
            if matches!(code as i32 as u32, 0x80010108 | 0x800401fd | 0x800706ba) {
                return stale();
            }
            if code == 0x80004005u32 as i32 as i64 {
                return match self.ensure_attached() {
                    Err(Error::ElementStale { .. }) => stale(),
                    Ok(()) => Error::platform(code, message),
                    Err(proof) => Error::platform(
                        code,
                        format!("{message}; attachment check also failed: {proof}"),
                    ),
                };
            }
            Error::platform(code, message)
        } else {
            error
        }
    }
    fn validate(&self) -> Result<()> {
        self.owner.validate()?;
        if let Some(owner) = &self.event_owner {
            owner.validate()?;
        }
        let result = (|| {
            self.validate_lookup()?;
            if let (Some(a), Some((id, hwnd))) = (&self.ia2, self.identity) {
                let mut current = 0;
                let mut current_hwnd = HWND::default();
                let mut states = 0;
                unsafe {
                    (a.vtable().unique_id)(a.as_raw(), &mut current)
                        .ok()
                        .map_err(native)?;
                    (a.vtable().window_handle)(a.as_raw(), &mut current_hwnd)
                        .ok()
                        .map_err(native)?;
                    (a.vtable().states)(a.as_raw(), &mut states)
                        .ok()
                        .map_err(native)?;
                }
                if current != id || current_hwnd.0 as usize != hwnd || states & 4 != 0 {
                    return Err(stale());
                }
            }
            Ok(())
        })();
        result.map_err(|error| self.classify_error(error))
    }
    fn interface<T: Interface>(&self, e: &ElementData, action: &str) -> Result<T> {
        self.validate()?;
        let a = self.ia2.as_ref().ok_or_else(|| unsupported(e, action))?;
        capability(a)?.ok_or_else(|| unsupported(e, action))
    }
}
fn root_child(root: &IAccessible, id: i32) -> Result<IAccessible> {
    match unsafe { root.get_accChild(&id.into()) } {
        Ok(child) => child.cast().map_err(native),
        Err(error) if matches!(error.code().0 as u32, 0 | 0x80070057 | 0x80020003) => Err(stale()),
        Err(error) => Err(Error::platform(
            error.code().0 as i64,
            format!("resolving retained IA2 uniqueID {id}: {error}"),
        )),
    }
}
#[derive(Clone)]
enum Entry {
    App(Owner),
    Window(Node),
    Node(Node),
}
struct State {
    /// Retained objects by handle. Bounded and aged like the UIA backend's
    /// handle table: an entry stays resolvable until enough newer ones push
    /// it out, however long that takes, and a handle is never reused, so a
    /// lookup miss is always `Evicted`.
    entries: BoundedHandleCache<Entry>,
    next: u64,
    dispatched: bool,
    deadline: Option<Instant>,
    /// Set when [`check_deadline`](Self::check_deadline) refused to start a
    /// native operation, so the worker can tell a cooperative stop from a
    /// native call that overran its budget.
    deadline_hit: std::cell::Cell<bool>,
}
impl State {
    fn new() -> Self {
        Self {
            entries: BoundedHandleCache::new(MAX_CACHED_ELEMENTS),
            next: 1,
            dispatched: false,
            deadline: None,
            deadline_hit: std::cell::Cell::new(false),
        }
    }
    /// Whether the request's deadline has passed. No side effect: unlike
    /// [`check_deadline`](Self::check_deadline) it does not record a
    /// cooperative stop, so a caller that only asks cannot turn the work's
    /// own result into a budget timeout.
    fn deadline_passed(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
    fn check_deadline(&self) -> Result<()> {
        if self.deadline_passed() {
            self.deadline_hit.set(true);
            return Err(platform(
                "IA2 request deadline expired before the next native operation",
            ));
        }
        Ok(())
    }
    fn before_mutation(&mut self) -> Result<()> {
        self.check_deadline()?;
        self.dispatched = true;
        Ok(())
    }
    fn retain(&mut self, entry: Entry) -> u64 {
        let id = (1u64 << 62) | self.next;
        self.next += 1;
        self.entries.insert(id, entry);
        id
    }
    fn get(&mut self, e: &ElementData) -> Result<Entry> {
        if e.raw.get("accessibility_backend").and_then(|v| v.as_str()) != Some("ia2") {
            return Err(stale());
        }
        let evicted = || {
            Error::stale(
                xa11y_core::StaleReason::Evicted,
                format!("IA2 handle {} is no longer retained", e.handle),
            )
        };
        let entry = self.entries.get(&e.handle).ok_or_else(evicted)?.clone();
        match &entry {
            Entry::App(o) => {
                if generation(o.pid)? != o.generation {
                    return Err(stale());
                }
            }
            Entry::Window(n) | Entry::Node(n) => n.validate()?,
        }
        Ok(entry)
    }
    fn actionable(&mut self, e: &ElementData) -> Result<Node> {
        let n = self.node(e)?;
        if n.identity.is_none() {
            return Err(unsupported(
                e,
                "actions on MSAA objects without stable IA2 identity",
            ));
        }
        n.ensure_attached()?;
        Ok(n)
    }
    fn classify_result<T>(&self, e: &ElementData, result: Result<T>) -> Result<T> {
        // A failed native mutation may already have taken effect. Never turn
        // its HRESULT into a NotDelivered stale error after dispatch. Past the
        // deadline, classification (which can make native calls) is skipped,
        // but the work's own result is returned as it is: the work finished,
        // so a real error must not be replaced with a budget timeout.
        if self.dispatched || self.deadline_passed() {
            return result;
        }
        result.map_err(|error| match self.entries.get(&e.handle) {
            Some(Entry::Node(node) | Entry::Window(node)) => node.classify_error(error),
            _ => error,
        })
    }
    fn node(&mut self, e: &ElementData) -> Result<Node> {
        match self.get(e)? {
            Entry::Node(n) => Ok(n),
            _ => Err(unsupported(e, "IA2 object")),
        }
    }
    fn windows(&self, pid: Option<u32>) -> Result<Vec<Owner>> {
        unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
            let out = unsafe { &mut *(data.0 as *mut Vec<(usize, u32)>) };
            if out.len() >= LIMIT {
                return BOOL(0);
            }
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            if pid != 0 && unsafe { IsWindowVisible(hwnd) }.as_bool() {
                out.push((hwnd.0 as usize, pid));
            }
            BOOL(1)
        }
        let mut windows = Vec::<(usize, u32)>::new();
        unsafe { EnumWindows(Some(visit), LPARAM((&raw mut windows) as isize)) }.map_err(native)?;
        windows
            .into_iter()
            .filter(|(_, p)| pid.is_none_or(|wanted| wanted == *p))
            .map(|(hwnd, pid)| {
                Ok(Owner {
                    hwnd,
                    pid,
                    generation: generation(pid)?,
                })
            })
            .collect()
    }
    fn synthetic(&mut self, owner: Owner, app: bool) -> Result<ElementData> {
        owner.validate()?;
        let mut title = [0u16; 1024];
        let n = unsafe { GetWindowTextW(HWND(owner.hwnd as _), &mut title) };
        let name = if app {
            process_name(owner.pid)?
        } else {
            String::from_utf16_lossy(&title[..n.max(0) as usize])
        };
        let role = if app { Role::Application } else { Role::Window };
        let entry = if app {
            Entry::App(owner.clone())
        } else {
            let mut raw = std::ptr::null_mut();
            unsafe {
                AccessibleObjectFromWindow(
                    HWND(owner.hwnd as _),
                    0xffff_fffc,
                    &IAccessible::IID,
                    &mut raw,
                )
            }
            .map_err(native)?;
            if raw.is_null() {
                return Err(platform("MSAA window root is null"));
            }
            let mut node = Node::new(unsafe { IAccessible::from_raw(raw) }, 0, owner.clone(), 0)?;
            node.root_instance = true;
            if node.identity.is_none() {
                return Err(Error::Unsupported {
                    feature: "selected window exposes no stable IA2 object identity".into(),
                });
            }
            Entry::Window(node)
        };
        let handle = self.retain(entry);
        Ok(data(
            Snapshot {
                role,
                name: Some(name),
                value: None,
                description: None,
                bounds: None,
                raw: HashMap::new(),
                table: None,
                cell: None,
                actions: Vec::new(),
            },
            0,
            &owner,
            handle,
        ))
    }

    fn snapshot(&mut self, node: Node) -> Result<ElementData> {
        node.validate()?;
        let child = VARIANT::from(node.child);
        let msaa = msaa_role(&unsafe { node.object.get_accRole(&child) }.map_err(native)?)?;
        let mut ia2_states = 0;
        // IA2's role is the MSAA role where one fits and an IA2_ROLE_* above
        // 0x400 otherwise. Without IA2 it is the MSAA id; a role string
        // (below) has no id, so 0, which maps to nothing.
        let mut ia2_role = match &msaa {
            MsaaRole::Id(id) => *id,
            MsaaRole::Name(_) => 0,
        };
        if let Some(a) = &node.ia2 {
            unsafe {
                (a.vtable().role)(a.as_raw(), &mut ia2_role)
                    .ok()
                    .map_err(native)?;
                (a.vtable().states)(a.as_raw(), &mut ia2_states)
                    .ok()
                    .map_err(native)?;
            }
        }
        // A role string (Firefox's `paragraph`, say) is a real answer, not
        // an error: map the element by its IA2 role, which is Unknown when
        // that has no mapping either, and report the string in
        // raw["msaa_role"] so the platform's own answer stays visible.
        let role_id = match &msaa {
            MsaaRole::Id(id) => *id,
            MsaaRole::Name(_) => ia2_role,
        };
        let role = crate::uia::map_msaa_role(role_id).unwrap_or(Role::Unknown);
        let states = integer(&unsafe { node.object.get_accState(&child) }.map_err(native)?)?;
        let name = bstr(unsafe { node.object.get_accName(&child) })?;
        let value = bstr(unsafe { node.object.get_accValue(&child) })?;
        let description = bstr(unsafe { node.object.get_accDescription(&child) })?;
        let action = bstr(unsafe { node.object.get_accDefaultAction(&child) })?;
        let dpi = crate::dpi::DpiScope::enter()?;
        let bounds = match crate::coords::msaa_bounds(&dpi, &node.object, &child) {
            Ok(b) => Some(b),
            Err(e) if absent(&e) => None,
            Err(e) => return Err(native(e)),
        };
        let mut raw = HashMap::new();
        raw.insert(
            "msaa_role".into(),
            match msaa {
                MsaaRole::Id(id) => id.into(),
                MsaaRole::Name(name) => name.into(),
            },
        );
        if let Some((id, hwnd)) = node.identity {
            raw.insert("ia2_unique_id".into(), id.into());
            raw.insert("ia2_window".into(), hwnd.into());
        }
        if matches!(role, Role::TextField | Role::TextArea) {
            let (text, text2) = if let Some(a) = &node.ia2 {
                (
                    capability::<AccessibleText>(a)?.is_some(),
                    capability::<AccessibleText2>(a)?.is_some(),
                )
            } else {
                (false, false)
            };
            raw.insert("ia2_text".into(), text.into());
            raw.insert("ia2_text2".into(), text2.into());
        }
        let (table, cell) = rich::metadata(&node, role_id)?;
        let attrs = semantics::object_attributes(&node)?;
        let link_url = if role == Role::Link {
            if let Some(a) = &node.ia2 {
                if let Some(link) = capability::<AccessibleHyperlink>(a)? {
                    let mut target = VARIANT::default();
                    unsafe {
                        (link.vtable().anchorTarget)(link.as_raw(), 0, &mut target)
                            .ok()
                            .map_err(native)?;
                    }
                    if target.vt() == windows::Win32::System::Variant::VT_BSTR {
                        bstr(BSTR::try_from(&target))?
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        let mut actions = Vec::new();
        if node.identity.is_some() {
            if action.is_some() {
                actions.push("press".into());
            }
            if states & 0x100000 != 0 {
                actions.push("focus".into());
            }
            if states & 0x200000 != 0 {
                actions.push("select".into());
            }
        }
        let owner = node.owner.clone();
        let handle = self.retain(Entry::Node(node));
        let mut result = data(
            Snapshot {
                role,
                name,
                value,
                description,
                bounds,
                raw,
                table,
                cell,
                actions,
            },
            states,
            &owner,
            handle,
        );
        semantics::apply(&mut result, &attrs, ia2_role)?;
        result.link_url = link_url;
        result.states.required = ia2_states & 0x800 != 0;
        result.states.modal = ia2_states & 0x100 != 0;
        // IA2 editable means a caret/text interface, even on read-only fields.
        // The shared flag promises writes, so preserve MSAA's READONLY bit.
        result.states.editable |= ia2_states & 0x8 != 0 && states & 0x40 == 0;
        result.raw.insert("ia2_states".into(), ia2_states.into());
        Ok(result)
    }

    fn children(&mut self, e: Option<ElementData>) -> Result<Vec<ElementData>> {
        self.check_deadline()?;
        let Some(e) = e else {
            return self.apps();
        };
        match self.get(&e)? {
            Entry::App(owner) => self
                .windows(Some(owner.pid))?
                .into_iter()
                .map(|o| self.synthetic(o, false))
                .collect(),
            Entry::Window(mut node) => {
                node.parent = e.handle;
                Ok(vec![self.snapshot(node)?])
            }
            Entry::Node(node) => {
                if node.child != 0 {
                    return Ok(Vec::new());
                }
                let count = bounded(
                    unsafe { node.object.accChildCount() }.map_err(native)?,
                    LIMIT,
                    "child count",
                )?;
                let mut values: Vec<VARIANT> = (0..count).map(|_| VARIANT::default()).collect();
                let mut got = 0;
                if count > 0 {
                    unsafe { AccessibleChildren(&node.object, 0, &mut values, &mut got) }
                        .map_err(native)?;
                }
                let got = bounded(got, count, "returned child count")?;
                let mut out = Vec::new();
                for value in &values[..got] {
                    self.check_deadline()?;
                    let (object, child) = match value.vt() {
                        VT_DISPATCH => (
                            IDispatch::try_from(value)
                                .map_err(native)?
                                .cast()
                                .map_err(native)?,
                            0,
                        ),
                        VT_I4 => {
                            let id = integer(value)?;
                            if id <= 0 {
                                return Err(platform("MSAA simple-child ID must be positive"));
                            }
                            (node.object.clone(), id)
                        }
                        _ => return Err(platform("unexpected MSAA child variant")),
                    };
                    let data =
                        self.snapshot(Node::new(object, child, node.owner.clone(), e.handle)?)?;
                    // Inactive browser documents must never contribute content.
                    if data.role == Role::WebArea && !data.states.visible {
                        continue;
                    }
                    out.push(data);
                }
                Ok(out)
            }
        }
    }
    fn apps(&mut self) -> Result<Vec<ElementData>> {
        let mut seen = std::collections::HashSet::new();
        self.windows(None)?
            .into_iter()
            .filter(|o| seen.insert(o.pid))
            .map(|o| self.synthetic(o, true))
            .collect()
    }
}
struct Snapshot {
    role: Role,
    name: Option<String>,
    value: Option<String>,
    description: Option<String>,
    bounds: Option<Rect>,
    raw: HashMap<String, serde_json::Value>,
    table: Option<TableInfo>,
    cell: Option<CellInfo>,
    actions: Vec<String>,
}
fn data(snapshot: Snapshot, bits: i32, owner: &Owner, handle: u64) -> ElementData {
    let Snapshot {
        role,
        name,
        value,
        description,
        bounds,
        mut raw,
        table,
        cell,
        actions,
    } = snapshot;
    for (key, value) in [
        ("msaa_name", &name),
        ("msaa_value", &value),
        ("msaa_description", &description),
    ] {
        if let Some(value) = value {
            raw.insert(key.into(), value.clone().into());
        }
    }
    let name = xa11y_core::text::strip_bidi_opt(name);
    let value = xa11y_core::text::strip_bidi_opt(value);
    let description = xa11y_core::text::strip_bidi_opt(description);
    raw.insert("accessibility_backend".into(), "ia2".into());
    let visible = bits & (0x8000 | 0x10000) == 0;
    let states = StateParts {
        enabled: bits & 1 == 0,
        visible,
        hidden_reason: (!visible).then_some(HiddenReason::NotShowing),
        focused: bits & 4 != 0,
        active: role == Role::Window && unsafe { GetForegroundWindow() }.0 as usize == owner.hwnd,
        minimized: None,
        maximized: None,
        fullscreen: None,
        checked: if matches!(role, Role::CheckBox | Role::RadioButton) {
            Some(if bits & 0x20 != 0 {
                Toggled::Mixed
            } else if bits & 0x10 != 0 {
                Toggled::On
            } else {
                Toggled::Off
            })
        } else {
            None
        },
        selected: bits & 2 != 0,
        expanded: if bits & 0x200 != 0 {
            Some(true)
        } else if bits & 0x400 != 0 {
            Some(false)
        } else {
            None
        },
        editable: matches!(role, Role::TextField | Role::TextArea) && bits & 0x40 == 0,
        focusable: bits & 0x100000 != 0,
        modal: false,
        required: false,
        busy: bits & 0x800 != 0,
        password: bits & 0x20000000 != 0,
    }
    .into();
    ElementParts {
        heading_level: None,
        landmark_type: None,
        web_role: None,
        link_url: None,
        live_region: None,
        role,
        name,
        value,
        description,
        bounds,
        actions,
        states,
        numeric_value: None,
        min_value: None,
        max_value: None,
        table,
        cell,
        table_row: None,
        collection: None,
        stable_id: None,
        pid: Some(owner.pid),
        raw,
        handle,
    }
    .into()
}

type Job = Box<dyn FnOnce(&mut State) + Send>;

/// Run the worker's queue until every sender is gone.
///
/// A job that panics must not take the worker with it: the thread owns
/// every retained COM interface, and the process-wide slot would stay
/// `Ready` with nothing serving it. Each job already turns a panic in its
/// request into an error for its caller (see `call_with_timeout`); this is
/// the backstop for a panic outside that, whose caller then sees its answer
/// channel close. `State` holds caches and handles whose consistency a
/// panic cannot break in a memory-unsafe way, so it is kept, as a poisoned
/// cache lock is recovered rather than fatal (tenet 4).
fn serve(jobs: mpsc::Receiver<Job>, state: &mut State) {
    while let Ok(job) = jobs.recv() {
        if let Err(panic) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut *state)))
        {
            eprintln!(
                "xa11y: IA2 worker job panicked outside its request: {}",
                panic_message(panic.as_ref())
            );
        }
    }
}

/// The message a panic carried, when it carried a string.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a non-string payload".into())
}

/// Clears the worker's `running` record when a request ends, however it
/// ends, so a panic can never leave a record that later reads as a hung
/// native call.
struct ClearRunning(Arc<Mutex<Option<Running>>>);
impl Drop for ClearRunning {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// How long past a request's budget its caller keeps waiting before it
/// treats the native call as hung. A request whose budget ends between
/// native operations stops at its next `check_deadline`; this covers the gap
/// between that stop and the caller seeing it.
const STALL_GRACE: Duration = Duration::from_millis(200);
/// How long a caller sleeps between attempts to enqueue on a full queue.
const QUEUE_RETRY: Duration = Duration::from_millis(2);

/// The request the worker is running, so a caller can tell whether the
/// worker is hung and cancel only its own call.
#[derive(Clone, Copy)]
struct Running {
    id: u64,
    started: Instant,
    budget: Duration,
}
impl Running {
    fn overran(&self, now: Instant) -> bool {
        now >= self.started + self.budget + STALL_GRACE
    }
}

const QUEUED: u8 = 0;
const STARTED: u8 = 1;
const CANCELLED: u8 = 2;

/// One request's hand-off between its caller and the worker. Exactly one of
/// them moves it out of `QUEUED`: the worker to `STARTED`, or the caller to
/// `CANCELLED` once it stops waiting, after which the worker skips it.
#[derive(Default)]
struct Ticket {
    status: std::sync::atomic::AtomicU8,
    /// When the worker began running the request. Set before `status`
    /// becomes `STARTED`, so a caller that sees `STARTED` can read it.
    started: std::sync::OnceLock<Instant>,
}
impl Ticket {
    /// Claim the request for the worker; `None` when its caller cancelled it.
    fn start(&self) -> Option<Instant> {
        let now = Instant::now();
        // Set before the claim; a cancelled ticket's start time is never read,
        // and only the worker sets it, once.
        let _ = self.started.set(now);
        self.status
            .compare_exchange(QUEUED, STARTED, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| now)
    }
    /// Withdraw a request that has not started. False once the worker has it.
    fn cancel(&self) -> bool {
        self.status
            .compare_exchange(QUEUED, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    fn started_at(&self) -> Option<Instant> {
        if self.status.load(Ordering::Acquire) == STARTED {
            self.started.get().copied()
        } else {
            None
        }
    }
}

struct Worker {
    sender: mpsc::SyncSender<Job>,
    thread: u32,
    running: Arc<Mutex<Option<Running>>>,
    next_id: std::sync::atomic::AtomicU64,
}
impl Worker {
    fn new(sender: mpsc::SyncSender<Job>, thread: u32) -> Self {
        Self {
            sender,
            thread,
            running: Arc::default(),
            next_id: std::sync::atomic::AtomicU64::new(1),
        }
    }
    fn running(&self) -> Option<Running> {
        *self.running.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Whether the request the worker is running right now has overrun its
    /// budget: a native call is hung in it, and every later request would
    /// only queue behind it. Read from the live `running` record, which the
    /// worker clears when the call returns, so a call that was only slow
    /// leaves nothing behind once it finishes, and a record of a request
    /// that has since returned can never disable the worker.
    fn check_hung(&self) -> bool {
        self.running()
            .is_some_and(|running| running.overran(Instant::now()))
    }
}
enum WorkerInit {
    Vacant,
    Ready(Arc<Worker>),
    Failed,
}
static WORKER: Mutex<WorkerInit> = Mutex::new(WorkerInit::Vacant);
/// Explicit MSAA/IA2 provider. Select before the umbrella provider's first use.
/// Native interfaces remain confined to one MTA worker. Requests and retained
/// handles are bounded. Each request's budget runs from when the worker
/// starts it. A request that times out in the queue, or stops at its next
/// native operation when its budget or the caller's deadline runs out, is an
/// ordinary timeout. While a native COM call is still running past its
/// budget, the process-wide worker is disabled: no replacement threads are
/// spawned and other calls fail explicitly instead of queueing behind it.
/// Once the call returns, the worker serves requests again. COM cancellation
/// of the hung call is best effort, because an in-process/misbehaving server
/// may not honor it.
pub struct Ia2Provider {
    worker: Arc<Worker>,
}
impl Ia2Provider {
    /// Create the explicit IA2 provider, sharing its bounded process worker.
    pub fn new() -> Result<Self> {
        let mut slot = WORKER.lock().unwrap_or_else(|e| e.into_inner());
        if let WorkerInit::Ready(worker) = &*slot {
            return Ok(Self {
                worker: Arc::clone(worker),
            });
        }
        if matches!(*slot, WorkerInit::Failed) {
            return Err(platform(
                "IA2 worker initialization failed previously; restart the host",
            ));
        }
        // A hung COM initialization must not create replacement threads.
        *slot = WorkerInit::Failed;
        let (tx, rx) = mpsc::sync_channel::<Job>(32);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("xa11y-ia2".into())
            .spawn(move || {
                let init =
                    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }.map_err(native);
                if let Err(error) = init {
                    let _ = ready_tx.send(Err(error));
                    return;
                } // Caller may already have timed out.
                let enabled = unsafe { CoEnableCallCancellation(None) }.map_err(native);
                if let Err(error) = enabled {
                    let _ = ready_tx.send(Err(error));
                    unsafe {
                        CoUninitialize();
                    }
                    return;
                } // Caller may have timed out.
                let id = unsafe { GetCurrentThreadId() };
                if ready_tx.send(Ok(id)).is_ok() {
                    let mut state = State::new();
                    serve(rx, &mut state);
                    // All retained COM interfaces are released on their owner thread.
                    drop(state);
                }
                if let Err(e) = unsafe { CoDisableCallCancellation(None) } {
                    eprintln!("IA2 cancellation cleanup: {e}");
                }
                unsafe {
                    CoUninitialize();
                }
            })
            .map_err(|e| platform(format!("start IA2 worker: {e}")))?;
        let thread = ready_rx
            .recv_timeout(CALL_TIMEOUT)
            .map_err(|e| platform(format!("initialize IA2 worker: {e}")))??;
        let worker = Arc::new(Worker::new(tx, thread));
        *slot = WorkerInit::Ready(Arc::clone(&worker));
        Ok(Self { worker })
    }
    fn call_for<T: Send + 'static>(
        &self,
        operation: &'static str,
        e: ElementData,
        f: impl FnOnce(&mut State) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let context = Arc::new(e);
        let worker_context = Arc::clone(&context);
        self.call(move |state| {
            let result = f(state);
            state.classify_result(&worker_context, result)
        })
        .map_err(|error| {
            let name = context
                .name
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(160)
                .collect::<String>();
            match error {
                Error::Platform { code, message, .. } => Error::platform(
                    code,
                    format!(
                        "IA2 {operation} on {} {name:?} (pid {:?}, handle {}): {message}",
                        context.role, context.pid, context.handle
                    ),
                ),
                Error::ElementStale { reason, element } => Error::stale(
                    reason,
                    format!("{}[name={name:?}] via IA2: {element}", context.role),
                ),
                error => error,
            }
        })
    }
    fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut State) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.call_with_timeout(CALL_TIMEOUT, f)
    }
    /// Run `f` on the IA2 worker, bounded by `timeout` and by the caller's
    /// open [`Deadline`](xa11y_core::Deadline) (#531).
    ///
    /// `timeout` is the request's budget, measured from when the worker
    /// starts it, not from when it was queued. Three outcomes end a request
    /// early, and only the last disables the worker:
    ///
    /// - **It never started.** The caller stops waiting for a request still
    ///   queued at its deadline (or after `timeout` in the queue) and
    ///   cancels it; the worker skips it. The error is `NotDelivered`.
    /// - **It stopped cooperatively.** A budget or deadline that expires
    ///   between native operations stops the request at the next one
    ///   (`State::check_deadline`). That is an ordinary timeout.
    /// - **A native call hung.** A request still running `STALL_GRACE`
    ///   after its budget is inside a native call that cannot be taken
    ///   back. The worker is disabled while that call is still running, and
    ///   the caller's own call is cancelled (best effort) with
    ///   `CoCancelCall`. A call that returns, however late, re-enables it.
    ///
    /// The caller's deadline, when it is shorter than the budget, is the
    /// caller choosing to stop waiting: the worker stops at its next native
    /// operation and stays usable.
    fn call_with_timeout<T: Send + 'static>(
        &self,
        timeout: Duration,
        f: impl FnOnce(&mut State) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        if self.worker.check_hung() {
            return Err(disabled());
        }
        let bound = xa11y_core::call_timeout(timeout, || PENDING.to_string())?;
        let caller = xa11y_core::Deadline::current();
        let caller_at = caller.as_ref().map(|d| d.at());
        // How long the request may wait to start: its own budget, or less
        // when the caller's deadline is nearer.
        let queue_limit = Instant::now() + bound.duration;
        let id = self.worker.next_id.fetch_add(1, Ordering::Relaxed);
        let ticket = Arc::new(Ticket::default());
        let (tx, rx) = mpsc::sync_channel(1);
        let job: Job = {
            let ticket = Arc::clone(&ticket);
            let running = Arc::clone(&self.worker.running);
            Box::new(move |state| {
                let Some(started) = ticket.start() else {
                    // The caller stopped waiting before it started; never run it.
                    return;
                };
                let budget_end = started + timeout;
                // The worker is idle here (it runs one job at a time), so
                // whatever hung before has returned and this request runs.
                let result = if caller_at.is_some_and(|at| started >= at) {
                    Err(platform("IA2 request expired before dispatch"))
                } else {
                    let stop_at = caller_at.map_or(budget_end, |at| at.min(budget_end));
                    *running.lock().unwrap_or_else(|e| e.into_inner()) = Some(Running {
                        id,
                        started,
                        budget: timeout,
                    });
                    let clear = ClearRunning(Arc::clone(&running));
                    state.dispatched = false;
                    state.deadline_hit.set(false);
                    state.deadline = Some(stop_at);
                    let result =
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(state)))
                        {
                            Ok(result) => result,
                            // Whatever the request did before it panicked
                            // may have reached the app: a platform error,
                            // whose delivery is Unknown, never a retryable one.
                            Err(panic) => Err(platform(format!(
                                "IA2 request panicked on the worker: {}; an in-flight action may have been delivered; the worker keeps serving requests",
                                panic_message(panic.as_ref())
                            ))),
                        };
                    state.deadline = None;
                    drop(clear);
                    let budget_stop =
                        state.deadline_hit.get() && caller_at.is_none_or(|at| at >= budget_end);
                    match result {
                        Err(_) if budget_stop => Err(budget_timeout(timeout, state.dispatched)),
                        result => result,
                    }
                };
                // The caller may have abandoned a timed-out read/action. Never retry it.
                let _ = tx.send(result);
            })
        };
        if let Err(error) = self.enqueue(job, queue_limit) {
            return Err(error.unwrap_or_else(|| self.not_started(timeout, caller.as_ref())));
        }
        loop {
            let started = ticket.started_at();
            let limit = match started {
                None => queue_limit,
                Some(started) => {
                    let budget_end = started + timeout;
                    caller_at.map_or(budget_end, |at| at.min(budget_end))
                }
            };
            match rx.recv_timeout(limit.saturating_duration_since(Instant::now())) {
                Ok(result) => return self.finish(result, caller.as_ref()),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(platform(
                        "IA2 worker dropped a request without answering (it panicked outside the request, or exited); an in-flight action may have been delivered",
                    ))
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let Some(started) = started else {
                if ticket.cancel() {
                    return Err(self.not_started(timeout, caller.as_ref()));
                }
                // The worker took it as the wait ended; wait on its budget.
                continue;
            };
            if let Some(caller) = caller.as_ref().filter(|d| d.is_expired()) {
                if caller.at() < started + timeout {
                    // The caller stopped waiting, not the worker: it stops at
                    // its next native operation, and stays usable.
                    return Err(caller.expired(Some(PENDING.into())));
                }
            }
            // The budget is spent. A request between native operations stops
            // at the next one; give that stop time to arrive.
            match rx.recv_timeout(STALL_GRACE) {
                Ok(result) => return self.finish(result, caller.as_ref()),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(platform(
                        "IA2 worker dropped a request without answering (it panicked outside the request, or exited); an in-flight action may have been delivered",
                    ))
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if self.worker.running().is_some_and(|r| r.id == id) {
                self.cancel_native_call();
            } else if let Ok(result) = rx.recv_timeout(STALL_GRACE) {
                // The call returned just after the grace (the worker clears
                // `running` before it answers): only slow, not hung.
                return self.finish(result, caller.as_ref());
            }
            return Err(platform(format!(
                "IA2 native request still running {:?} after it started (budget {timeout:?}); an in-flight action may have been delivered; the worker refuses new requests until the call returns",
                started.elapsed()
            )));
        }
    }
    /// Hand `job` to the worker, waiting for queue capacity until `limit`.
    /// `Err(None)` when the queue stayed full until then: the request was
    /// never queued.
    fn enqueue(&self, mut job: Job, limit: Instant) -> std::result::Result<(), Option<Error>> {
        loop {
            match self.worker.sender.try_send(job) {
                Ok(()) => return Ok(()),
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(Some(platform(
                        "IA2 request queue is disconnected; worker exited",
                    )))
                }
                Err(mpsc::TrySendError::Full(back)) => {
                    let left = limit.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(None);
                    }
                    job = back;
                    std::thread::sleep(QUEUE_RETRY.min(left));
                }
            }
        }
    }
    /// The error for a request that never started: it was not run.
    fn not_started(&self, timeout: Duration, caller: Option<&xa11y_core::Deadline>) -> Error {
        // A request ahead of this one that has overrun its budget is a hung
        // native call; record it so later callers fail at once.
        let hung = self.worker.check_hung();
        let error = match caller.filter(|d| d.is_expired()) {
            Some(caller) => caller.expired(Some(PENDING.into())),
            None => Error::timeout(timeout).diagnose(
                xa11y_core::Diagnosis::new()
                    .condition(format!(
                        "IA2 worker to start the request within {timeout:?}{}",
                        if hung {
                            "; the request ahead of it is hung in a native call and the worker is disabled until it returns"
                        } else {
                            "; it was queued behind other requests"
                        }
                    ))
                    .pending(PENDING),
            ),
        };
        let diagnosis = error
            .diagnosis()
            .cloned()
            .unwrap_or_default()
            .delivery(xa11y_core::Delivery::NotDelivered);
        error.diagnose(diagnosis)
    }
    fn finish<T>(&self, result: Result<T>, caller: Option<&xa11y_core::Deadline>) -> Result<T> {
        if let Some(caller) = caller.filter(|d| d.is_expired()) {
            if result.is_err() {
                // The worker stopped at the caller's deadline. Recording the
                // cut is the point; core builds the error the caller sees
                // from it.
                let _ = caller.expired(Some(PENDING.into()));
            }
        }
        result
    }
    /// Ask COM to cancel the outbound call on the worker thread. Called only
    /// for the caller's own request, while it is the one running.
    fn cancel_native_call(&self) {
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        // RPC_E_CHANGED_MODE means the caller already has an STA.
        if let Err(e) = unsafe { CoCancelCall(self.worker.thread, 0) } {
            eprintln!("IA2 cancellation request: {e}");
        }
        if initialized {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

/// The error for a request refused because an earlier native call hung.
fn disabled() -> Error {
    platform("IA2 worker is busy with an earlier native call that has hung past its budget; requests are refused until it returns")
}

/// The error for a request that stopped at its next native operation
/// because its budget ran out.
fn budget_timeout(timeout: Duration, dispatched: bool) -> Error {
    let delivery = if dispatched {
        xa11y_core::Delivery::Unknown
    } else {
        xa11y_core::Delivery::NotDelivered
    };
    Error::timeout(timeout).diagnose(
        xa11y_core::Diagnosis::new()
            .condition(format!(
                "IA2 request to finish within its {timeout:?} budget; it stopped before its next native operation"
            ))
            .pending(PENDING)
            .delivery(delivery),
    )
}

/// What a deadline diagnosis names as pending for an IA2 request. The
/// request runs on the worker thread, so the caller knows only that it was
/// one; `call_for` adds the element to the error message.
const PENDING: &str = "an MSAA/IA2 request on the IA2 worker thread";

impl Provider for Ia2Provider {
    fn capabilities(&self) -> Result<xa11y_core::Capabilities> {
        xa11y_core::Capabilities::for_backend(xa11y_core::Platform::Windows, None, "ia2")
    }
    fn identity(&self, e: &ElementData) -> Result<xa11y_core::NodeIdentity> {
        let e = e.clone();
        self.call_for("identity", e.clone(), move |s| {
            let (owner, key) = match s.get(&e)? {
                Entry::App(owner) => (owner, "application".to_string()),
                Entry::Window(node) => (node.owner.clone(), format!("window:{}", node.owner.hwnd)),
                Entry::Node(node) => {
                    let (id, hwnd) = node
                        .identity
                        .ok_or_else(|| unsupported(&e, "identity on a simple MSAA child"))?;
                    let owner = node.event_owner.clone().ok_or_else(stale)?;
                    (owner, format!("{hwnd}:{id}"))
                }
            };
            Ok(xa11y_core::NodeIdentity::from_key(
                "ia2",
                format!("{}:{}", owner.pid, owner.generation),
                key,
            ))
        })
    }
    fn revalidate(&self, e: &ElementData) -> Result<()> {
        let e = e.clone();
        self.call_for("revalidate", e.clone(), move |s| {
            match s.get(&e)? {
                Entry::Node(node) => {
                    node.ensure_attached()?;
                    let current = s.snapshot(node)?;
                    if current.role != e.role {
                        return Err(stale());
                    }
                }
                Entry::Window(node) => node.ensure_attached()?,
                Entry::App(_) => {}
            }
            Ok(())
        })
    }
    fn relations(
        &self,
        e: &ElementData,
        kinds: &[xa11y_core::RelationKind],
    ) -> Result<Vec<(xa11y_core::RelationKind, Vec<ElementData>)>> {
        let e = e.clone();
        let kinds = kinds.to_vec();
        self.call_for("relations", e.clone(), move |s| s.relations(&e, &kinds))
    }
    fn get_children(&self, e: Option<&ElementData>) -> Result<Vec<ElementData>> {
        let e = e.cloned();
        self.call(move |s| s.children(e))
    }
    fn list_apps(&self) -> Result<Vec<ElementData>> {
        self.call(State::apps)
    }
    fn app_by_pid(&self, pid: u32) -> Result<ElementData> {
        self.call(move |s| {
            let owner = s.windows(Some(pid))?.into_iter().next().ok_or_else(|| {
                Error::selector_not_matched(format!(
                    "application[pid={pid}] via explicit IA2 source"
                ))
            })?;
            s.synthetic(owner, true)
        })
    }
    fn focused_app(&self) -> Result<ElementData> {
        self.call(|s| {
            let hwnd = unsafe { GetForegroundWindow() };
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            let owner = s
                .windows(Some(pid))?
                .into_iter()
                .next()
                .ok_or_else(|| Error::selector_not_matched("foreground IA2 application"))?;
            s.synthetic(owner, true)
        })
    }
    fn get_parent(&self, e: &ElementData) -> Result<Option<ElementData>> {
        let e = e.clone();
        self.call_for("get_parent", e.clone(), move |s| match s.get(&e)? {
            Entry::App(_) => Ok(None),
            Entry::Window(n) => Ok(Some(s.synthetic(n.owner, true)?)),
            Entry::Node(n) => {
                if n.root_instance {
                    return Ok(Some(s.synthetic(n.owner,false)?));
                }
                let object=if n.child!=0 {n.object.clone()} else {unsafe{n.object.accParent()}.map_err(native)?.cast().map_err(native)?};
                let mut parent=Node::new(object,0,n.owner,0)?;
                if let Some((handle,_))=s.entries.iter().find(|(_,entry)|matches!(entry,Entry::Window(root)if parent.identity.is_some()&&root.identity==parent.identity&&root.owner.hwnd==parent.owner.hwnd)){
                    parent.root_instance=true;parent.parent = *handle;
                }
                Ok(Some(s.snapshot(parent)?))
            }

        })
    }
    fn find_elements_group(
        &self,
        root: &ElementData,
        group: &xa11y_core::SelectorGroup,
        limit: Option<usize>,
        max_depth: Option<u32>,
    ) -> Result<Vec<ElementData>> {
        let root = root.clone();
        let group = group.clone();
        self.call(move |s| {
            let state = std::cell::RefCell::new(s);
            let visited = std::cell::Cell::new(0usize);
            let depths = std::cell::RefCell::new(HashMap::from([(root.handle, 0u32)]));
            xa11y_core::selector::find_elements_in_tree_group(
                |parent| {
                    let depth = parent
                        .and_then(|p| depths.borrow().get(&p.handle).copied())
                        .unwrap_or(0);
                    let children = state.borrow_mut().children(parent.cloned())?;
                    if depth >= 32 && !children.is_empty() {
                        return Err(Error::Unsupported {
                            feature: "IA2 tree walk exceeds depth 32".into(),
                        });
                    }
                    visited.set(visited.get() + children.len());
                    if visited.get() > LIMIT {
                        return Err(Error::Unsupported {
                            feature: format!("IA2 tree walk exceeds {LIMIT} nodes"),
                        });
                    }
                    for child in &children {
                        depths.borrow_mut().insert(child.handle, depth + 1);
                    }
                    Ok(children)
                },
                Some(&root),
                &group,
                limit,
                max_depth,
            )
        })
    }
    fn list_shell_surfaces(
        &self,
    ) -> Result<Vec<(xa11y_core::shell::ShellSurfaceKind, ElementData)>> {
        Err(Error::Unsupported {
            feature: "shell surface discovery is unavailable on the explicit IA2 document source"
                .into(),
        })
    }
    fn press(&self, e: &ElementData) -> Result<()> {
        let e = e.clone();
        self.call_for("press", e.clone(), move |s| {
            let n = s.actionable(&e)?;
            if bstr(unsafe { n.object.get_accDefaultAction(&n.child.into()) })?.is_none() {
                return Err(unsupported(&e, "press"));
            }
            s.before_mutation()?;
            unsafe { n.object.accDoDefaultAction(&n.child.into()) }.map_err(native)
        })
    }
    fn focus(&self, e: &ElementData) -> Result<()> {
        let e = e.clone();
        self.call_for("focus", e.clone(), move |s| {
            let n = s.actionable(&e)?;
            s.before_mutation()?;
            unsafe { n.object.accSelect(1, &n.child.into()) }.map_err(native)
        })
    }
    fn select(&self, e: &ElementData) -> Result<()> {
        let e = e.clone();
        self.call_for("select", e.clone(), move |s| {
            let n = s.actionable(&e)?;
            s.before_mutation()?;
            unsafe { n.object.accSelect(2, &n.child.into()) }.map_err(native)
        })
    }
    fn set_value(&self, e: &ElementData, value: &str) -> Result<()> {
        if value.encode_utf16().take(1024 * 1024 + 1).count() > 1024 * 1024 {
            return Err(Error::Unsupported {
                feature: "IA2 value write exceeds 1 Mi UTF-16 units".into(),
            });
        }
        let e = e.clone();
        let value = value.to_owned();
        self.call_for("set_value", e.clone(), move |s| {
            let n = s.actionable(&e)?;
            let value = BSTR::from(value);
            s.before_mutation()?;
            unsafe { n.object.put_accValue(&n.child.into(), &value) }.map_err(native)
        })
    }
    fn set_text_selection(&self, e: &ElementData, start: u32, end: u32) -> Result<()> {
        let e = e.clone();
        self.call_for("set_text_selection", e.clone(), move |s| {
            s.set_selection(&e, TextRange { start, end })
        })
    }
    fn perform_action(&self, e: &ElementData, action: &str) -> Result<()> {
        match action {
            "press" => self.press(e),
            "focus" => self.focus(e),
            "select" => self.select(e),
            _ => Err(unsupported(e, action)),
        }
    }
    fn text_length(&self, e: &ElementData) -> Result<u32> {
        let e = e.clone();
        self.call_for("text_length", e.clone(), move |s| s.text_length(&e))
    }
    fn text_caret(&self, e: &ElementData) -> Result<Option<u32>> {
        let e = e.clone();
        self.call_for("text_caret", e.clone(), move |s| s.text_caret(&e))
    }
    fn text_selections(&self, e: &ElementData) -> Result<Vec<TextRange>> {
        let e = e.clone();
        self.call_for("text_selections", e.clone(), move |s| s.text_selections(&e))
    }
    fn text_read(&self, e: &ElementData, r: TextRange) -> Result<String> {
        let e = e.clone();
        self.call_for("text_read", e.clone(), move |s| s.text_read(&e, r))
    }
    fn text_object_at(&self, e: &ElementData, offset: u32) -> Result<Option<ElementData>> {
        let e = e.clone();
        self.call_for("text_object_at", e.clone(), move |s| {
            let node = s.node(&e)?;
            let h: AccessibleHypertext = node.interface(&e, "text_object_at")?;
            let mut index = -1;
            unsafe {
                (h.vtable().hyperlinkIndex)(
                    h.as_raw(),
                    i32::try_from(offset).map_err(|_| platform("IA2 text offset exceeds LONG"))?,
                    &mut index,
                )
                .ok()
                .map_err(native)?;
            }
            if index == -1 {
                return Ok(None);
            }
            let mut count = 0;
            unsafe {
                (h.vtable().nHyperlinks)(h.as_raw(), &mut count)
                    .ok()
                    .map_err(native)?;
            }
            if index < 0 || index >= count {
                return Err(platform("IA2 returned an invalid hyperlink index"));
            }
            let mut raw = std::ptr::null_mut();
            let hr = unsafe { (h.vtable().hyperlink)(h.as_raw(), index, &mut raw) };
            let link = if raw.is_null() {
                None
            } else {
                Some(unsafe { AccessibleHyperlink::from_raw(raw) })
            };
            hr.ok().map_err(native)?;
            let object = link
                .ok_or_else(|| platform("IA2 returned a null hyperlink"))?
                .cast()
                .map_err(native)?;
            Ok(Some(
                s.snapshot(Node::new(object, 0, node.owner, e.handle)?)?,
            ))
        })
    }
    fn text_range_at(&self, e: &ElementData, offset: u32, unit: TextUnit) -> Result<TextRange> {
        let e = e.clone();
        self.call_for("text_range_at", e.clone(), move |s| {
            s.text_range_at(&e, offset, unit)
        })
    }
    fn text_attributes_at(&self, e: &ElementData, offset: u32) -> Result<TextRun> {
        let e = e.clone();
        self.call_for("text_attributes_at", e.clone(), move |s| {
            s.text_attributes(&e, offset)
        })
    }
    fn text_range_bounds(&self, e: &ElementData, r: TextRange) -> Result<Option<Rect>> {
        let e = e.clone();
        self.call_for("text_range_bounds", e.clone(), move |s| {
            s.text_bounds(&e, r)
        })
    }
    fn table_cell_at(&self, e: &ElementData, row: u32, column: u32) -> Result<Option<ElementData>> {
        let e = e.clone();
        self.call_for("table_cell_at", e.clone(), move |s| {
            s.cell_at(&e, row, column)
        })
    }
    fn table_headers(&self, e: &ElementData, kind: HeaderKind) -> Result<Vec<ElementData>> {
        let e = e.clone();
        self.call_for("table_headers", e.clone(), move |s| {
            s.table_headers(&e, kind)
        })
    }
    fn cell_headers(&self, e: &ElementData, kind: HeaderKind) -> Result<Vec<ElementData>> {
        let e = e.clone();
        self.call_for("cell_headers", e.clone(), move |s| s.cell_headers(&e, kind))
    }
    fn selected_items(&self, e: &ElementData) -> Result<Vec<ElementData>> {
        let e = e.clone();
        self.call_for("selected_items", e.clone(), move |s| s.selected(&e))
    }
    fn subscribe(&self, e: &ElementData) -> Result<Subscription> {
        Err(unsupported(e, "IA2 events"))
    }
    fn blur(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "blur"))
    }
    fn toggle(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "toggle"))
    }
    fn expand(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "expand"))
    }
    fn collapse(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "collapse"))
    }
    fn show_menu(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "show_menu"))
    }
    fn increment(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "increment"))
    }
    fn decrement(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "decrement"))
    }
    fn scroll_into_view(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "scroll_into_view"))
    }
    fn activate(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "activate"))
    }
    fn minimize(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "minimize"))
    }
    fn maximize(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "maximize"))
    }
    fn enter_fullscreen(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "enter_fullscreen"))
    }
    fn restore(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "restore"))
    }
    fn close(&self, e: &ElementData) -> Result<()> {
        Err(unsupported(e, "close"))
    }
    fn type_text(&self, e: &ElementData, _text: &str) -> Result<()> {
        Err(unsupported(e, "type_text"))
    }
    fn set_numeric_value(&self, e: &ElementData, _value: f64) -> Result<()> {
        Err(unsupported(e, "set_numeric_value"))
    }
    fn move_to(&self, e: &ElementData, _x: i32, _y: i32) -> Result<()> {
        Err(unsupported(e, "move_to"))
    }
    fn resize_to(&self, e: &ElementData, _width: u32, _height: u32) -> Result<()> {
        Err(unsupported(e, "resize_to"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    #[test]
    fn msaa_role_strings_are_answers_not_errors() {
        assert_eq!(msaa_role(&VARIANT::from(0x2a)).unwrap(), MsaaRole::Id(0x2a));
        assert_eq!(
            msaa_role(&VARIANT::from(BSTR::from("paragraph"))).unwrap(),
            MsaaRole::Name("paragraph".into())
        );
        assert!(msaa_role(&VARIANT::from(true)).is_err());
    }
    #[test]
    fn validation_that_finishes_after_deadline_cannot_begin_a_write() {
        let mut state = State::new();
        state.deadline = Some(Instant::now() - Duration::from_millis(1));
        assert!(state.before_mutation().is_err());
        assert!(!state.dispatched);
        state.deadline = Some(Instant::now() + Duration::from_secs(1));
        state.before_mutation().expect("live deadline");
        assert!(state.dispatched);
    }
    #[test]
    fn a_native_hresult_alone_never_asserts_an_action_was_not_delivered() {
        for code in [0x80004005u32, 0x80010108, 0x800401fd, 0x800706ba] {
            let error = native(windows::core::Error::from_hresult(windows::core::HRESULT(
                code as i32,
            )));
            assert!(matches!(error,Error::Platform{code:actual,..}if actual==code as i32 as i64));
            assert_eq!(error.delivery(), xa11y_core::error::Delivery::Unknown);
        }
    }
    /// A worker thread serving `capacity` queued requests, held before its
    /// first request until the returned sender is used or dropped.
    fn test_worker(
        capacity: usize,
    ) -> (
        Ia2Provider,
        mpsc::SyncSender<()>,
        std::thread::JoinHandle<()>,
    ) {
        let (sender, receiver) = mpsc::sync_channel::<Job>(capacity);
        let (release, paused) = mpsc::sync_channel(1);
        let (id_tx, id_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            id_tx
                .send(unsafe { GetCurrentThreadId() })
                .expect("thread id");
            // Released, or the test dropped the sender: either way, serve.
            let _ = paused.recv();
            let mut state = State::new();
            serve(receiver, &mut state);
        });
        let p = Ia2Provider {
            worker: Arc::new(Worker::new(sender, id_rx.recv().expect("thread id"))),
        };
        (p, release, thread)
    }
    #[test]
    fn an_expired_queued_request_never_runs_and_the_worker_stays_healthy() {
        let (p, release, thread) = test_worker(32);
        let delivered = Arc::new(AtomicBool::new(false));
        let action = Arc::clone(&delivered);
        let error = p
            .call_with_timeout(Duration::from_millis(10), move |_| {
                action.store(true, Ordering::Release);
                Ok(())
            })
            .expect_err("queued request deadline");
        assert!(matches!(error, Error::Timeout { .. }), "got {error:?}");
        assert_eq!(error.delivery(), xa11y_core::error::Delivery::NotDelivered);
        assert!(
            !p.worker.check_hung(),
            "a request that never started is not a stalled worker"
        );
        release.send(()).expect("release worker");
        p.call(|_| Ok(()))
            .expect("the worker still serves requests");
        assert!(
            !delivered.load(Ordering::Acquire),
            "the worker skipped the cancelled request"
        );
        drop(p);
        thread.join().expect("worker exit");
    }
    #[test]
    fn a_request_stopped_by_its_budget_is_a_timeout_not_a_stall() {
        let (p, release, thread) = test_worker(32);
        drop(release);
        let error = p
            .call_with_timeout::<()>(Duration::from_millis(50), |state| loop {
                // Each iteration stands for one native operation.
                state.check_deadline()?;
                std::thread::sleep(Duration::from_millis(5));
            })
            .expect_err("the budget stops the request");
        assert!(matches!(error, Error::Timeout { .. }), "got {error:?}");
        assert_eq!(error.delivery(), xa11y_core::error::Delivery::NotDelivered);
        assert!(
            !p.worker.check_hung(),
            "a cooperative stop is not a stalled worker"
        );
        p.call(|_| Ok(()))
            .expect("the worker still serves requests");
        drop(p);
        thread.join().expect("worker exit");
    }
    #[test]
    fn a_native_call_hung_past_its_budget_disables_the_worker() {
        let (p, release, thread) = test_worker(32);
        drop(release);
        let error = p
            .call_with_timeout(Duration::from_millis(50), |_| {
                // A native call that ignores the deadline.
                std::thread::sleep(Duration::from_millis(800));
                Ok(())
            })
            .expect_err("the hung call outlives its budget");
        assert!(matches!(error, Error::Platform { .. }), "got {error:?}");
        assert_eq!(error.delivery(), xa11y_core::error::Delivery::Unknown);
        assert!(p.worker.check_hung());
        let started = Instant::now();
        assert!(
            p.call(|_| Ok(())).is_err(),
            "no replacement worker while a native call is hung"
        );
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "a disabled worker fails at once"
        );
        // Once the hung call returns, the worker serves requests again.
        std::thread::sleep(Duration::from_millis(800));
        assert!(!p.worker.check_hung());
        p.call(|_| Ok(()))
            .expect("the worker serves requests once the slow call returned");
        drop(p);
        thread.join().expect("worker exit");
    }
    #[test]
    fn a_panicking_request_is_an_error_and_the_worker_survives() {
        let (p, release, thread) = test_worker(32);
        drop(release);
        let error = p
            .call_with_timeout::<()>(Duration::from_millis(50), |_| panic!("boom"))
            .expect_err("the panic is reported");
        assert!(
            matches!(&error, Error::Platform { message, .. } if message.contains("panicked") && message.contains("boom")),
            "got {error:?}"
        );
        assert_eq!(error.delivery(), xa11y_core::error::Delivery::Unknown);
        // Long past the panicked request's budget, it is not a hung call.
        std::thread::sleep(Duration::from_millis(300));
        assert!(!p.worker.check_hung());
        assert_eq!(p.call(|_| Ok(7)).expect("the worker still serves"), 7);
        drop(p);
        thread.join().expect("worker exit");
    }
    #[test]
    fn a_stale_running_record_never_disables_the_worker() {
        let (p, release, thread) = test_worker(32);
        drop(release);
        p.call_with_timeout(Duration::from_millis(50), |_| Ok(()))
            .expect("fast call");
        // Long after that request's budget, nothing is running.
        std::thread::sleep(Duration::from_millis(300));
        assert!(!p.worker.check_hung());
        p.call(|_| Ok(()))
            .expect("the worker still serves requests");
        drop(p);
        thread.join().expect("worker exit");
    }
    #[test]
    fn a_real_error_that_finishes_after_the_deadline_is_not_a_timeout() {
        let (p, release, thread) = test_worker(32);
        drop(release);
        let error = p
            .call_with_timeout::<()>(Duration::from_millis(50), |state| {
                // Work that ends just past its budget with an error of its
                // own; the classification must not ask `check_deadline`.
                std::thread::sleep(Duration::from_millis(80));
                state.classify_result(
                    &ElementData::for_role(Role::Unknown),
                    Err(Error::platform(0x80004005u32 as i32 as i64, "real")),
                )
            })
            .expect_err("the work failed");
        assert!(
            matches!(&error, Error::Platform { message, .. } if message.contains("real")),
            "got {error:?}"
        );
        drop(p);
        thread.join().expect("worker exit");
    }
    #[test]
    fn a_budget_runs_from_dispatch_not_from_enqueue() {
        let (p, release, thread) = test_worker(32);
        let p = Arc::new(p);
        let first = {
            let p = Arc::clone(&p);
            std::thread::spawn(move || {
                p.call_with_timeout(Duration::from_millis(400), |_| {
                    std::thread::sleep(Duration::from_millis(250));
                    Ok(())
                })
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        release.send(()).expect("release worker");
        // Queued behind 250 ms of work, then 250 ms of its own: past 400 ms
        // from enqueue, within 400 ms of starting.
        p.call_with_timeout(Duration::from_millis(400), |_| {
            std::thread::sleep(Duration::from_millis(250));
            Ok(())
        })
        .expect("the second request's budget starts when it runs");
        first
            .join()
            .expect("first caller")
            .expect("first request finished within budget");
        assert!(!p.worker.check_hung());
        drop(p);
        thread.join().expect("worker exit");
    }
    #[test]
    fn a_full_queue_waits_for_capacity_within_the_callers_time() {
        let (p, release, thread) = test_worker(1);
        let p = Arc::new(p);
        let first = {
            let p = Arc::clone(&p);
            std::thread::spawn(move || p.call(|_| Ok(1)))
        };
        std::thread::sleep(Duration::from_millis(20));
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            release.send(()).expect("release worker");
        });
        assert_eq!(p.call(|_| Ok(2)).expect("queued once capacity freed"), 2);
        assert_eq!(first.join().expect("first caller").expect("first"), 1);
        releaser.join().expect("releaser");
        drop(p);
        thread.join().expect("worker exit");
    }
    #[test]
    fn a_callers_deadline_cuts_the_wait_without_disabling_the_worker() {
        let (p, release, thread) = test_worker(32);
        drop(release);
        let deadline = xa11y_core::Deadline::after(Duration::from_millis(50));
        let probe = deadline.clone();
        let started = Instant::now();
        let error = deadline
            .scope(|| {
                p.call(|_| {
                    std::thread::sleep(Duration::from_millis(300));
                    Ok(())
                })
            })
            .expect_err("the caller's deadline cuts the wait");
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "waited past the deadline"
        );
        assert!(matches!(error, Error::Timeout { .. }), "got {error:?}");
        assert_eq!(probe.cut_short(), Some(PENDING));
        assert!(
            !p.worker.check_hung(),
            "a caller's deadline is not a stalled worker"
        );
        p.call(|_| Ok(()))
            .expect("the worker still serves requests");
        drop(p);
        thread.join().expect("worker stopped");
    }
    #[test]
    fn the_registry_is_bounded_like_the_uia_handle_table_and_never_expires_by_age() {
        let mut state = State::new();
        let owner = Owner {
            hwnd: 0,
            pid: 0,
            generation: 0,
        };
        let first = state.retain(Entry::App(owner.clone()));
        for _ in 0..MAX_CACHED_ELEMENTS {
            state.retain(Entry::App(owner.clone()));
        }
        assert_eq!(state.entries.len(), MAX_CACHED_ELEMENTS);
        let mut e = ElementData::for_role(Role::Application);
        e.handle = first;
        e.raw.insert("accessibility_backend".into(), "ia2".into());
        assert!(matches!(
            state.get(&e),
            Err(Error::ElementStale {
                reason: xa11y_core::StaleReason::Evicted,
                ..
            })
        ));
    }
}
