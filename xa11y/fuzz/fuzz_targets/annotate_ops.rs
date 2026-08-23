//! Fuzz target for the annotation drawing layer (`xa11y_core::screenshot::annotate`).
//!
//! `Screenshot::annotate` is where `i32` rectangles, an `f32` scale factor, and
//! a `Vec<u8>` pixel buffer meet: every coordinate ends up as an index into
//! that buffer, and a single unchecked product is an out-of-bounds write. This
//! target drives it with arbitrary rects, origins, scales, tags, and image
//! dimensions, and asserts it never panics and never resizes or reshapes the
//! buffer it was handed.
//!
//! `tag_for` is fuzzed alongside it — it is the one place the tag format is
//! decided, and it is reached with caller-supplied indices.
#![no_main]

use libfuzzer_sys::fuzz_target;

use arbitrary::Arbitrary;
use xa11y::screenshot::{tag_for, Annotation, ANNOTATION_PALETTE};
use xa11y::{Point, Rect, Screenshot};

/// A rectangle with no constraints at all: `i32::MIN` origins and `u32::MAX`
/// sizes are exactly the inputs that overflow a naive implementation.
#[derive(Arbitrary, Debug)]
struct FuzzRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Arbitrary, Debug)]
struct FuzzAnnotation {
    rect: FuzzRect,
    /// Arbitrary text: most of it has no glyph in the 5×7 font, which is the
    /// path that has to produce no badge rather than a zero-width one.
    tag: String,
    /// Indexes `ANNOTATION_PALETTE`, plus an arbitrary RGB triple so colours
    /// outside the palette are exercised too.
    palette_index: u8,
    color: Option<[u8; 3]>,
}

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    /// Image dimensions, kept small enough to allocate — the arithmetic risk
    /// is in the coordinates, not in the buffer size.
    width: u8,
    height: u8,
    /// Raw bits, so `NaN`, the infinities, subnormals, and huge finite scales
    /// all reach `Screenshot::scale`.
    scale_bits: u32,
    origin_x: i32,
    origin_y: i32,
    annotations: Vec<FuzzAnnotation>,
    /// Arguments for `tag_for`, which is 1-based and must not panic on 0.
    tag_group: usize,
    tag_index: usize,
}

fuzz_target!(|input: FuzzInput| {
    // ── tag_for: the single definition of the tag format ─────────────────────
    let tag = tag_for(input.tag_group, input.tag_index);
    assert!(!tag.is_empty(), "tag_for must always produce a tag");

    // ── Build a capture whose buffer really is width × height × 4 ────────────
    let width = u32::from(input.width);
    let height = u32::from(input.height);
    let len = (width as usize) * (height as usize) * 4;
    let scale = f32::from_bits(input.scale_bits);
    let shot = Screenshot::new(width, height, vec![0_u8; len], scale);

    // ── Arbitrary annotations, capped so one input stays cheap ───────────────
    let annotations: Vec<Annotation> = input
        .annotations
        .iter()
        .take(24)
        .map(|a| {
            let rect = Rect {
                x: a.rect.x,
                y: a.rect.y,
                width: a.rect.width,
                height: a.rect.height,
            };
            let color = a.color.unwrap_or(
                ANNOTATION_PALETTE[usize::from(a.palette_index) % ANNOTATION_PALETTE.len()],
            );
            Annotation::new(rect, a.tag.as_str()).color(color)
        })
        .collect();

    let origin = Point::new(input.origin_x, input.origin_y);

    // A buffer that matches its dimensions must always annotate.
    let (out, skipped) = shot
        .annotate(&annotations, origin)
        .expect("annotate must succeed on a well-formed capture");

    // The output is a copy: same shape, same length, nothing reallocated
    // around the caller's dimensions.
    assert_eq!(out.width, shot.width);
    assert_eq!(out.height, shot.height);
    assert_eq!(out.pixels.len(), shot.pixels.len());
    assert_eq!(
        shot.pixels.len(),
        len,
        "annotate must not mutate the receiver"
    );

    // Skipped indices address the input slice and are reported in order.
    assert!(skipped.len() <= annotations.len());
    assert!(skipped.windows(2).all(|w| w[0] < w[1]));
    assert!(skipped.iter().all(|&i| i < annotations.len()));

    // Encoding the result exercises the dimensions-vs-buffer agreement again.
    let _ = out.to_png();

    // A capture whose buffer disagrees with its dimensions is an error, never
    // a panic and never a partial write.
    if len > 0 {
        let short = Screenshot::new(width, height, vec![0_u8; len - 1], scale);
        assert!(short.annotate(&annotations, origin).is_err());
    }
});
