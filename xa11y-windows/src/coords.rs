//! The only module allowed to call coordinate-bearing Win32 / UIA APIs.
//!
//! Windows answers every one of these in the *calling thread's* DPI
//! awareness, and xa11y reports physical desktop pixels, so each call must run
//! inside a [`DpiScope`] (issue #506). Three layers keep a call site from
//! slipping out of one:
//!
//! 1. **Compiler.** Every wrapper here takes `&DpiScope`, and `DpiScope` is
//!    `!Send` and only built by [`DpiScope::enter`], so a caller without a
//!    live scope on its own thread does not compile.
//! 2. **Lint.** `cargo xtask check-windows-dpi` fails the build when any other
//!    file under `xa11y-windows/src` names one of the raw symbols wrapped
//!    here, so the wrappers cannot be bypassed by accident. It is a
//!    heuristic over identifier tokens, written to catch accidents rather
//!    than deliberate evasion: a raw function pointer, an FFI declaration of
//!    its own, or a macro that pastes a name together all get past it.
//! 3. **Runtime.** Each wrapper `debug_assert!`s that the thread really is
//!    per-monitor aware, and `tests/dpi_system_aware.rs` runs the backends in
//!    a process pinned to `SYSTEM_AWARE`, where a path that got past 1 and 2
//!    would trip it.
//!
//! UIA converts `BoundingRectangle` when it *fetches* a cache
//! (`FindAllBuildCache`, `BuildUpdatedCache`, `ElementFromHandleBuildCache`,
//! …), not when [`cached_bounding_rect`] reads it back. So the gate is on the
//! fetch: the cache request that asks for bounds is a [`BoundsCacheRequest`],
//! and the only way to hand it to a fetch is [`BoundsCacheRequest::get`],
//! which takes `&DpiScope`.

#![cfg(target_os = "windows")]

use windows::core::BOOL;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::GetDC;
use windows::Win32::Graphics::Gdi::{
    BitBlt, EnumDisplayMonitors, GetDIBits, BITMAPINFO, DIB_USAGE, HBITMAP, HDC, HMONITOR, SRCCOPY,
};
use windows::Win32::UI::Accessibility::{
    IUIAutomationCacheRequest, IUIAutomationElement, IUIAutomationTransformPattern,
    UIA_BoundingRectanglePropertyId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, GetWindowRect, WindowFromPoint, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::dpi::DpiScope;
use xa11y_core::{Error, Result};

/// Read UIA text rectangles while the calling thread uses physical pixels.
pub fn text_range_bounds(
    _scope: &DpiScope,
    range: &windows::Win32::UI::Accessibility::IUIAutomationTextRange,
) -> Result<Option<xa11y_core::Rect>> {
    use windows::Win32::System::Ole::{
        SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElement, SafeArrayGetLBound,
        SafeArrayGetUBound, SafeArrayGetVartype,
    };
    use windows::Win32::System::Variant::VT_R8;
    assert_per_monitor();
    let err = |e: windows::core::Error| {
        Error::platform(
            e.code().0 as i64,
            format!("reading UIA text bounding rectangles: {e}"),
        )
    };
    let array = unsafe { range.GetBoundingRectangles() }.map_err(err)?;
    if array.is_null() {
        return Ok(None);
    }
    struct OwnedArray(*mut windows::Win32::System::Com::SAFEARRAY);
    impl Drop for OwnedArray {
        fn drop(&mut self) {
            // Ownership comes from GetBoundingRectangles; destruction cannot
            // affect the already-read result and must also run on error paths.
            let _ = unsafe { SafeArrayDestroy(self.0) };
        }
    }
    let array = OwnedArray(array);
    if unsafe { SafeArrayGetDim(array.0) } != 1
        || unsafe { SafeArrayGetVartype(array.0) }.map_err(err)? != VT_R8
    {
        return Err(Error::platform(
            -1,
            "UIA text rectangles must be a one-dimensional double array",
        ));
    }
    let lower = unsafe { SafeArrayGetLBound(array.0, 1) }.map_err(err)?;
    let upper = unsafe { SafeArrayGetUBound(array.0, 1) }.map_err(err)?;
    let count = (i64::from(upper) - i64::from(lower) + 1).max(0);
    if count % 4 != 0 {
        return Err(Error::platform(
            -1,
            "UIA text rectangle array length is not a multiple of four",
        ));
    }
    let read = |index: i32| -> Result<f64> {
        let mut value = 0f64;
        unsafe { SafeArrayGetElement(array.0, &index, (&raw mut value).cast()) }.map_err(err)?;
        Ok(value)
    };
    let mut union: Option<(f64, f64, f64, f64)> = None;
    for i in (i64::from(lower)..=i64::from(upper)).step_by(4) {
        let i = i as i32;
        let (x, y, w, h) = (read(i)?, read(i + 1)?, read(i + 2)?, read(i + 3)?);
        if ![x, y, w, h].iter().all(|v| v.is_finite()) || w < 0.0 || h < 0.0 {
            return Err(Error::platform(
                -1,
                "UIA returned invalid text rectangle coordinates",
            ));
        }
        union = Some(match union {
            None => (x, y, x + w, y + h),
            Some((l, t, r, b)) => (l.min(x), t.min(y), r.max(x + w), b.max(y + h)),
        });
    }
    Ok(union.map(|(l, t, r, b)| {
        crate::dpi::physical_rect_to_desktop(RECT {
            left: l.floor() as i32,
            top: t.floor() as i32,
            right: r.ceil() as i32,
            bottom: b.ceil() as i32,
        })
    }))
}

/// The runtime layer: the scope a wrapper was handed is really in effect.
#[inline]
fn assert_per_monitor() {
    debug_assert!(
        crate::dpi::thread_is_per_monitor(),
        "a coordinate-bearing Win32/UIA call ran on a thread that is not per-monitor DPI \
         aware; its DpiScope is not in effect (issue #506)"
    );
}

/// `IUIAutomationElement::CachedBoundingRectangle`.
pub fn cached_bounding_rect(
    _scope: &DpiScope,
    element: &IUIAutomationElement,
) -> windows::core::Result<RECT> {
    assert_per_monitor();
    unsafe { element.CachedBoundingRectangle() }
}

/// `SendInput`; returns how many events were inserted.
pub fn send_input(_scope: &DpiScope, inputs: &[INPUT]) -> u32 {
    assert_per_monitor();
    // SAFETY: `inputs` is a valid slice and the size matches `INPUT`.
    unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) }
}

