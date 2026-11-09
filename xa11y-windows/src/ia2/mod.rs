//! Explicit MSAA/IA2 source. UIA is never consulted by this provider.
pub(super) mod abi;
mod rich;
use abi::*;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
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
use windows::Win32::System::Variant::{VARIANT, VT_DISPATCH, VT_I4};
use windows::Win32::UI::Accessibility::{
    AccessibleChildren, AccessibleObjectFromWindow, IAccessible,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    IsWindow, IsWindowVisible, GA_ROOT,
};
use xa11y_core::event_provider::Subscription;
use xa11y_core::table::*;
use xa11y_core::text_api::*;
use xa11y_core::{
    ElementData, ElementParts, Error, HiddenReason, Provider, Rect, Result, Role, StateParts,
    Toggled,
};

const LIMIT: usize = 4096;
const REGISTRY_LIMIT: usize = 8192;
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
const RETENTION: Duration = Duration::from_secs(300);
fn platform(message: impl Into<String>) -> Error {
    Error::Platform {
        code: -1,
        message: message.into(),
    }
}
fn native(error: windows::core::Error) -> Error {
    Error::Platform {
        code: error.code().0 as i64,
        message: format!("MSAA/IA2: {error}"),
    }
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
fn bstr(v: windows::core::Result<BSTR>) -> Result<Option<String>> {
    let v = match v {
        Ok(v) => v,
        Err(e) if matches!(e.code().0, -2147352573 | -2147467263) => return Ok(None),
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
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    let h =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(native)?;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let result = unsafe { GetProcessTimes(h, &mut creation, &mut exit, &mut kernel, &mut user) };
    let close = unsafe { CloseHandle(h) };
    result.map_err(native)?;
    close.map_err(native)?;
    if exit.dwLowDateTime != 0 || exit.dwHighDateTime != 0 {
        return Err(stale());
    }
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
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
        if let Error::Platform { code, message } = error {
            if matches!(code as i32 as u32, 0x80010108 | 0x800401fd | 0x800706ba) {
                return stale();
            }
            if code == 0x80004005u32 as i32 as i64 {
                return match self.ensure_attached() {
                    Err(Error::ElementStale { .. }) => stale(),
                    Ok(()) => Error::Platform { code, message },
                    Err(proof) => Error::Platform {
                        code,
                        message: format!("{message}; attachment check also failed: {proof}"),
                    },
                };
            }
            Error::Platform { code, message }
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
        Err(error) => Err(Error::Platform {
            code: error.code().0 as i64,
            message: format!("resolving retained IA2 uniqueID {id}: {error}"),
        }),
    }
}
#[derive(Clone)]
enum Entry {
    App(Owner),
    Window(Node),
    Node(Node),
}
struct State {
    entries: HashMap<u64, (Instant, Entry)>,
    order: VecDeque<u64>,
    next: u64,
    dispatched: bool,
    deadline: Option<Instant>,
}
impl State {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            next: 1,
            dispatched: false,
            deadline: None,
        }
    }
    fn check_deadline(&self) -> Result<()> {
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
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
        while self.entries.len() >= REGISTRY_LIMIT {
            if let Some(id) = self.order.pop_front() {
                self.entries.remove(&id);
            } else {
                break;
            }
        }
        let id = (1u64 << 62) | self.next;
        self.next += 1;
        self.entries.insert(id, (Instant::now(), entry));
        self.order.push_back(id);
        id
    }
    fn get(&mut self, e: &ElementData) -> Result<Entry> {
        if e.raw.get("accessibility_backend").and_then(|v| v.as_str()) != Some("ia2") {
            return Err(stale());
        }
        let (time, entry) = self.entries.get(&e.handle).ok_or_else(stale)?;
        if time.elapsed() > RETENTION {
            self.entries.remove(&e.handle);
            self.order.retain(|id| *id != e.handle);
            return Err(stale());
        }
        let entry = entry.clone();
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
        // its HRESULT into a NotDelivered stale error after dispatch.
        if self.dispatched || self.check_deadline().is_err() {
            return result;
        }
        result.map_err(|error| match self.entries.get(&e.handle) {
            Some((_, Entry::Node(node) | Entry::Window(node))) => node.classify_error(error),
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
        let name = String::from_utf16_lossy(&title[..n.max(0) as usize]);
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
        let role_id = integer(&unsafe { node.object.get_accRole(&child) }.map_err(native)?)?;
        let role = crate::uia::map_msaa_role(role_id).unwrap_or(Role::Unknown);
        let states = integer(&unsafe { node.object.get_accState(&child) }.map_err(native)?)?;
        let name = bstr(unsafe { node.object.get_accName(&child) })?;
        let value = bstr(unsafe { node.object.get_accValue(&child) })?;
        let description = bstr(unsafe { node.object.get_accDescription(&child) })?;
        let action = bstr(unsafe { node.object.get_accDefaultAction(&child) })?;
        let dpi = crate::dpi::DpiScope::enter()?;
        let bounds = crate::coords::msaa_bounds(&dpi, &node.object, &child).map_err(native)?;
        let mut raw = HashMap::new();
        raw.insert("msaa_role".into(), role_id.into());
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
        Ok(data(
            Snapshot {
                role,
                name,
                value,
                description,
                bounds: Some(bounds),
                raw,
                table,
                cell,
                actions,
            },
            states,
            &owner,
            handle,
        ))
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
struct Worker {
    sender: mpsc::SyncSender<Job>,
    stalled: Arc<AtomicBool>,
    thread: u32,
}
enum WorkerInit {
    Vacant,
    Ready(Arc<Worker>),
    Failed,
}
static WORKER: Mutex<WorkerInit> = Mutex::new(WorkerInit::Vacant);
/// Explicit MSAA/IA2 provider. Select before the umbrella provider's first use.
/// Native interfaces remain confined to one MTA worker. Requests and retained
/// handles are bounded. A timed-out COM call poisons this process-wide worker;
/// no replacement threads are spawned and subsequent calls fail explicitly.
/// Restart the host after a timeout. COM cancellation is best effort, because
/// an in-process/misbehaving server may not honor it.
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
                    while let Ok(job) = rx.recv() {
                        job(&mut state);
                    }
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
        let worker = Arc::new(Worker {
            sender: tx,
            stalled: Arc::new(AtomicBool::new(false)),
            thread,
        });
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
                Error::Platform { code, message } => Error::Platform {
                    code,
                    message: format!(
                        "IA2 {operation} on {} {name:?} (pid {:?}, handle {}): {message}",
                        context.role, context.pid, context.handle
                    ),
                },
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
    /// The two bounds mean different things. `timeout` is the stall
    /// detector: a request still running when it expires disables the
    /// worker, because a hung native call cannot be taken back. The caller's
    /// deadline is the caller choosing to stop waiting: the worker stops at
    /// its next native operation (`State::check_deadline`), a request still
    /// queued never starts, and the worker stays usable.
    fn call_with_timeout<T: Send + 'static>(
        &self,
        timeout: Duration,
        f: impl FnOnce(&mut State) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        if self.worker.stalled.load(Ordering::Acquire) {
            return Err(platform(
                "IA2 worker timed out previously; restart the host before using IA2 again",
            ));
        }
        let bound = xa11y_core::call_timeout(timeout, || PENDING.to_string())?;
        let caller = bound.deadline.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let expires = Instant::now() + timeout;
        let stop_at = caller.as_ref().map_or(expires, |d| d.at().min(expires));
        let stalled = Arc::clone(&self.worker.stalled);
        self.worker
            .sender
            .try_send(Box::new(move |state| {
                let result = if stalled.load(Ordering::Acquire) || Instant::now() >= stop_at {
                    Err(platform("IA2 request expired before dispatch"))
                } else {
                    state.dispatched = false;
                    state.deadline=Some(stop_at);
                    let result=f(state);
                    state.deadline=None;
                    if Instant::now()>=expires{
                        stalled.store(true,Ordering::Release);
                        Err(platform("IA2 request finished after its deadline; native action delivery may be unknown; worker disabled"))
                    }else{result}
                };
                // The caller may have abandoned a timed-out read/action. Never retry it.
                let _ = tx.send(result);
            }))
            .map_err(|e| platform(format!("IA2 request queue is full or disconnected: {e}")))?;
        match rx.recv_timeout(bound.duration) {
            Ok(result) => {
                if result.is_err() && caller.as_ref().is_some_and(|d| d.is_expired()) {
                    // The worker stopped at the caller's deadline. Recording
                    // the cut is the point; core builds the error the caller
                    // sees from it.
                    let _ = bound.expired(PENDING);
                }
                result
            }
            Err(_) if bound.is_deadline() => {
                // The caller stopped waiting, not the worker: it stops at its
                // next native operation, and stays usable.
                Err(bound
                    .expired(PENDING)
                    .unwrap_or_else(|| platform("IA2 request outlived the caller's deadline")))
            }
            Err(error) => {
                self.worker.stalled.store(true, Ordering::Release);
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
                Err(platform(format!("IA2 native request did not finish within {timeout:?} ({error}); an in-flight action may have been delivered; worker disabled")))
            }
        }
    }
}

/// What a deadline diagnosis names as pending for an IA2 request. The
/// request runs on the worker thread, so the caller knows only that it was
/// one; `call_for` adds the element to the error message.
const PENDING: &str = "an MSAA/IA2 request on the IA2 worker thread";

impl Provider for Ia2Provider {
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
                if let Some((handle,_))=s.entries.iter().find(|(_,(_,entry))|matches!(entry,Entry::Window(root)if parent.identity.is_some()&&root.identity==parent.identity&&root.owner.hwnd==parent.owner.hwnd)){
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
    #[test]
    fn an_expired_queued_action_never_runs_and_the_worker_stays_disabled() {
        let (sender, receiver) = mpsc::sync_channel::<Job>(32);
        let (release, paused) = mpsc::sync_channel(1);
        let (id_tx, id_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            id_tx
                .send(unsafe { GetCurrentThreadId() })
                .expect("thread id");
            paused.recv().expect("release worker");
            let mut state = State::new();
            while let Ok(job) = receiver.recv() {
                job(&mut state);
            }
            done_tx.send(()).expect("worker stopped");
        });
        let p = Ia2Provider {
            worker: Arc::new(Worker {
                sender,
                thread: id_rx.recv().expect("thread id"),
                stalled: Arc::new(AtomicBool::new(false)),
            }),
        };
        let delivered = Arc::new(AtomicBool::new(false));
        let action = Arc::clone(&delivered);
        let error = p
            .call_with_timeout(Duration::from_millis(10), move |_| {
                action.store(true, Ordering::Release);
                Ok(())
            })
            .expect_err("queued request deadline");
        assert!(matches!(error, Error::Platform { .. }));
        assert_eq!(error.delivery(), xa11y_core::error::Delivery::Unknown);
        assert!(
            p.call(|_| Ok(())).is_err(),
            "no replacement worker after timeout"
        );
        release.send(()).expect("release worker");
        drop(p);
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker drains expired request and exits");
        thread.join().expect("worker exit");
        assert!(!delivered.load(Ordering::Acquire));
    }
    #[test]
    fn a_callers_deadline_cuts_the_wait_without_disabling_the_worker() {
        let (sender, receiver) = mpsc::sync_channel::<Job>(32);
        let (id_tx, id_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            id_tx
                .send(unsafe { GetCurrentThreadId() })
                .expect("thread id");
            let mut state = State::new();
            while let Ok(job) = receiver.recv() {
                job(&mut state);
            }
        });
        let p = Ia2Provider {
            worker: Arc::new(Worker {
                sender,
                thread: id_rx.recv().expect("thread id"),
                stalled: Arc::new(AtomicBool::new(false)),
            }),
        };
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
            !p.worker.stalled.load(Ordering::Acquire),
            "a caller's deadline is not a stalled worker"
        );
        p.call(|_| Ok(()))
            .expect("the worker still serves requests");
        drop(p);
        thread.join().expect("worker stopped");
    }
    #[test]
    fn expired_handles_do_not_grow_the_registry_order_queue() {
        let mut state = State::new();
        let owner = Owner {
            hwnd: 0,
            pid: 0,
            generation: 0,
        };
        for _ in 0..10_000 {
            let handle = state.retain(Entry::App(owner.clone()));
            state.entries.get_mut(&handle).expect("retained entry").0 =
                Instant::now() - RETENTION - Duration::from_secs(1);
            let mut e = ElementData::for_role(Role::Application);
            e.handle = handle;
            e.raw.insert("accessibility_backend".into(), "ia2".into());
            assert!(matches!(state.get(&e), Err(Error::ElementStale { .. })));
            assert!(state.entries.is_empty());
            assert!(state.order.is_empty());
        }
    }
}
