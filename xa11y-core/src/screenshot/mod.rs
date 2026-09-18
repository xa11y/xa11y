//! Screenshot capture: pixel-level snapshots of the screen or a region.
//!
//! Screenshot is **separate from** both the accessibility action layer
//! ([`crate::Provider`]) and the input-synthesis layer ([`crate::InputProvider`]).
//! Backends that only capture pixels do not know how to read the a11y tree,
//! synthesise input, or activate windows — they are pure pixel readers.
//!
//! # What you get
//!
//! The caller-facing entry points in the `xa11y` umbrella crate
//! (`xa11y::screenshot()`, `xa11y::screenshot_region()`,
//! `xa11y::screenshot_element()`) all return a [`Screenshot`] carrying raw
//! RGBA8 pixels in **physical** (device) pixels — the same resolution the
//! compositor renders at. On HiDPI displays that means pixel dimensions
//! exceed the logical bounds you passed in; [`Screenshot::scale`] records a
//! compatibility ratio for simple one-display consumers. It is not a desktop
//! transform: use the screenshot's explicit coordinate methods for mixed-DPI,
//! cropped, or resized images. Call [`Screenshot::to_png`] or
//! [`Screenshot::save_png`] to encode.
//!
//! # No auto-activation
//!
//! Capturing an element that is occluded or off-screen returns whatever pixels
//! are at those coordinates — the target window is **not** activated. If you
//! need the element in the foreground, do that explicitly
//! before calling `xa11y::screenshot_element`.
//!
//! # Annotation
//!
//! [`annotate`] draws boxes and tags onto a capture: [`Screenshot::annotate`]
//! takes [`Annotation`]s in logical screen coordinates, plus the logical
//! coordinate the capture's own pixel `(0, 0)` sits at, and returns a new
//! capture with them drawn in. That second argument is why
//! [`ScreenshotProvider::capture_full`] returns a pair: what a full capture
//! covers differs per platform, and so does where it starts. That module is
//! pure pixels — it knows nothing about selectors, providers, or platforms.
//!
//! [`legend`] carries the other half of the result: [`Annotated`],
//! [`LegendEntry`], [`Omission`] and [`OmissionReason`] describe *what* was
//! drawn and what could not be. They are built by
//! `xa11y::screenshot_annotated`, which is where selectors are resolved, and
//! live here so the language bindings that surface them are covered by
//! `cargo xtask check-bindings-parity`.

use std::path::Path;

use crate::element::Rect;
use crate::error::{Error, Result};
use crate::input::Point;

pub mod annotate;
pub mod legend;

pub use annotate::{tag_for, Annotation, ANNOTATION_PALETTE};
pub use legend::{Annotated, LegendEntry, Omission, OmissionReason};

/// One affine piece of a capture's desktop-to-image mapping.
///
/// This is a backend construction detail. Public consumers use
/// [`Screenshot::desktop_to_image`], [`Screenshot::image_to_desktop`], and
/// [`Screenshot::desktop_rect_to_image`] instead.
#[doc(hidden)]
#[allow(
    clippy::exhaustive_structs,
    reason = "Backend construction detail: one affine segment is completely described by its desktop and image rectangles."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureMappingSegment {
    pub desktop: Rect,
    pub image: Rect,
}

/// Mapping metadata frozen at capture time.
///
/// Backends provide one segment per display portion present in the returned
/// image. A layout token and validator are optional; Windows supplies both so
/// conversions fail after a relevant display-layout change instead of using a
/// stale transform.
#[doc(hidden)]
#[derive(Clone)]
pub struct CaptureMapping {
    segments: Vec<CaptureMappingSegment>,
    layout_token: Option<u64>,
    validator: Option<fn(u64) -> Result<bool>>,
}

impl std::fmt::Debug for CaptureMapping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureMapping")
            .field("segments", &self.segments)
            .field("layout_token", &self.layout_token)
            .finish_non_exhaustive()
    }
}

impl CaptureMapping {
    #[doc(hidden)]
    pub fn new(segments: Vec<CaptureMappingSegment>) -> Self {
        Self {
            segments,
            layout_token: None,
            validator: None,
        }
    }

