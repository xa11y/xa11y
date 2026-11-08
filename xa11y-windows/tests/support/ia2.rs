//! Private, executable test support for #483; not an accessibility provider.
//!
//! ABI source: LinuxA11y/IAccessible2 c9ae003d9c85eb707716928de97e055f5b77189c,
//! api/Accessible2.idl. The inherited prefix is the SDK's complete IAccessible
//! vtable (IUnknown + IDispatch + MSAA), not merely an IUnknown prefix.
#[cfg(test)]
use std::ffi::c_void;
use std::time::{Duration, Instant};
use windows::core::{IUnknown, Interface, BSTR};
use windows::Win32::Foundation::{CloseHandle, E_NOINTERFACE, FILETIME, HWND};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, IDispatch, IServiceProvider, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::{VARIANT, VT_DISPATCH, VT_I4};
use windows::Win32::UI::Accessibility::{
    AccessibleChildren, AccessibleObjectFromWindow, IAccessible,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetWindowThreadProcessId, IsWindow, GA_ROOT,
};

#[path = "../../src/ia2/abi.rs"]
mod abi;
use abi::Accessible2;
#[cfg(test)]
use abi::Accessible2Vtable;
#[cfg(test)]
use windows::Win32::UI::Accessibility::IAccessible_Vtbl;

pub type ProbeResult<T> = Result<T, Box<dyn std::error::Error>>;

pub struct Apartment;
impl Apartment {
    pub fn new() -> ProbeResult<Self> {
        // This executable owns its thread. A changed apartment is a real error.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

fn query_ia2(acc: &IAccessible) -> ProbeResult<Option<Accessible2>> {
    let service: IServiceProvider = match acc.cast() {
        Ok(service) => service,
        Err(error) if error.code() == E_NOINTERFACE => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    match unsafe { service.QueryService::<Accessible2>(&IAccessible::IID) } {
        Ok(ia2) => Ok(Some(ia2)),
        Err(error) if error.code() == E_NOINTERFACE => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn integer(value: &VARIANT) -> ProbeResult<i32> {
    if value.vt() != VT_I4 {
        return Err(format!("expected VT_I4, got {:?}", value.vt()).into());
    }
    Ok(unsafe { value.Anonymous.Anonymous.Anonymous.lVal })
}

#[derive(Clone)]
struct Node {
    object: IAccessible,
    child: i32,
}
impl Node {
    fn role(&self) -> ProbeResult<i32> {
        integer(&unsafe { self.object.get_accRole(&self.child.into())? })
    }
    fn name(&self) -> ProbeResult<String> {
        let value = unsafe { self.object.get_accName(&self.child.into())? };
        if value.len() > 8192 {
            return Err("fixture node name exceeded 8192 UTF-16 units".into());
        }
        Ok(value.to_string())
    }
    fn state(&self) -> ProbeResult<i32> {
        integer(&unsafe { self.object.get_accState(&self.child.into())? })
    }
}

fn decode_children(
    parent: &IAccessible,
    values: &[VARIANT],
    obtained: i32,
) -> ProbeResult<Vec<Node>> {
    let obtained = usize::try_from(obtained)?;
    if obtained > values.len() {
        return Err("AccessibleChildren returned more entries than requested".into());
    }
    values[..obtained]
        .iter()
        .map(|value| match value.vt() {
            VT_DISPATCH => Ok(Node {
                object: IDispatch::try_from(value)?.cast()?,
                child: 0,
            }),
            VT_I4 => {
                let child = integer(value)?;
                if child <= 0 {
                    return Err(format!(
                        "invalid MSAA simple-child ID {child}; not an IA2 uniqueID"
                    )
                    .into());
                }
                Ok(Node {
                    object: parent.clone(),
                    child,
                })
            }
            other => Err(format!("unexpected AccessibleChildren variant {other:?}").into()),
        })
        .collect()
}

fn capture(root: &IAccessible) -> ProbeResult<Vec<Node>> {
    let mut pending = vec![(
        Node {
            object: root.clone(),
            child: 0,
        },
        0,
    )];
    let mut nodes = Vec::new();
    while let Some((node, depth)) = pending.pop() {
        if nodes.len() >= 2048 || depth > 32 {
            return Err("fixture walk exceeded 2048 nodes / depth 32".into());
        }
        if node.child == 0 {
            let count = usize::try_from(unsafe { node.object.accChildCount()? })?;
            if count > 2048 - nodes.len() - pending.len() - 1 {
                return Err(format!("unbounded remote child count {count}").into());
            }
            if count != 0 {
                let mut values: Vec<VARIANT> = (0..count).map(|_| VARIANT::default()).collect();
                let mut obtained = 0;
                // Returned variants own their contents. decode_children borrows them
                // and clones interface references before these variants are cleared.
                unsafe {
                    AccessibleChildren(&node.object, 0, &mut values, &mut obtained)?;
                }
                for child in decode_children(&node.object, &values, obtained)?
                    .into_iter()
                    .rev()
                {
                    pending.push((child, depth + 1));
                }
            }
        }
        nodes.push(node);
    }
    Ok(nodes)
}

fn check_window(hwnd: HWND, pid: u32) -> ProbeResult<()> {
    let mut actual = 0;
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err("fixture HWND was destroyed".into());
    }
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut actual));
    }
    if actual != pid {
        return Err(format!("fixture HWND changed owner: expected {pid}, got {actual}").into());
    }
    Ok(())
}

fn process_generation(pid: u32) -> ProbeResult<u64> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)? };
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let result =
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    let closed = unsafe { CloseHandle(handle) };
    result?;
    closed?;
    if exit.dwHighDateTime != 0 || exit.dwLowDateTime != 0 {
        return Err("fixture process exited".into());
    }
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

