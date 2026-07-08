//! Best-effort HiDPI scale detection for Linux, shared by the AT-SPI provider,
//! the screenshot backend, and the input backend.
//!
//! # Two scales, because two coordinate spaces
//!
//! The cross-platform contract (see `xa11y_core`) is that `Element::bounds` and
//! input `Point`s are **logical** (device-independent) coordinates, and
//! `Screenshot::scale` is the physical-to-logical ratio. Reaching that on Linux
//! needs the display scale — but *where* the scale applies differs by display
//! server, so this module exposes two functions:
//!
//! - [`coordinate_scale`] — the factor relating AT-SPI bounds / input points to
//!   logical coordinates. Non-`1.0` **only on a pure-X11 session** with integer
//!   scaling, read from `Xft.dpi` in the X `RESOURCE_MANAGER` (the same signal
//!   GTK/Qt use). On X11, AT-SPI `GetExtents(screen)` and XTest are in physical
//!   pixels, so bounds are divided by this and input points multiplied by it.
//!   On Wayland it is `1.0`: AT-SPI coordinates are already logical, and the
//!   uinput backend maps its virtual absolute range onto the compositor's
//!   logical pointer space — so no coordinate scaling is applied there.
//!
//! - [`screenshot_scale`] — the physical-to-logical ratio of *captured pixels*.
//!   On X11 this equals [`coordinate_scale`]. On **Wayland** it comes from a
//!   live `wl_output` query, because the Screenshot portal returns physical
//!   pixels while bounds/regions are logical: the region must be scaled up to
//!   crop correctly, and the honest ratio is reported on the `Screenshot`.
//!
//! # Caveats (documented, not silently guessed)
//!
//! - **Fractional X11 scaling** (e.g. `Xft.dpi` 120/144): GTK's X11 window
//!   scale is integer-only, so a fractional `Xft.dpi` scales fonts, not the
//!   coordinate space. [`coordinate_scale`] returns `1.0` for non-integer DPI.
//! - **Fractional Wayland scaling** *(single output)*: handled exactly via
//!   `xdg-output` — the scale is `physical_mode_width / logical_size_width`
//!   (e.g. `1920 / 1280 = 1.5`), not the rounded `wl_output.scale`.
//! - **Multi-output mixed-DPI Wayland**: a single scalar `Screenshot::scale`
//!   can't represent per-monitor scales, so we fall back to the largest integer
//!   `wl_output.scale`. A true fix needs a per-region scale, which is a larger
//!   API change (the Windows backend has the same limitation).
//!
//! Reporting `1.0` never breaks capture/input round-trips (bounds and pixels
//! stay in the same space); it only means bounds are physical and scale is not
//! upscaled on those configurations.

#![cfg(target_os = "linux")]

use std::sync::OnceLock;

use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

/// "100%" DPI baseline: `scale = dpi / 96`.
const BASE_DPI: f64 = 96.0;
/// Largest integer scale we will honor; guards against a garbage reading.
const MAX_SCALE: i64 = 8;
/// How close `dpi / 96` must be to an integer to be treated as integer scaling.
const INTEGER_EPS: f64 = 0.05;

/// The scale relating AT-SPI bounds and input points to logical coordinates.
/// Detected once, then cached. `1.0` on anything but a pure-X11 integer-scaled
/// session — see the module docs.
pub fn coordinate_scale() -> f64 {
    static SCALE: OnceLock<f64> = OnceLock::new();
    *SCALE.get_or_init(detect_x11_scale)
}

/// The physical-to-logical ratio of captured pixels. Equals
/// [`coordinate_scale`] on X11; on Wayland it comes from a `wl_output` query.
/// Detected once, then cached.
pub fn screenshot_scale() -> f64 {
    static SCALE: OnceLock<f64> = OnceLock::new();
    *SCALE.get_or_init(|| {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            wayland_output_scale().unwrap_or(1.0)
        } else {
            coordinate_scale()
        }
    })
}

/// X11 `Xft.dpi`-derived integer scale. `1.0` under Wayland (coordinates are
/// logical there) or when there is no X display.
fn detect_x11_scale() -> f64 {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return 1.0;
    }
    if std::env::var_os("DISPLAY").is_none() {
        return 1.0;
    }
    match read_xft_dpi() {
        Some(dpi) => scale_from_dpi(dpi),
        None => 1.0,
    }
}

/// Read the `Xft.dpi` resource from the X `RESOURCE_MANAGER` root-window
/// property. Returns `None` on any X failure or if the resource is absent —
/// callers degrade to `1.0`, never an error (a DPI query must not fail a tree
/// read or a capture).
fn read_xft_dpi() -> Option<f64> {
    let (conn, screen_num) = RustConnection::connect(None).ok()?;
    let root = conn.setup().roots.get(screen_num)?.root;
    let reply = conn
        .get_property(
            false,
            root,
            AtomEnum::RESOURCE_MANAGER,
            AtomEnum::STRING,
            0,
            // RESOURCE_MANAGER is small (KBs); cap the read generously in
            // 32-bit units as the X protocol expects.
            1 << 18,
        )
        .ok()?
        .reply()
        .ok()?;
    let text = String::from_utf8_lossy(&reply.value);
    parse_xft_dpi(&text)
}