    /// Build the usual one-display mapping from a logical origin, returned
    /// image dimensions, and physical/logical scale.
    #[doc(hidden)]
    pub fn single(origin: Point, image_width: u32, image_height: u32, scale: f32) -> Self {
        let scale = crate::element::sane_scale(f64::from(scale));
        Self::new(vec![CaptureMappingSegment {
            desktop: Rect {
                x: origin.x,
                y: origin.y,
                width: (f64::from(image_width) / scale).round() as u32,
                height: (f64::from(image_height) / scale).round() as u32,
            },
            image: Rect {
                x: 0,
                y: 0,
                width: image_width,
                height: image_height,
            },
        }])
    }

    #[doc(hidden)]
    pub fn with_layout_validation(
        segments: Vec<CaptureMappingSegment>,
        layout_token: u64,
        validator: fn(u64) -> Result<bool>,
    ) -> Self {
        Self {
            segments,
            layout_token: Some(layout_token),
            validator: Some(validator),
        }
    }

    fn validate(&self) -> Result<()> {
        if let (Some(token), Some(validator)) = (self.layout_token, self.validator) {
            if !validator(token)? {
                return Err(Error::Platform {
                    code: -1,
                    message: "screenshot coordinate mapping is stale because the display layout changed; capture a new screenshot".into(),
                });
            }
        }
        Ok(())
    }
}

/// Platform backend trait for screen capture.
///
/// Implementors snapshot pixels from a display or a sub-region. They must
/// return **physical** (device) pixels — never downscaled to logical points —
/// and report the scale factor alongside the pixel buffer.
///
/// # Errors
///
/// - [`Error::PermissionDenied`] when the OS denies the capture permission
///   (e.g. macOS Screen Recording).
/// - [`Error::Unsupported`] when the current session has no capture path
///   (e.g. Linux with neither X11 DISPLAY nor a working Wayland portal).
/// - [`Error::Platform`] for raw OS / FFI failures.
pub trait ScreenshotProvider: Send + Sync {
    /// Capture everything this backend treats as "the screen", and report
    /// **where** those pixels are.
    ///
    /// The returned [`Point`] is the logical screen coordinate that the
    /// capture's pixel `(0, 0)` sits at. It is not always the origin:
    ///
    /// - Windows captures the whole **virtual desktop**, whose top-left is
    ///   `(SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN)` — negative whenever a
    ///   monitor is arranged left of or above the primary one.
    /// - macOS captures one `SCDisplay`, whose `frame.origin` is only `(0, 0)`
    ///   when it is the display at the coordinate-space origin.
    /// - Linux X11 captures the root window, which is at `(0, 0)`.
    ///
    /// Reporting it is the whole reason this method returns a pair. Anything
    /// that maps a logical rectangle (an `Element::bounds`) onto these pixels
    /// — [`Screenshot::annotate`] above all — must subtract this origin
    /// first, and a backend that guessed `(0, 0)` drew every box one
    /// monitor's width out of place with nothing to report it.
    fn capture_full(&self) -> Result<(Screenshot, Point)>;

    /// Capture a sub-rectangle specified in logical screen coordinates
    /// (the same coordinate space as [`Rect`] in `Element::bounds`).
    ///
    /// No origin is returned because `rect` **is** it: an implementation must
    /// capture the pixels at `rect`, so the capture's pixel `(0, 0)` is at
    /// `(rect.x, rect.y)` by contract.
    fn capture_region(&self, rect: Rect) -> Result<Screenshot>;
}

/// A captured image: raw RGBA8 pixels plus dimensions and scale.
///
/// `width` and `height` are in **image** pixels. `scale` is the compatibility ratio of
/// physical to logical (1.0 on standard displays, 2.0 on typical Retina /
/// 1.5/1.75/2.0 on common Windows/Linux HiDPI configurations). A single value
/// cannot describe mixed-DPI displays or non-uniform resizing; use
/// [`Screenshot::desktop_to_image`] and related methods for coordinates.
/// `pixels.len()` equals `width * height * 4`.
///
/// `#[non_exhaustive]`: capture metadata grows — which display the pixels came
/// from, and the colour space they are in, are both things a backend could
/// start reporting. Build one with [`Screenshot::new`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub scale: f32,
    mapping: Option<CaptureMapping>,
}

