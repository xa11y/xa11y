//! Windows screen capture using GDI `BitBlt` against the virtual-desktop DC.
//!
//! We chose GDI over `Windows.Graphics.Capture` (WinRT) because the latter
//! requires async plumbing and a message pump even for a single snapshot, and
//! over DXGI Desktop Duplication because that API is per-adapter and adds a
//! D3D dependency for what is otherwise a one-shot pixel read. `BitBlt` is
//! synchronous, has no extra dependencies, and matches the model the other
//! backends in this crate use (direct OS calls, no framework wrapper).
//!
//! # DPI
//!
//! Per-Monitor-V2 DPI awareness is set once, eagerly, via
//! [`crate::dpi::ensure_process_dpi_aware`] — the same shared init the UIA
//! provider calls — so it is established before the first UIA bounds read
//! regardless of call order (issue #300). If a higher awareness has already
//! been set (for example by a host application), the call is a no-op and we
//! keep the existing awareness — we never downgrade.
//!
//! Regions are captured in **physical** pixels. `capture_region` receives a
//! rectangle in **logical** coordinates (the cross-platform contract, matching
//! `Element::bounds`) and converts it to physical using the DPI of the monitor
//! under its origin; the resulting [`Screenshot::scale`] carries the
//! physical-to-logical ratio so callers can map logical bounds onto captured
//! pixels. Multi-monitor setups with mixed DPI are handled per-monitor; see
//! [`crate::dpi`] for the boundary-straddling caveat.
//!
//! # Active session required
//!
//! `BitBlt` against the desktop DC only works when the calling process's
//! window station has a usable interactive desktop — i.e. a logged-in,
//! *connected* session. Disconnected RDP sessions, Session 0 services, and
//! other non-interactive contexts return `ERROR_INVALID_HANDLE` from
//! `BitBlt` even though `GetDC(NULL)` succeeded. When we detect that error
//! code we surface it as [`Error::Unsupported`] so callers can distinguish
//! "no desktop to capture" from a real platform failure.

use xa11y_core::{Error, Rect, Result, Screenshot, ScreenshotProvider};

pub struct WindowsScreenshot;

#[cfg(not(target_os = "windows"))]
impl WindowsScreenshot {
    pub fn new() -> Result<Self> {
        Err(Error::Platform {
            code: -1,
            message: "Windows screenshot backend only available on Windows".into(),
        })
    }
}

#[cfg(not(target_os = "windows"))]
impl ScreenshotProvider for WindowsScreenshot {
    fn capture_full(&self) -> Result<Screenshot> {
        unreachable!()
    }
    fn capture_region(&self, _: Rect) -> Result<Screenshot> {
        unreachable!()
    }
}

#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ, SRCCOPY,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

#[cfg(target_os = "windows")]
impl WindowsScreenshot {
    pub fn new() -> Result<Self> {
        // Ensure Per-Monitor-V2 awareness is set before we read pixels. This
        // is the same shared, once-only init the UIA provider calls, so
        // awareness is established regardless of whether the consumer touches
        // the a11y tree or a screenshot first (issue #300).
        crate::dpi::ensure_process_dpi_aware();
        Ok(Self)
    }
}

#[cfg(target_os = "windows")]
impl ScreenshotProvider for WindowsScreenshot {
    fn capture_full(&self) -> Result<Screenshot> {
        // With Per-Monitor-V2 awareness the virtual-screen metrics are already
        // physical pixels, so no logical->physical conversion is needed here.
        let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        if vw <= 0 || vh <= 0 {
            return Err(Error::Platform {
                code: -1,
                message: format!("virtual screen has non-positive size: {vw}x{vh}"),
            });
        }
        // Report the scale of the monitor at the virtual-desktop origin. The
        // full capture may span mixed-DPI monitors; `scale` is a single scalar
        // by contract, so we report the origin monitor's factor.
        let scale = crate::dpi::scale_for_physical_point(vx, vy) as f32;
        capture_rect(vx, vy, vw, vh, scale)
    }