/// Parse `Xft.dpi:<whitespace><number>` out of an xrdb `RESOURCE_MANAGER`
/// dump. Pure — unit-tested without an X server.
fn parse_xft_dpi(resources: &str) -> Option<f64> {
    for line in resources.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Xft.dpi:") {
            return rest.trim().parse::<f64>().ok().filter(|d| *d > 0.0);
        }
    }
    None
}

/// Map an `Xft.dpi` value to a coordinate scale, honoring **integer** scales
/// only. `dpi / 96` is snapped to the nearest integer when within
/// [`INTEGER_EPS`]; otherwise (fractional or out-of-range) returns `1.0`.
/// Pure — unit-tested.
fn scale_from_dpi(dpi: f64) -> f64 {
    if !dpi.is_finite() || dpi <= 0.0 {
        return 1.0;
    }
    let raw = dpi / BASE_DPI;
    let nearest = raw.round();
    if (raw - nearest).abs() <= INTEGER_EPS && nearest >= 1.0 && (nearest as i64) <= MAX_SCALE {
        nearest
    } else {
        1.0
    }
}

/// Wayland output geometry gathered to compute an exact scale.
///
/// `wl_output` gives the physical current-mode width and the integer buffer
/// scale; `xdg-output` gives the logical width. The exact (possibly fractional)
/// scale is `physical_mode / logical_size`.
#[derive(Default)]
struct OutputInfo {
    outputs: Vec<wl_output::WlOutput>,
    xdg_manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    xdg_outputs: Vec<zxdg_output_v1::ZxdgOutputV1>,
    output_count: usize,
    /// Width of the current mode, in physical pixels.
    current_mode_width: Option<i32>,
    /// Logical width from xdg-output.
    logical_width: Option<i32>,
    /// Largest integer `wl_output.scale` (fallback for multi-output).
    max_int_scale: i32,
}