impl Screenshot {
    /// A capture of `width` × `height` physical pixels in RGBA8.
    ///
    /// No validation here — [`Screenshot::to_png`] is where a `pixels` length
    /// that disagrees with the dimensions is reported, so a backend that
    /// builds one can still hand back a partial buffer for inspection.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>, scale: f32) -> Self {
        Self {
            width,
            height,
            pixels,
            scale,
            mapping: None,
        }
    }

    /// Attach capture-owned coordinate metadata. Intended for platform
    /// backends; ordinary callers receive mapped screenshots from the public
    /// capture functions.
    #[doc(hidden)]
    #[must_use]
    pub fn with_mapping(mut self, mapping: CaptureMapping) -> Self {
        self.mapping = Some(mapping);
        self
    }

    /// Whether this image can convert between desktop coordinates and its
    /// actual returned pixels.
    #[must_use]
    pub fn mapping_available(&self) -> bool {
        self.mapping.is_some()
    }

    /// Convert a desktop point to a pixel in this returned image.
    ///
    /// Coordinates use pixel centres and half-open image bounds: `(0, 0)` is
    /// the centre of the top-left pixel, and `x == width` is outside.
    pub fn desktop_to_image(&self, point: Point) -> Result<Point> {
        let mapping = self.mapping()?;
        mapping.validate()?;
        mapping
            .segments
            .iter()
            .find_map(|segment| map_point(point, segment.desktop, segment.image))
            .ok_or_else(|| mapping_error("desktop point is outside the captured display areas"))
    }

    /// Convert a pixel in this returned image to desktop coordinates.
    ///
    /// The result can be passed to simulated input. If the backend recorded a
    /// display-layout identity, the conversion first verifies that the layout
    /// still matches the one captured.
    pub fn image_to_desktop(&self, point: Point) -> Result<Point> {
        if point.x < 0
            || point.y < 0
            || point.x as u32 >= self.width
            || point.y as u32 >= self.height
        {
            return Err(mapping_error("image point is outside the screenshot"));
        }
        let mapping = self.mapping()?;
        mapping.validate()?;
        mapping
            .segments
            .iter()
            .find_map(|segment| map_point(point, segment.image, segment.desktop))
            .ok_or_else(|| mapping_error("image point does not belong to a captured display"))
    }

    /// Convert a desktop rectangle into the image rectangles it covers.
    ///
    /// A rectangle crossing independently scaled displays produces one image
    /// rectangle per display. Empty intersections are omitted. Returning a
    /// vector is deliberate: one scalar scale and one rectangle cannot encode
    /// a mixed-DPI seam without guessing.
    pub fn desktop_rect_to_image(&self, rect: Rect) -> Result<Vec<Rect>> {
        let mapping = self.mapping()?;
        mapping.validate()?;
        Ok(mapping
            .segments
            .iter()
            .filter_map(|segment| map_rect(rect, segment.desktop, segment.image))
            .collect())
    }

    /// Crop using image-pixel coordinates, preserving and translating the
    /// capture mapping for the returned pixels.
    pub fn crop(&self, rect: Rect) -> Result<Screenshot> {
        let crop = intersect_rect(
            rect,
            Rect {
                x: 0,
                y: 0,
                width: self.width,
                height: self.height,
            },
        )
        .ok_or_else(|| mapping_error("crop rectangle is outside the screenshot or empty"))?;
        let expected = expected_len(self.width, self.height)?;
        if self.pixels.len() != expected {
            return Err(buffer_mismatch(self));
        }
        let mut pixels = Vec::with_capacity(expected_len(crop.width, crop.height)?);
        for row in 0..crop.height {
            let start = (((crop.y as u32 + row) * self.width + crop.x as u32) * 4) as usize;
            let end = start + crop.width as usize * 4;
            pixels.extend_from_slice(&self.pixels[start..end]);
        }
        let mapping = self.mapping.as_ref().map(|mapping| CaptureMapping {
            segments: mapping
                .segments
                .iter()
                .filter_map(|segment| {
                    let image = intersect_rect(segment.image, crop)?;
                    Some(CaptureMappingSegment {
                        desktop: map_rect(image, segment.image, segment.desktop)?,
                        image: Rect {
                            x: image.x - crop.x,
                            y: image.y - crop.y,
                            width: image.width,
                            height: image.height,
                        },
                    })
                })
                .collect(),
            layout_token: mapping.layout_token,
            validator: mapping.validator,
        });
        Ok(Screenshot {
            width: crop.width,
            height: crop.height,
            pixels,
            scale: self.scale,
            mapping,
        })
    }

    /// Resize to image-pixel dimensions using nearest-neighbour sampling,
    /// scaling the capture mapping to the actual returned image.
    pub fn resize(&self, width: u32, height: u32) -> Result<Screenshot> {
        if width == 0 || height == 0 {
            return Err(mapping_error("resize dimensions must be non-zero"));
        }
        if self.width == 0 || self.height == 0 {
            return Err(mapping_error("cannot resize an empty screenshot"));
        }
        let expected = expected_len(self.width, self.height)?;
        if self.pixels.len() != expected {
            return Err(buffer_mismatch(self));
        }
        let mut pixels = vec![0; expected_len(width, height)?];
        for y in 0..height {
            let sy = (u64::from(y) * u64::from(self.height) / u64::from(height)) as u32;
            for x in 0..width {
                let sx = (u64::from(x) * u64::from(self.width) / u64::from(width)) as u32;
                let src = ((sy * self.width + sx) * 4) as usize;
                let dst = ((y * width + x) * 4) as usize;
                pixels[dst..dst + 4].copy_from_slice(&self.pixels[src..src + 4]);
            }
        }
        let mapping = self.mapping.as_ref().map(|mapping| CaptureMapping {
            segments: mapping
                .segments
                .iter()
                .map(|segment| CaptureMappingSegment {
                    desktop: segment.desktop,
                    image: scale_rect(segment.image, self.width, self.height, width, height),
                })
                .collect(),
            layout_token: mapping.layout_token,
            validator: mapping.validator,
        });
        Ok(Screenshot {
            width,
            height,
            pixels,
            scale: self.scale * width as f32 / self.width.max(1) as f32,
            mapping,
        })
    }

    fn mapping(&self) -> Result<&CaptureMapping> {
        self.mapping.as_ref().ok_or_else(|| mapping_error(
            "screenshot has no coordinate mapping; it was constructed from raw pixels rather than captured from a desktop",
        ))
    }

    /// Encode as PNG and return the bytes.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let expected = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| Error::Platform {
                code: -1,
                message: "screenshot dimensions overflow".into(),
            })?;
        if self.pixels.len() != expected {
            return Err(Error::Platform {
                code: -1,
                message: format!(
                    "screenshot pixel buffer size {} does not match {}x{} RGBA ({} bytes)",
                    self.pixels.len(),
                    self.width,
                    self.height,
                    expected
                ),
            });
        }

        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(png_err)?;
            writer.write_image_data(&self.pixels).map_err(png_err)?;
        }
        Ok(out)
    }

    /// Encode as PNG and write to `path`.
    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<()> {
        let bytes = self.to_png()?;
        std::fs::write(path, bytes).map_err(|e| Error::Platform {
            code: e.raw_os_error().unwrap_or(-1) as i64,
            message: format!("save_png: {e}"),
        })
    }
}