/// The virtual screen as `(x, y, width, height)` from `GetSystemMetrics`.
pub fn virtual_screen(_scope: &DpiScope) -> (i32, i32, i32, i32) {
    assert_per_monitor();
    // SAFETY: GetSystemMetrics takes a constant and returns an i32.
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// `GetCursorPos`.
pub fn cursor_pos(_scope: &DpiScope) -> windows::core::Result<POINT> {
    assert_per_monitor();
    let mut p = POINT::default();
    // SAFETY: `p` outlives the call.
    unsafe { GetCursorPos(&mut p) }?;
    Ok(p)
}

/// `WindowFromPoint`.
pub fn window_from_point(_scope: &DpiScope, p: POINT) -> HWND {
    assert_per_monitor();
    // SAFETY: plain Win32 call on a value POINT.
    unsafe { WindowFromPoint(p) }
}

/// `GetWindowRect`.
pub fn window_rect(_scope: &DpiScope, hwnd: HWND) -> windows::core::Result<RECT> {
    assert_per_monitor();
    let mut rect = RECT::default();
    // SAFETY: `rect` outlives the call.
    unsafe { GetWindowRect(hwnd, &mut rect) }?;
    Ok(rect)
}

/// `IUIAutomationTransformPattern::Move`.
pub fn transform_move(
    _scope: &DpiScope,
    pattern: &IUIAutomationTransformPattern,
    x: f64,
    y: f64,
) -> windows::core::Result<()> {
    assert_per_monitor();
    unsafe { pattern.Move(x, y) }
}

/// `IUIAutomationTransformPattern::Resize`.
pub fn transform_resize(
    _scope: &DpiScope,
    pattern: &IUIAutomationTransformPattern,
    width: f64,
    height: f64,
) -> windows::core::Result<()> {
    assert_per_monitor();
    unsafe { pattern.Resize(width, height) }
}

/// `BitBlt` of the screen rectangle at `(x, y)` into `(0, 0)` of `dest`.
///
/// # Safety
/// `dest` and `screen` must be valid device contexts.
pub unsafe fn blit_from_screen(
    _scope: &DpiScope,
    dest: HDC,
    width: i32,
    height: i32,
    screen: HDC,
    x: i32,
    y: i32,
) -> windows::core::Result<()> {
    assert_per_monitor();
    unsafe { BitBlt(dest, 0, 0, width, height, Some(screen), x, y, SRCCOPY) }
}

/// `GetDIBits`; returns the number of scan lines copied.
///
/// # Safety
/// Same contract as `GetDIBits`: `pixels` must hold `lines` scan lines in the
/// format `info` describes.
pub unsafe fn dib_bits(
    _scope: &DpiScope,
    dc: HDC,
    bitmap: HBITMAP,
    lines: u32,
    pixels: *mut core::ffi::c_void,
    info: &mut BITMAPINFO,
    usage: DIB_USAGE,
) -> i32 {
    assert_per_monitor();
    unsafe { GetDIBits(dc, bitmap, 0, lines, Some(pixels), info, usage) }
}

/// Every display's rectangle in virtual-screen pixels (`EnumDisplayMonitors`).
pub fn monitor_rects(_scope: &DpiScope) -> Result<Vec<RECT>> {
    assert_per_monitor();
    let mut monitors = Vec::new();
    // SAFETY: the callback receives the pointer unchanged; `monitors` lives
    // until EnumDisplayMonitors returns.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut monitors as *mut Vec<RECT> as isize),
        )
    }
    .as_bool();
    if !ok {
        return Err(Error::platform(
            -1,
            "EnumDisplayMonitors failed while reading desktop layout",
        ));
    }
    Ok(monitors)
}