    fn capture_region(&self, rect: Rect) -> Result<Screenshot> {
        // `rect` arrives in logical coordinates (the cross-platform contract,
        // matching `Element::bounds`). Convert to the physical pixels BitBlt
        // works in, using the DPI of the monitor under the rect's origin.
        let scale = crate::dpi::scale_for_logical_point(rect.x, rect.y);
        let phys = rect.to_physical(scale);
        if phys.width == 0 || phys.height == 0 {
            return Err(Error::Platform {
                code: -1,
                message: "zero-sized capture rect".into(),
            });
        }
        let w = i32::try_from(phys.width).map_err(|_| Error::Platform {
            code: -1,
            message: "rect width out of i32 range".into(),
        })?;
        let h = i32::try_from(phys.height).map_err(|_| Error::Platform {
            code: -1,
            message: "rect height out of i32 range".into(),
        })?;
        capture_rect(phys.x, phys.y, w, h, scale as f32)
    }
}

/// BitBlt a rectangle of the virtual desktop into an RGBA8 `Screenshot`.
///
/// `x`/`y` are **physical** virtual-screen coordinates (may be negative on
/// multi-monitor setups); `w`/`h` are positive physical pixel dimensions.
/// `scale` is the physical-to-logical ratio recorded on the returned
/// `Screenshot` so callers can map logical bounds to captured pixels.
#[cfg(target_os = "windows")]
fn capture_rect(x: i32, y: i32, w: i32, h: i32, scale: f32) -> Result<Screenshot> {
    let width = w as u32;
    let height = h as u32;

    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.is_invalid() {
        return Err(platform("GetDC(NULL) returned an invalid DC"));
    }
    let _guard_screen = ScreenDc(screen_dc);

    let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
    if mem_dc.is_invalid() {
        return Err(platform("CreateCompatibleDC failed"));
    }
    let _guard_mem = MemDc(mem_dc);

    let bitmap = unsafe { CreateCompatibleBitmap(screen_dc, w, h) };
    if bitmap.is_invalid() {
        return Err(platform("CreateCompatibleBitmap failed"));
    }
    let _guard_bmp = BmpGuard(bitmap);

    let prev = unsafe { SelectObject(mem_dc, HGDIOBJ(bitmap.0)) };

    unsafe {
        BitBlt(mem_dc, 0, 0, w, h, Some(screen_dc), x, y, SRCCOPY).map_err(|e| {
            let code = e.code().0;
            // HRESULT_FROM_WIN32(ERROR_INVALID_HANDLE) — what BitBlt actually
            // returns on disconnected / non-interactive sessions. Surface as
            // Unsupported so tests can skip gracefully rather than panic.
            if code == 0x8007_0006_u32 as i32 {
                Error::Unsupported {
                    feature: "screen capture (no active desktop session; \
                              reconnect the user session and retry)"
                        .into(),
                }
            } else {
                Error::Platform {
                    code: -1,
                    message: format!("BitBlt: {e}"),
                }
            }
        })?;
    }

    // Request top-down BGRA (negative height ⇒ top-down DIB). We later swap
    // blue/red in place to get RGBA.
    let mut header = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: Default::default(),
    };

    let byte_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| platform("screenshot dimensions overflow"))?;
    let mut pixels = vec![0u8; byte_len];

    let scanned = unsafe {
        GetDIBits(
            mem_dc,
            bitmap,
            0,
            height,
            Some(pixels.as_mut_ptr().cast()),
            &mut header,
            DIB_RGB_COLORS,
        )
    };
    if scanned == 0 {
        return Err(platform("GetDIBits returned 0 scanlines"));
    }

    // Restore the original bitmap before the guards run so Windows doesn't
    // delete a selected object (GDI leaks otherwise). The guards then free
    // the bitmap/memory DC/screen DC in the right order.
    let _ = unsafe { SelectObject(mem_dc, prev) };

    // BGRA → RGBA, in place.
    for px in pixels.chunks_exact_mut(4) {
        px.swap(0, 2);
        px[3] = 0xFF;
    }

    Ok(Screenshot {
        width,
        height,
        pixels,
        scale,
    })
}

#[cfg(target_os = "windows")]
fn platform(msg: &str) -> Error {
    Error::Platform {
        code: -1,
        message: msg.into(),
    }
}

// RAII guards so every exit path releases GDI handles.

#[cfg(target_os = "windows")]
struct ScreenDc(HDC);
#[cfg(target_os = "windows")]
impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseDC(None, self.0);
        }
    }
}

#[cfg(target_os = "windows")]
struct MemDc(HDC);
#[cfg(target_os = "windows")]
impl Drop for MemDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

#[cfg(target_os = "windows")]
struct BmpGuard(HBITMAP);
#[cfg(target_os = "windows")]
impl Drop for BmpGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0 .0));
        }
    }
}