fn expected_len(width: u32, height: u32) -> Result<usize> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| mapping_error("screenshot dimensions overflow"))
}

fn buffer_mismatch(shot: &Screenshot) -> Error {
    Error::Platform {
        code: -1,
        message: format!(
            "screenshot pixel buffer size {} does not match {}x{} RGBA",
            shot.pixels.len(),
            shot.width,
            shot.height
        ),
    }
}

fn mapping_error(message: &str) -> Error {
    Error::Platform {
        code: -1,
        message: message.into(),
    }
}

fn contains(rect: Rect, point: Point) -> bool {
    let right = i64::from(rect.x) + i64::from(rect.width);
    let bottom = i64::from(rect.y) + i64::from(rect.height);
    i64::from(point.x) >= i64::from(rect.x)
        && i64::from(point.y) >= i64::from(rect.y)
        && i64::from(point.x) < right
        && i64::from(point.y) < bottom
}

fn map_point(point: Point, from: Rect, to: Rect) -> Option<Point> {
    if !contains(from, point) || from.width == 0 || from.height == 0 {
        return None;
    }
    let x = i64::from(to.x)
        + ((i64::from(point.x) - i64::from(from.x)) * i64::from(to.width) / i64::from(from.width));
    let y = i64::from(to.y)
        + ((i64::from(point.y) - i64::from(from.y)) * i64::from(to.height)
            / i64::from(from.height));
    Some(Point::new(
        x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
    ))
}