unsafe extern "system" fn collect_monitor(
    _hmonitor: HMONITOR,
    _hdc: HDC,
    lprc: *mut RECT,
    user_data: LPARAM,
) -> BOOL {
    let monitors = unsafe { &mut *(user_data.0 as *mut Vec<RECT>) };
    // A null HDC makes the supplied rectangle use virtual-screen pixels.
    // EnumDisplayMonitors keeps this pointer valid for the callback.
    monitors.push(unsafe { *lprc });
    BOOL(1)
}

/// A UIA cache request that fetches `BoundingRectangle`.
///
/// UIA converts the bounds into the calling thread's DPI space during the
/// fetch, so the request is only reachable through [`Self::get`], which needs
/// a live [`DpiScope`]. The bounds property is added here, the one place
/// allowed to name it.
#[derive(Clone)]
pub struct BoundsCacheRequest(IUIAutomationCacheRequest);

impl BoundsCacheRequest {
    /// Wrap `request`, adding the bounds property to it.
    pub fn new(request: IUIAutomationCacheRequest) -> Result<Self> {
        unsafe { request.AddProperty(UIA_BoundingRectanglePropertyId) }.map_err(|e| {
            Error::platform(
                e.code().0 as i64,
                format!("AddProperty(BoundingRectangle) failed: {e}"),
            )
        })?;
        Ok(Self(request))
    }

    /// The request, for a fetch made inside `_scope`.
    pub fn get(&self, _scope: &DpiScope) -> &IUIAutomationCacheRequest {
        assert_per_monitor();
        &self.0
    }
}

/// Whether DWM cloaks `hwnd` (`DWMWA_CLOAKED`). DwmGetWindowAttribute also
/// answers coordinate attributes, so it lives here with the rest.
pub fn is_cloaked(_scope: &DpiScope, hwnd: HWND) -> windows::core::Result<bool> {
    assert_per_monitor();
    let mut cloaked = 0u32;
    // SAFETY: `cloaked` is a u32 for the call's duration, as DWMWA_CLOAKED
    // requires.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    }?;
    Ok(cloaked != 0)
}

/// `GetDC(NULL)`: the whole-screen device context, whose coordinates are the
/// calling thread's DPI space. The caller releases it.
pub fn screen_dc(_scope: &DpiScope) -> HDC {
    assert_per_monitor();
    // SAFETY: plain GDI call.
    unsafe { GetDC(None) }
}

/// MSAA coordinates read with the same physical desktop DPI scope as UIA.
pub fn msaa_bounds(
    _scope: &DpiScope,
    object: &windows::Win32::UI::Accessibility::IAccessible,
    child: &windows::Win32::System::Variant::VARIANT,
) -> windows::core::Result<xa11y_core::Rect> {
    assert_per_monitor();
    let (mut x, mut y, mut width, mut height) = (0, 0, 0, 0);
    unsafe {
        object.accLocation(&mut x, &mut y, &mut width, &mut height, child)?;
    }
    if width < 0 || height < 0 {
        return Err(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_INVALIDARG,
        ));
    }
    Ok(xa11y_core::Rect {
        x,
        y,
        width: width as u32,
        height: height as u32,
    })
}
/// IA2 screen-relative extents in physical desktop pixels.
pub fn ia2_character_bounds(
    _scope: &DpiScope,
    text: &crate::ia2::abi::AccessibleText,
    offset: i32,
) -> windows::core::Result<xa11y_core::Rect> {
    use windows::core::Interface;
    assert_per_monitor();
    let (mut x, mut y, mut width, mut height) = (0, 0, 0, 0);
    unsafe {
        (text.vtable().characterExtents)(
            text.as_raw(),
            offset,
            0,
            &mut x,
            &mut y,
            &mut width,
            &mut height,
        )
        .ok()?;
    }
    if width < 0 || height < 0 {
        return Err(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_INVALIDARG,
        ));
    }
    Ok(xa11y_core::Rect {
        x,
        y,
        width: width as u32,
        height: height as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_are_never_in_a_shared_property_list() {
        // The bounds property must only be added by BoundsCacheRequest::new,
        // so a fetch of it always needs a scope.
        assert!(!crate::uia::batch_properties().contains(&UIA_BoundingRectanglePropertyId));
        assert!(!crate::uia::scroll_properties().contains(&UIA_BoundingRectanglePropertyId));
    }
}