fn identity(ia2: &Accessible2) -> ProbeResult<(i32, HWND)> {
    let mut id = 0;
    let mut hwnd = HWND::default();
    unsafe {
        (ia2.vtable().unique_id)(ia2.as_raw(), &mut id).ok()?;
        (ia2.vtable().window_handle)(ia2.as_raw(), &mut hwnd).ok()?;
    }
    Ok((id, hwnd))
}

pub fn probe(hwnd: HWND, pid: u32, active: &str, inactive: &str) -> ProbeResult<serde_json::Value> {
    check_window(hwnd, pid)?;
    let generation = process_generation(pid)?;
    let mut raw = std::ptr::null_mut();
    unsafe {
        AccessibleObjectFromWindow(hwnd, 0xffff_fffc, &IAccessible::IID, &mut raw)?;
    }
    if raw.is_null() {
        return Err("AccessibleObjectFromWindow succeeded with a null interface".into());
    }
    let root = unsafe { IAccessible::from_raw(raw) };
    let nodes = capture(&root)?;
    let mut tabs = Vec::new();
    for node in &nodes {
        if node.role()? == 0x25 && node.name()?.contains(active) {
            tabs.push(node);
        }
    }
    if tabs.len() != 1 {
        return Err(format!("expected one fixture tab {active:?}, found {}", tabs.len()).into());
    }
    unsafe {
        tabs[0].object.accDoDefaultAction(&tabs[0].child.into())?;
    }
    let start = Instant::now();
    let (document, examined) = loop {
        check_window(hwnd, pid)?;
        let fresh = capture(&root)?;
        let mut documents = Vec::new();
        let mut inactive_names = Vec::new();
        for node in &fresh {
            // MSAA visible/offscreen are provider facts, not physical hit testing.
            if node.role()? == 0x0f && node.state()? & (0x8000 | 0x10000) == 0 {
                let name = node.name()?;
                if name.contains(inactive) {
                    inactive_names.push(name.chars().take(256).collect::<String>());
                }
                if name.contains(active) {
                    documents.push(node.clone());
                }
            }
        }
        if documents.len() == 1 && inactive_names.is_empty() {
            break (documents.remove(0), fresh.len());
        }
        if start.elapsed() > Duration::from_secs(5) {
            let mut observed = Vec::new();
            for node in &fresh {
                let role = node.role()?;
                let name = node.name()?;
                if role == 0x0f || name.contains(active) || name.contains(inactive) {
                    observed.push(format!(
                        "role={role:#x}, state={:#x}, name={:?}",
                        node.state()?,
                        name.chars().take(160).collect::<String>()
                    ));
                    if role == 0x0f && node.child == 0 {
                        let value = unsafe { node.object.get_accValue(&VARIANT::from(0))? };
                        observed.push(format!(
                            "document value={:?}, IA2={}, child count={}",
                            value.to_string().chars().take(160).collect::<String>(),
                            query_ia2(&node.object)?.is_some(),
                            unsafe { node.object.accChildCount()? }
                        ));
                    }
                    if role == 0x0f && node.child == 0 {
                        if let Some(native) = query_ia2(&node.object)? {
                            let native_acc: IAccessible = native.cast()?;
                            let mut attributes = BSTR::default();
                            unsafe {
                                (native.vtable().attributes)(native.as_raw(), &mut attributes)
                                    .ok()?;
                            }
                            observed.push(format!(
                                "IA2 identity={:?}, name={:?}, children={}, attributes={:?}",
                                identity(&native)?,
                                unsafe { native_acc.get_accName(&0.into())? }
                                    .to_string()
                                    .chars()
                                    .take(160)
                                    .collect::<String>(),
                                unsafe { native_acc.accChildCount()? },
                                attributes.to_string().chars().take(400).collect::<String>()
                            ));
                        }
                    }
                    if observed.len() >= 24 {
                        observed.truncate(24);
                        break;
                    }
                }
            }
            return Err(format!("expected one active document {active:?} and zero visible inactive documents; active count {}, inactive count {}, inactive names {:?}, elapsed {:?}; observed {:?}", documents.len(), inactive_names.len(), &inactive_names[..inactive_names.len().min(8)], start.elapsed(), observed).into());
        }
        // Only the declared publication condition is polled. Native failures propagate.
        std::thread::sleep(Duration::from_millis(50));
    };
    if document.child != 0 {
        return Err("browser document unexpectedly represented as simple MSAA child".into());
    }
    let ia2 = query_ia2(&document.object)?
        .ok_or("selected document does not expose IA2 via QueryService")?;
    let (id, event_window) = identity(&ia2)?;
    if id >= 0 {
        return Err(format!("expected Chromium signed negative uniqueID, got {id}").into());
    }
    // Chromium can expose a renderer child HWND with a different PID. Prove
    // its actual native root is this owned browser window; never equate the
    // renderer PID with the browser PID or match another window by title.
    if !unsafe { IsWindow(Some(event_window)) }.as_bool()
        || unsafe { GetAncestor(event_window, GA_ROOT) } != hwnd
    {
        return Err("IA2 event HWND is not rooted in the owned fixture window".into());
    }
    check_window(hwnd, pid)?;
    let mut event_pid = 0;
    unsafe {
        GetWindowThreadProcessId(event_window, Some(&mut event_pid));
    }
    let again = query_ia2(&document.object)?.ok_or("IA2 disappeared on repeat query")?;
    if identity(&again)? != (id, event_window) {
        return Err("same retained document changed IA2 identity".into());
    }
    let original_unknown: IUnknown = ia2.cast()?;
    let again_unknown: IUnknown = again.cast()?;
    if original_unknown != again_unknown {
        return Err("repeat QueryService changed canonical IUnknown identity".into());
    }
    // Chromium's documented negative-ID mechanism uses a signed VT_I4, not
    // a positive MSAA child ordinal. Do not abs/cast the value to u32.
    let resolved: IAccessible = unsafe { document.object.get_accChild(&id.into())? }.cast()?;
    let resolved_ia2 = query_ia2(&resolved)?.ok_or("negative ID lookup lost IA2")?;
    if identity(&resolved_ia2)? != (id, event_window)
        || resolved_ia2.cast::<IUnknown>()? != original_unknown
    {
        return Err("signed negative-ID round-trip changed document identity".into());
    }
    let fresh = capture(&root)?;
    let mut repeated = Vec::new();
    for node in fresh {
        if node.role()? == 0x0f
            && node.state()? & (0x8000 | 0x10000) == 0
            && node.name()?.contains(active)
        {
            repeated.push(node);
        }
    }
    if repeated.len() != 1 {
        return Err("fresh traversal no longer has exactly one active fixture document".into());
    }
    let fresh_ia2 = query_ia2(&repeated[0].object)?.ok_or("fresh capture lost IA2")?;
    if identity(&fresh_ia2)? != (id, event_window)
        || fresh_ia2.cast::<IUnknown>()? != original_unknown
    {
        return Err("fresh capture changed live document identity".into());
    }
    check_window(hwnd, pid)?;
    if process_generation(pid)? != generation {
        return Err("browser process generation changed during probe".into());
    }
    // The fixture report intentionally contains no general-purpose document tree.
    Ok(
        serde_json::json!({"active_marker":active,"inactive_marker":inactive,"document_name":document.name()?,"unique_id":id,"window_handle":event_window.0 as usize,"pid":pid,"event_pid":event_pid,"examined_nodes":examined,"canonical_identity_preserved":true,"repeat_capture_identity_preserved":true,"negative_id_resolved":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use windows::core::{implement, GUID, PCWSTR};
    use windows::Win32::Foundation::{E_ACCESSDENIED, E_NOTIMPL};
    use windows::Win32::System::Com::{
        IDispatch_Impl, IServiceProvider_Impl, ITypeInfo, DISPATCH_FLAGS, DISPPARAMS, EXCEPINFO,
    };
    use windows::Win32::UI::Accessibility::IAccessible_Impl;

    #[implement(IAccessible, IServiceProvider)]
    struct NativeObject {
        drops: Arc<AtomicUsize>,
        queries: Arc<AtomicUsize>,
        denied: bool,
    }
    impl Drop for NativeObject {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    // All methods not exercised by these ownership tests fail explicitly.
    macro_rules! unsupported {
        ($(fn $name:ident($($arg:ident : $ty:ty),*) -> $ret:ty;)*) => {$(
            fn $name(&self, $($arg:$ty),*) -> windows::core::Result<$ret> {
                $(let _ = $arg;)*
                Err(E_NOTIMPL.into())
            }
        )*};
    }
    impl IDispatch_Impl for NativeObject_Impl {
        unsupported! {
            fn GetTypeInfoCount() -> u32;
            fn GetTypeInfo(info:u32, locale:u32) -> ITypeInfo;
            fn GetIDsOfNames(iid:*const GUID, names:*const PCWSTR, count:u32, locale:u32, ids:*mut i32) -> ();
            fn Invoke(id:i32, iid:*const GUID, locale:u32, flags:DISPATCH_FLAGS, params:*const DISPPARAMS, value:*mut VARIANT, exception:*mut EXCEPINFO, argument:*mut u32) -> ();
        }
    }
    impl IAccessible_Impl for NativeObject_Impl {
        fn get_accName(&self, child: &VARIANT) -> windows::core::Result<BSTR> {
            // Goes through the real SDK-generated COM trampoline. The [in]
            // by-value VARIANT is borrowed, and the returned BSTR is a copy.
            BSTR::try_from(child)
        }
        unsupported! {
            fn accParent() -> IDispatch;
            fn accChildCount() -> i32;
            fn get_accChild(child:&VARIANT) -> IDispatch;
            fn get_accValue(child:&VARIANT) -> BSTR;
            fn get_accDescription(child:&VARIANT) -> BSTR;
            fn get_accRole(child:&VARIANT) -> VARIANT;
            fn get_accState(child:&VARIANT) -> VARIANT;
            fn get_accHelp(child:&VARIANT) -> BSTR;
            fn get_accHelpTopic(file:*mut BSTR, child:&VARIANT) -> i32;
            fn get_accKeyboardShortcut(child:&VARIANT) -> BSTR;
            fn accFocus() -> VARIANT;
            fn accSelection() -> VARIANT;
            fn get_accDefaultAction(child:&VARIANT) -> BSTR;
            fn accSelect(flags:i32, child:&VARIANT) -> ();
            fn accLocation(x:*mut i32, y:*mut i32, width:*mut i32, height:*mut i32, child:&VARIANT) -> ();
            fn accNavigate(direction:i32, child:&VARIANT) -> VARIANT;
            fn accHitTest(x:i32, y:i32) -> VARIANT;
            fn accDoDefaultAction(child:&VARIANT) -> ();
            fn put_accName(child:&VARIANT, name:&BSTR) -> ();
            fn put_accValue(child:&VARIANT, value:&BSTR) -> ();
        }
    }
    impl IServiceProvider_Impl for NativeObject_Impl {
        fn QueryService(
            &self,
            service: *const GUID,
            iid: *const GUID,
            output: *mut *mut c_void,
        ) -> windows::core::Result<()> {
            assert_eq!(unsafe { *service }, IAccessible::IID);
            assert_eq!(unsafe { *iid }, Accessible2::IID);
            unsafe {
                *output = std::ptr::null_mut();
            }
            self.queries.fetch_add(1, Ordering::SeqCst);
            Err(if self.denied {
                E_ACCESSDENIED
            } else {
                E_NOINTERFACE
            }
            .into())
        }
    }
    fn object(drops: &Arc<AtomicUsize>, queries: &Arc<AtomicUsize>, denied: bool) -> IAccessible {
        NativeObject {
            drops: drops.clone(),
            queries: queries.clone(),
            denied,
        }
        .into()
    }

    #[test]
    fn ia2_abi_keeps_complete_msaa_prefix_and_signed_id() {
        assert_eq!(
            std::mem::offset_of!(Accessible2Vtable, n_relations),
            std::mem::size_of::<IAccessible_Vtbl>()
        );
        assert_eq!(
            std::mem::offset_of!(Accessible2Vtable, unique_id),
            std::mem::size_of::<IAccessible_Vtbl>() + 13 * std::mem::size_of::<usize>()
        );
        assert_eq!(
            std::mem::offset_of!(Accessible2Vtable, attributes),
            std::mem::size_of::<IAccessible_Vtbl>() + 17 * std::mem::size_of::<usize>()
        );
        assert_eq!(integer(&VARIANT::from(i32::MIN)).unwrap(), i32::MIN);
        assert_eq!(integer(&VARIANT::from(-17)).unwrap(), -17);
        assert!(integer(&VARIANT::from("not a LONG")).is_err());
    }

    #[test]
    fn native_query_service_preserves_absence_and_real_errors() {
        let _apartment = Apartment::new().unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let queries = Arc::new(AtomicUsize::new(0));
        let absent = object(&drops, &queries, false);
        assert!(query_ia2(&absent).unwrap().is_none());
        let denied = object(&drops, &queries, true);
        let error = query_ia2(&denied).unwrap_err();
        assert_eq!(
            error.downcast_ref::<windows::core::Error>().unwrap().code(),
            E_ACCESSDENIED
        );
        assert_eq!(queries.load(Ordering::SeqCst), 2);
        drop(absent);
        drop(denied);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn native_variants_keep_borrowed_bstr_and_owned_dispatch_alive() {
        let _apartment = Apartment::new().unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let queries = Arc::new(AtomicUsize::new(0));
        let parent = object(&drops, &queries, false);
        let input = VARIANT::from("borrowed input");
        assert_eq!(
            unsafe { parent.get_accName(&input) }.unwrap().to_string(),
            "borrowed input"
        );
        assert_eq!(
            BSTR::try_from(&input).unwrap().to_string(),
            "borrowed input"
        );
        let child = object(&drops, &queries, false);
        let values = vec![
            VARIANT::from(child.cast::<IDispatch>().unwrap()),
            VARIANT::from(7),
        ];
        let decoded = decode_children(&parent, &values, 2).unwrap();
        assert_eq!(decoded[1].child, 7);
        let canonical: IUnknown = child.cast().unwrap();
        assert_eq!(decoded[0].object.cast::<IUnknown>().unwrap(), canonical);
        drop(canonical);
        drop(child);
        drop(values);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(decoded);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(parent);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn child_count_and_simple_id_validation_never_reinterpret_unique_ids() {
        let _apartment = Apartment::new().unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let parent = object(&drops, &Arc::new(AtomicUsize::new(0)), false);
        assert!(decode_children(&parent, &[1.into()], -1).is_err());
        assert!(decode_children(&parent, &[1.into()], 2).is_err());
        assert!(decode_children(&parent, &[(-17).into()], 1).is_err());
        assert!(decode_children(&parent, &[0.into()], 1).is_err());
        assert!(decode_children(&parent, &["invalid".into()], 1).is_err());
        assert_eq!(decode_children(&parent, &[99.into()], 0).unwrap().len(), 0);
    }
}