fn intersect_rect(a: Rect, b: Rect) -> Option<Rect> {
    let left = i64::from(a.x).max(i64::from(b.x));
    let top = i64::from(a.y).max(i64::from(b.y));
    let right = (i64::from(a.x) + i64::from(a.width)).min(i64::from(b.x) + i64::from(b.width));
    let bottom = (i64::from(a.y) + i64::from(a.height)).min(i64::from(b.y) + i64::from(b.height));
    if right <= left || bottom <= top {
        return None;
    }
    Some(Rect {
        x: left as i32,
        y: top as i32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

fn map_rect(rect: Rect, from: Rect, to: Rect) -> Option<Rect> {
    let clipped = intersect_rect(rect, from)?;
    if from.width == 0 || from.height == 0 {
        return None;
    }
    let map_x = |x: i64| {
        i64::from(to.x) + (x - i64::from(from.x)) * i64::from(to.width) / i64::from(from.width)
    };
    let map_y = |y: i64| {
        i64::from(to.y) + (y - i64::from(from.y)) * i64::from(to.height) / i64::from(from.height)
    };
    let x0 = map_x(i64::from(clipped.x));
    let y0 = map_y(i64::from(clipped.y));
    let x1 = map_x(i64::from(clipped.x) + i64::from(clipped.width));
    let y1 = map_y(i64::from(clipped.y) + i64::from(clipped.height));
    Some(Rect {
        x: clamp_i32(x0),
        y: clamp_i32(y0),
        width: clamp_u32((x1 - x0).max(0)),
        height: clamp_u32((y1 - y0).max(0)),
    })
}

fn scale_rect(rect: Rect, old_w: u32, old_h: u32, new_w: u32, new_h: u32) -> Rect {
    let x0 = i64::from(rect.x) * i64::from(new_w) / i64::from(old_w.max(1));
    let y0 = i64::from(rect.y) * i64::from(new_h) / i64::from(old_h.max(1));
    let x1 =
        (i64::from(rect.x) + i64::from(rect.width)) * i64::from(new_w) / i64::from(old_w.max(1));
    let y1 =
        (i64::from(rect.y) + i64::from(rect.height)) * i64::from(new_h) / i64::from(old_h.max(1));
    Rect {
        x: clamp_i32(x0),
        y: clamp_i32(y0),
        width: clamp_u32((x1 - x0).max(0)),
        height: clamp_u32((y1 - y0).max(0)),
    }
}

fn clamp_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn clamp_u32(value: i64) -> u32 {
    value.clamp(0, i64::from(u32::MAX)) as u32
}

fn png_err(e: png::EncodingError) -> Error {
    Error::Platform {
        code: -1,
        message: format!("png encode: {e}"),
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::*;

    fn segment(desktop: Rect, image: Rect) -> CaptureMappingSegment {
        CaptureMappingSegment { desktop, image }
    }

    fn blank(width: u32, height: u32, mapping: CaptureMapping) -> Screenshot {
        Screenshot::new(
            width,
            height,
            vec![0; width as usize * height as usize * 4],
            1.0,
        )
        .with_mapping(mapping)
    }

    #[test]
    fn issue_425_equal_dpi_secondary_maps_inside_full_capture() {
        let shot = blank(
            7680,
            2160,
            CaptureMapping::new(vec![
                segment(
                    Rect {
                        x: 0,
                        y: 0,
                        width: 1920,
                        height: 1080,
                    },
                    Rect {
                        x: 0,
                        y: 0,
                        width: 3840,
                        height: 2160,
                    },
                ),
                segment(
                    Rect {
                        x: 3840,
                        y: 0,
                        width: 1920,
                        height: 1080,
                    },
                    Rect {
                        x: 3840,
                        y: 0,
                        width: 3840,
                        height: 2160,
                    },
                ),
            ]),
        );
        let mapped = shot
            .desktop_rect_to_image(Rect {
                x: 3890,
                y: 50,
                width: 20,
                height: 20,
            })
            .expect("mapping must be available");
        assert_eq!(
            mapped,
            vec![Rect {
                x: 3940,
                y: 100,
                width: 40,
                height: 40
            }]
        );

        let (_, skipped) = shot
            .annotate(
                &[Annotation::new(
                    Rect {
                        x: 3890,
                        y: 50,
                        width: 20,
                        height: 20,
                    },
                    "A1",
                )],
                Point::new(0, 0),
            )
            .expect("mapped annotation must draw");
        assert!(skipped.is_empty());
    }

    #[test]
    fn a_desktop_rect_crossing_mixed_dpi_displays_returns_two_image_rects() {
        let shot = blank(
            4480,
            1080,
            CaptureMapping::new(vec![
                segment(
                    Rect {
                        x: 0,
                        y: 0,
                        width: 1920,
                        height: 1080,
                    },
                    Rect {
                        x: 0,
                        y: 0,
                        width: 1920,
                        height: 1080,
                    },
                ),
                segment(
                    Rect {
                        x: 1920,
                        y: 0,
                        width: 1280,
                        height: 540,
                    },
                    Rect {
                        x: 1920,
                        y: 0,
                        width: 2560,
                        height: 1080,
                    },
                ),
            ]),
        );
        assert_eq!(
            shot.desktop_rect_to_image(Rect {
                x: 1800,
                y: 100,
                width: 260,
                height: 100
            })
            .unwrap(),
            vec![
                Rect {
                    x: 1800,
                    y: 100,
                    width: 120,
                    height: 100
                },
                Rect {
                    x: 1920,
                    y: 200,
                    width: 280,
                    height: 200
                },
            ]
        );
    }

    #[test]
    fn negative_origin_and_fractional_scale_round_trip() {
        let shot = blank(
            1920,
            1080,
            CaptureMapping::new(vec![segment(
                Rect {
                    x: -1920,
                    y: -100,
                    width: 1280,
                    height: 720,
                },
                Rect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
            )]),
        );
        assert_eq!(
            shot.desktop_to_image(Point::new(-1280, 260)).unwrap(),
            Point::new(960, 540)
        );
        assert_eq!(
            shot.image_to_desktop(Point::new(960, 540)).unwrap(),
            Point::new(-1280, 260)
        );
    }

    #[test]
    fn crop_and_resize_update_the_mapping_for_actual_returned_pixels() {
        let shot = blank(
            200,
            100,
            CaptureMapping::single(Point::new(-50, 10), 200, 100, 2.0),
        );
        let transformed = shot
            .crop(Rect {
                x: 40,
                y: 20,
                width: 100,
                height: 60,
            })
            .unwrap()
            .resize(50, 30)
            .unwrap();
        assert_eq!(
            transformed.desktop_to_image(Point::new(-20, 30)).unwrap(),
            Point::new(10, 10)
        );
        assert_eq!(
            transformed.image_to_desktop(Point::new(10, 10)).unwrap(),
            Point::new(-20, 30)
        );
    }

    fn stale(_: u64) -> Result<bool> {
        Ok(false)
    }

    #[test]
    fn stale_layout_is_rejected_before_a_point_is_returned_for_input() {
        let mapping = CaptureMapping::with_layout_validation(
            vec![segment(
                Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                },
                Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                },
            )],
            7,
            stale,
        );
        let err = blank(10, 10, mapping)
            .image_to_desktop(Point::new(1, 1))
            .expect_err("a changed layout must invalidate capture coordinates");
        assert!(err.to_string().contains("display layout changed"));
    }
}

// The public entry points — `xa11y::screenshot()`, `screenshot_region()`,
// `screenshot_element()` — live in the umbrella crate (`xa11y/src/lib.rs`)
// so they can construct the platform-specific `ScreenshotProvider` backend
// and memoize it across calls. Keep this file focused on the data (Screenshot)
// and the backend trait (ScreenshotProvider); the umbrella crate composes
// them into the caller-facing API.
