//! Linux screen capture. X11 path uses `GetImage` on the root window; Wayland
//! path uses `org.freedesktop.portal.Screenshot` (PNG file URI returned by
//! xdg-desktop-portal, decoded back into RGBA).
//!
//! Wayland portal capture **prompts the user** for consent the first time per
//! session (subsequent calls may be auto-approved depending on the portal
//! implementation) and captures the full screen only — regions are cropped
//! client-side from the full capture.
//!
//! Captured pixels are physical on both paths. `Screenshot::scale` carries the
//! physical-to-logical ratio detected by [`crate::scale`]: an integer factor on
//! a pure-X11 integer-scaled session (read from `Xft.dpi`), and `1.0`
//! elsewhere (Wayland, fractional, or unknown). `capture_region` receives a
//! **logical** rectangle (matching `Element::bounds`) and converts it to
//! physical before reading pixels.

use std::collections::HashMap;
use std::sync::Mutex;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};
use x11rb::rust_connection::RustConnection;
use zbus::blocking::Connection as ZbusConnection;
use zbus::blocking::Proxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use xa11y_core::{Error, Rect, Result, Screenshot, ScreenshotProvider};

/// Choose the Linux screenshot backend based on session environment.
pub struct LinuxScreenshot {
    backend: Backend,
}

// Box the X11 variant's fields — `RustConnection` is large enough that the
// enum would otherwise be dominated by the X11 case and clippy flags it as
// `large_enum_variant`. The backend sits behind an `Arc` in `Screenshotter`,
// so the extra indirection costs at most one allocation per process.
enum Backend {
    X11(Box<X11Backend>),
    Wayland { conn: ZbusConnection },
}

struct X11Backend {
    conn: Mutex<RustConnection>,
    root_width: u16,
    root_height: u16,
    root: u32,
}

impl LinuxScreenshot {
    pub fn new() -> Result<Self> {
        let display_set = std::env::var_os("DISPLAY").is_some();
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();

        if display_set {
            let (conn, screen_num) =
                RustConnection::connect(None).map_err(|e| Error::Platform {
                    code: -1,
                    message: format!("X11 connect: {e}"),
                })?;
            let screen = conn
                .setup()
                .roots
                .get(screen_num)
                .ok_or_else(|| Error::Platform {
                    code: -1,
                    message: "X server reported no screens".into(),
                })?;
            let root = screen.root;
            let root_width = screen.width_in_pixels;
            let root_height = screen.height_in_pixels;
            Ok(Self {
                backend: Backend::X11(Box::new(X11Backend {
                    conn: Mutex::new(conn),
                    root,
                    root_width,
                    root_height,
                })),
            })
        } else if wayland {
            let conn = ZbusConnection::session().map_err(|e| Error::Platform {
                code: -1,
                message: format!("session bus connect: {e}"),
            })?;
            Ok(Self {
                backend: Backend::Wayland { conn },
            })
        } else {
            Err(Error::Unsupported {
                feature: "screenshot (no DISPLAY or WAYLAND_DISPLAY set)".into(),
            })
        }
    }

    fn capture_x11(
        &self,
        conn: &Mutex<RustConnection>,
        root: u32,
        root_w: u16,
        root_h: u16,
        rect: Option<Rect>,
    ) -> Result<Screenshot> {
        let (x, y, w, h) = match rect {
            None => (0_i16, 0_i16, root_w, root_h),
            Some(r) => {
                let x = i16::try_from(r.x).map_err(|_| Error::Platform {
                    code: -1,
                    message: "rect x out of i16 range".into(),
                })?;
                let y = i16::try_from(r.y).map_err(|_| Error::Platform {
                    code: -1,
                    message: "rect y out of i16 range".into(),
                })?;
                let w = u16::try_from(r.width).map_err(|_| Error::Platform {
                    code: -1,
                    message: "rect width out of u16 range".into(),
                })?;
                let h = u16::try_from(r.height).map_err(|_| Error::Platform {
                    code: -1,
                    message: "rect height out of u16 range".into(),
                })?;
                (x, y, w, h)
            }
        };
        if w == 0 || h == 0 {
            return Err(Error::Platform {
                code: -1,
                message: "zero-sized capture rect".into(),
            });
        }

        let guard = conn.lock().unwrap_or_else(|e| e.into_inner());
        let reply = guard
            .get_image(ImageFormat::Z_PIXMAP, root, x, y, w, h, !0)
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetImage: {e}"),
            })?
            .reply()
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("GetImage reply: {e}"),
            })?;

        // Most modern X servers return Z_PIXMAP at 32 bpp for 24-bit visuals,
        // with layout BGRX on little-endian. Detect and convert to RGBA.
        let bpp = (reply.data.len() / (w as usize * h as usize)) as u32;
        if bpp != 4 {
            return Err(Error::Platform {
                code: -1,
                message: format!("unsupported X11 pixmap layout: {bpp} bytes/pixel (expected 4)"),
            });
        }

        let mut rgba = Vec::with_capacity(reply.data.len());
        for chunk in reply.data.chunks_exact(4) {
            // X11 BGRX little-endian → RGBA
            rgba.push(chunk[2]);
            rgba.push(chunk[1]);
            rgba.push(chunk[0]);
            rgba.push(0xFF);
        }

        Ok(Screenshot {
            width: w as u32,
            height: h as u32,
            pixels: rgba,
            scale: 1.0,
        })
    }

    fn capture_wayland(&self, conn: &ZbusConnection, rect: Option<Rect>) -> Result<Screenshot> {
        let proxy = Proxy::new(
            conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Screenshot",
        )
        .map_err(|e| Error::Platform {
            code: -1,
            message: format!("portal Screenshot proxy: {e}"),
        })?;

        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("interactive", Value::Bool(false));
        options.insert("modal", Value::Bool(false));

        let request_path: OwnedObjectPath =
            proxy
                .call("Screenshot", &("", options))
                .map_err(|e| Error::Platform {
                    code: -1,
                    message: format!("portal Screenshot call: {e}"),
                })?;

        let request = Proxy::new(
            conn,
            "org.freedesktop.portal.Desktop",
            &request_path,
            "org.freedesktop.portal.Request",
        )
        .map_err(|e| Error::Platform {
            code: -1,
            message: format!("portal Request proxy: {e}"),
        })?;

        // Block for Response(response: u, results: a{sv}). First signal wins.
        let mut signals = request
            .receive_signal("Response")
            .map_err(|e| Error::Platform {
                code: -1,
                message: format!("receive_signal: {e}"),
            })?;
        let msg = signals.next().ok_or_else(|| Error::Platform {
            code: -1,
            message: "portal Response signal channel closed".into(),
        })?;
        let (response, results): (u32, HashMap<String, OwnedValue>) =
            msg.body().deserialize().map_err(|e| Error::Platform {
                code: -1,
                message: format!("portal Response deserialize: {e}"),
            })?;
        if response != 0 {
            return Err(Error::PermissionDenied {
                instructions: format!("xdg-desktop-portal Screenshot denied (response={response})"),
            });
        }
        let uri_val = results.get("uri").ok_or_else(|| Error::Platform {
            code: -1,
            message: "portal Response missing 'uri' key".into(),
        })?;
        let uri: String = uri_val
            .downcast_ref::<String>()
            .map_err(|_| Error::Platform {
                code: -1,
                message: "portal Response 'uri' is not a string".into(),
            })?;
        let path = uri.strip_prefix("file://").ok_or_else(|| Error::Platform {
            code: -1,
            message: format!("portal URI not file://: {uri}"),
        })?;
        let bytes = std::fs::read(path).map_err(|e| Error::Platform {
            code: e.raw_os_error().unwrap_or(-1) as i64,
            message: format!("read portal PNG: {e}"),
        })?;
        // Best-effort cleanup of portal tmpfile.
        let _ = std::fs::remove_file(path);

        let shot = decode_png_to_rgba(&bytes)?;
        match rect {
            None => Ok(shot),
            Some(r) => crop_rgba(shot, r),
        }
    }
}