impl Dispatch<wl_registry::WlRegistry, ()> for OutputInfo {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                // Bind at most v2 — enough for the `scale`/`mode` events, and
                // avoids having to handle the v4 name/description events.
                "wl_output" if version >= 2 => {
                    let output = registry.bind::<wl_output::WlOutput, (), Self>(name, 2, qh, ());
                    state.outputs.push(output);
                    state.output_count += 1;
                }
                "zxdg_output_manager_v1" => {
                    let mgr = registry
                        .bind::<zxdg_output_manager_v1::ZxdgOutputManagerV1, (), Self>(
                            name,
                            version.min(3),
                            qh,
                            (),
                        );
                    state.xdg_manager = Some(mgr);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for OutputInfo {
    fn event(
        state: &mut Self,
        _output: &wl_output::WlOutput,
        event: wl_output::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_output::Event::Scale { factor } => {
                state.max_int_scale = state.max_int_scale.max(factor);
            }
            // Only the current mode reflects the active physical resolution.
            wl_output::Event::Mode {
                flags: WEnum::Value(m),
                width,
                ..
            } if m.contains(wl_output::Mode::Current) => {
                state.current_mode_width = Some(width);
            }
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_manager_v1::ZxdgOutputManagerV1, ()> for OutputInfo {
    fn event(
        _state: &mut Self,
        _mgr: &zxdg_output_manager_v1::ZxdgOutputManagerV1,
        _event: zxdg_output_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // zxdg_output_manager_v1 has no events.
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, ()> for OutputInfo {
    fn event(
        state: &mut Self,
        _xdg_output: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zxdg_output_v1::Event::LogicalSize { width, .. } = event {
            state.logical_width = Some(width);
        }
    }
}

/// Query the live Wayland output scale. For a single output the exact scale is
/// `physical_mode_width / xdg_output.logical_width`, which handles fractional
/// scaling (e.g. 1.5×). With multiple outputs a single scalar can't represent
/// mixed DPI, so it falls back to the largest integer `wl_output.scale`.
/// Returns `None` on any failure so the caller degrades to `1.0`.
///
/// Only meaningful on a Wayland session; guarded by the `WAYLAND_DISPLAY` check
/// in [`screenshot_scale`].
fn wayland_output_scale() -> Option<f64> {
    let conn = Connection::connect_to_env().ok()?;
    let display = conn.display();
    let mut queue = conn.new_event_queue::<OutputInfo>();
    let qh = queue.handle();
    display.get_registry(&qh, ());

    let mut state = OutputInfo::default();
    // Roundtrip 1: registry globals arrive; bind the outputs and xdg manager.
    queue.roundtrip(&mut state).ok()?;

    // Now that the manager exists, request an xdg-output for each wl_output.
    if let Some(mgr) = state.xdg_manager.clone() {
        for output in state.outputs.clone() {
            let xdg_output = mgr.get_xdg_output(&output, &qh, ());
            state.xdg_outputs.push(xdg_output);
        }
    }

    // Roundtrips 2-3: wl_output mode/scale + xdg-output logical_size bursts.
    queue.roundtrip(&mut state).ok()?;
    queue.roundtrip(&mut state).ok()?;

    combine_wayland_scale(
        state.output_count,
        state.current_mode_width,
        state.logical_width,
        state.max_int_scale,
    )
}

/// Pure scale combination — unit-tested without a compositor.
///
/// Single output with known physical + logical widths → exact ratio (fractional
/// supported). Otherwise the integer buffer scale. `None` if nothing sane is
/// available.
fn combine_wayland_scale(
    output_count: usize,
    mode_width: Option<i32>,
    logical_width: Option<i32>,
    max_int_scale: i32,
) -> Option<f64> {
    if output_count == 1 {
        if let (Some(m), Some(l)) = (mode_width, logical_width) {
            if m > 0 && l > 0 {
                return sane_scale(f64::from(m) / f64::from(l));
            }
        }
    }
    // Multi-output or missing xdg-output geometry: integer buffer scale.
    sane_scale(f64::from(max_int_scale))
}

/// Accept a scale only if it is finite and within `[1.0, MAX_SCALE]`.
fn sane_scale(s: f64) -> Option<f64> {
    if s.is_finite() && s >= 1.0 && s <= MAX_SCALE as f64 {
        Some(s)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{combine_wayland_scale, parse_xft_dpi, scale_from_dpi};

    #[test]
    fn parse_extracts_dpi_line() {
        let xrdb = "*customization:\t-color\nXft.dpi:\t192\nXft.antialias:\t1\n";
        assert_eq!(parse_xft_dpi(xrdb), Some(192.0));
    }

    #[test]
    fn parse_tolerates_spaces_and_position() {
        assert_eq!(parse_xft_dpi("Xft.dpi:   96"), Some(96.0));
        assert_eq!(parse_xft_dpi("a:1\n  Xft.dpi:\t144  \nb:2"), Some(144.0));
    }

    #[test]
    fn parse_missing_or_bad_is_none() {
        assert_eq!(parse_xft_dpi(""), None);
        assert_eq!(parse_xft_dpi("Xft.antialias:\t1"), None);
        assert_eq!(parse_xft_dpi("Xft.dpi:\tnope"), None);
        assert_eq!(parse_xft_dpi("Xft.dpi:\t-5"), None);
    }

    #[test]
    fn integer_scales_are_honored() {
        assert_eq!(scale_from_dpi(96.0), 1.0);
        assert_eq!(scale_from_dpi(192.0), 2.0);
        assert_eq!(scale_from_dpi(288.0), 3.0);
        // GNOME sometimes reports 95.9/96.1-ish; snap within epsilon.
        assert_eq!(scale_from_dpi(191.5), 2.0);
    }

    #[test]
    fn fractional_scales_fall_back_to_one() {
        // 1.25 and 1.5 are font-only on X11; do not treat as coordinate scale.
        assert_eq!(scale_from_dpi(120.0), 1.0);
        assert_eq!(scale_from_dpi(144.0), 1.0);
    }

    #[test]
    fn out_of_range_or_bad_dpi_is_one() {
        assert_eq!(scale_from_dpi(0.0), 1.0);
        assert_eq!(scale_from_dpi(-96.0), 1.0);
        assert_eq!(scale_from_dpi(f64::NAN), 1.0);
        // 9× (864 dpi) exceeds MAX_SCALE.
        assert_eq!(scale_from_dpi(864.0), 1.0);
    }

    #[test]
    fn single_output_integer_scale_is_exact() {
        // 3840 physical / 1920 logical = 2.0.
        assert_eq!(
            combine_wayland_scale(1, Some(3840), Some(1920), 2),
            Some(2.0)
        );
    }

    #[test]
    fn single_output_fractional_scale_is_exact() {
        // 1920 physical / 1280 logical = 1.5 — the case wl_output.scale (=2)
        // gets wrong.
        assert_eq!(
            combine_wayland_scale(1, Some(1920), Some(1280), 2),
            Some(1.5)
        );
    }

    #[test]
    fn single_output_missing_logical_falls_back_to_int_scale() {
        assert_eq!(combine_wayland_scale(1, Some(1920), None, 2), Some(2.0));
        assert_eq!(combine_wayland_scale(1, None, Some(1280), 3), Some(3.0));
    }

    #[test]
    fn multi_output_uses_integer_scale_even_with_geometry() {
        // Geometry present but >1 output: a scalar can't represent mixed DPI,
        // so use the integer buffer scale, not the (wrong) single-output ratio.
        assert_eq!(
            combine_wayland_scale(2, Some(1920), Some(1280), 2),
            Some(2.0)
        );
    }

    #[test]
    fn no_geometry_and_no_scale_is_none() {
        assert_eq!(combine_wayland_scale(0, None, None, 0), None);
        assert_eq!(combine_wayland_scale(1, None, None, 0), None);
    }

    #[test]
    fn out_of_range_ratio_is_rejected() {
        // Degenerate logical width → absurd ratio → None (fail closed).
        assert_eq!(combine_wayland_scale(1, Some(1920), Some(1), 0), None);
    }
}
