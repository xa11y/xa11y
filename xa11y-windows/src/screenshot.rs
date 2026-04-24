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
//! The process is switched to Per-Monitor-V2 DPI awareness on first
//! [`WindowsScreenshot::new`] so the captured coordinates match the physical
//! pixels UIA reports for element bounds. If a higher awareness has already
//! been set (for example by a host application), the call is a no-op and we
//! keep the existing awareness — we never downgrade.
//!
//! `Screenshot::scale` is reported as 1.0 because with Per-Monitor-V2
//! awareness the bounds we take in are already physical pixels, so no
//! further upscaling is needed. Multi-monitor setups with mixed DPI still
//! work: the virtual desktop spans all monitors, and `BitBlt` composites
//! through DWM at each monitor's native DPI.
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
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

#[cfg(target_os = "windows")]
impl WindowsScreenshot {
    pub fn new() -> Result<Self> {
        // Best-effort: succeeds on first call, returns ERROR_ACCESS_DENIED on
        // subsequent calls (or if the awareness is pinned by manifest). Either
        // outcome is fine — we only care that awareness is at least
        // Per-Monitor-V2 before we read pixels.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        Ok(Self)
    }
}

#[cfg(target_os = "windows")]
impl ScreenshotProvider for WindowsScreenshot {
    fn capture_full(&self) -> Result<Screenshot> {
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
        capture_rect(vx, vy, vw, vh)
    }

    fn capture_region(&self, rect: Rect) -> Result<Screenshot> {
        if rect.width == 0 || rect.height == 0 {
            return Err(Error::Platform {
                code: -1,
                message: "zero-sized capture rect".into(),
            });
        }
        let w = i32::try_from(rect.width).map_err(|_| Error::Platform {
            code: -1,
            message: "rect width out of i32 range".into(),
        })?;
        let h = i32::try_from(rect.height).map_err(|_| Error::Platform {
            code: -1,
            message: "rect height out of i32 range".into(),
        })?;
        capture_rect(rect.x, rect.y, w, h)
    }
}

/// BitBlt a rectangle of the virtual desktop into an RGBA8 `Screenshot`.
///
/// `x`/`y` are virtual-screen coordinates (may be negative on multi-monitor
/// setups); `w`/`h` are positive pixel dimensions.
#[cfg(target_os = "windows")]
fn capture_rect(x: i32, y: i32, w: i32, h: i32) -> Result<Screenshot> {
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
        scale: 1.0,
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