impl ScreenshotProvider for LinuxScreenshot {
    fn capture_full(&self) -> Result<Screenshot> {
        // Captured pixels are physical; stamp the physical-to-logical ratio so
        // callers can map logical bounds onto them. `capture_*` produce the
        // raw pixels; the scale is metadata applied here. See `crate::scale`.
        let scale = crate::scale::screenshot_scale();
        let mut shot = match &self.backend {
            Backend::X11(x) => self.capture_x11(&x.conn, x.root, x.root_width, x.root_height, None),
            Backend::Wayland { conn } => self.capture_wayland(conn, None),
        }?;
        shot.scale = scale as f32;
        Ok(shot)
    }

    fn capture_region(&self, rect: Rect) -> Result<Screenshot> {
        // `rect` is logical (matching `Element::bounds`); convert to the
        // physical pixels the X server / portal image work in.
        let scale = crate::scale::screenshot_scale();
        let phys = rect.to_physical(scale);
        let mut shot = match &self.backend {
            Backend::X11(x) => {
                self.capture_x11(&x.conn, x.root, x.root_width, x.root_height, Some(phys))
            }
            Backend::Wayland { conn } => self.capture_wayland(conn, Some(phys)),
        }?;
        shot.scale = scale as f32;
        Ok(shot)
    }
}

fn decode_png_to_rgba(bytes: &[u8]) -> Result<Screenshot> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|e| Error::Platform {
        code: -1,
        message: format!("png decode header: {e}"),
    })?;
    let info = reader.info().clone();
    let buf_size = reader.output_buffer_size().ok_or_else(|| Error::Platform {
        code: -1,
        message: "png decode: output buffer size overflowed usize".to_string(),
    })?;
    let mut buf = vec![0u8; buf_size];
    let frame = reader.next_frame(&mut buf).map_err(|e| Error::Platform {
        code: -1,
        message: format!("png decode frame: {e}"),
    })?;
    buf.truncate(frame.buffer_size());

    let rgba = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buf,
        (png::ColorType::Rgb, png::BitDepth::Eight) => {
            let mut out = Vec::with_capacity((info.width * info.height * 4) as usize);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
            out
        }
        (ct, bd) => {
            return Err(Error::Platform {
                code: -1,
                message: format!("unsupported portal PNG format: {ct:?} @ {bd:?}"),
            });
        }
    };

    Ok(Screenshot {
        width: info.width,
        height: info.height,
        pixels: rgba,
        scale: 1.0,
    })
}

fn crop_rgba(shot: Screenshot, rect: Rect) -> Result<Screenshot> {
    let Screenshot {
        width: sw,
        height: sh,
        pixels,
        scale,
    } = shot;
    let x = rect.x.max(0) as u32;
    let y = rect.y.max(0) as u32;
    if x >= sw || y >= sh {
        return Err(Error::Platform {
            code: -1,
            message: "crop rect outside captured image".into(),
        });
    }
    let w = rect.width.min(sw - x);
    let h = rect.height.min(sh - y);
    if w == 0 || h == 0 {
        return Err(Error::Platform {
            code: -1,
            message: "crop rect has zero size".into(),
        });
    }
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let start = ((y + row) * sw + x) as usize * 4;
        let end = start + (w as usize) * 4;
        out.extend_from_slice(&pixels[start..end]);
    }
    Ok(Screenshot {
        width: w,
        height: h,
        pixels: out,
        scale,
    })
}
