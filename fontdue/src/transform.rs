//! Drawing a glyph under a linear transform, such as a rotation, about its pen point. The glyph is
//! filled as a path: the raster is sized from the transformed points, not the transformed bounding
//! box, and every point is clamped into it.

use crate::FontRepr;
use crate::font::MAX_DIMENSION;
use crate::outline::{GlyphRef, OutlineInfo, PathEvent, PathSource};
use crate::path::{Affine, Iter, Outline, Transform, TransformedMetrics, fill, split_pen};
use crate::platform::{as_i32_unchecked, ceil, sqrt};
use crate::raster::{BitmapIter, Raster};

/// A point's position in device pixels, relative to the whole-pixel part of the pen: outline units
/// to pen-relative y-down units, then the transform, then the pen's fraction, folded into one
/// affine map.
fn glyph_map(info: &OutlineInfo, scale: f32, transform: Transform, fraction: (f32, f32)) -> Affine {
    let s = scale * info.unit;
    let b = info.bounds;
    // Points are relative to the bounds' top-left corner, y down; the pen is on the baseline.
    let (x0, y0) = (b.xmin * s, -(b.ymin + b.height) * s);
    let [a, bb, c, d] = transform.m;
    Affine {
        ax: a * s,
        bx: bb * s,
        cx: a * x0 + bb * y0 + fraction.0,
        ay: c * s,
        by: d * s,
        cy: c * x0 + d * y0 + fraction.1,
    }
}

impl Outline for &GlyphRef<'_> {
    #[inline(always)]
    fn each(self, f: impl FnMut(PathEvent)) {
        self.visit(f)
    }

    #[inline(always)]
    fn each_first(self, n: usize, mut f: impl FnMut(PathEvent)) {
        let mut index = 0;
        self.visit(|event| {
            if index < n {
                f(event);
            }
            index += 1;
        })
    }
}

/// The shared body of the transformed entry points.
///
/// # Panics
///
/// If `pen` is out of range (see [`split_pen`]), or the transformed glyph does not fit `i32`.
#[inline(always)]
fn rasterize_with(
    canvas: &mut Raster<'_>,
    info: &OutlineInfo,
    scale: f32,
    transform: Transform,
    pen: (f32, f32),
    measure: impl Outline,
    draw: impl Outline,
) -> TransformedMetrics {
    let (pen_x, fx) = split_pen(pen.0);
    let (pen_y, fy) = split_pen(pen.1);
    fill(canvas, glyph_map(info, scale, transform, (fx, fy)), (pen_x, pen_y), measure, draw)
}

/// Rasterizes a glyph under `transform`, about the pen point `pen` in absolute device pixels.
/// `scale` is the font's `scale_factor(px)`: `px` sets the size, and `transform` the shape.
///
/// # Panics
///
/// If `pen` is not finite or does not fit `i32`, or the transformed glyph does not fit `i32`.
#[inline(always)]
pub fn rasterize_transformed(
    canvas: &mut Raster<'_>,
    glyph: &GlyphRef<'_>,
    scale: f32,
    transform: Transform,
    pen: (f32, f32),
) -> TransformedMetrics {
    // Stored points go through an iterator, not `visit`: its closure also serves `dyn` sources,
    // which kept it out of line, called once per point.
    match glyph.path() {
        Some(events) => {
            rasterize_with(canvas, &glyph.info(), scale, transform, pen, Iter(events.clone()), Iter(events))
        }
        None => rasterize_with(canvas, &glyph.info(), scale, transform, pen, glyph, glyph),
    }
}

/// [`rasterize_transformed`] for one glyph of `source`, monomorphized over it.
#[inline(always)]
pub fn rasterize_source_transformed<S: PathSource + ?Sized>(
    canvas: &mut Raster<'_>,
    source: &S,
    glyph: u16,
    scale: f32,
    transform: Transform,
    pen: (f32, f32),
) -> TransformedMetrics {
    let (measure, draw) = (source.points(glyph), source.points(glyph));
    rasterize_with(canvas, &source.info(glyph), scale, transform, pen, Iter(measure), Iter(draw))
}

/// `FontRepr::rasterize_indexed_transformed` over a source, generically, for fonts the macro
/// backs with a store. `scale` is the font's `scale_factor(px)`.
#[doc(hidden)]
#[inline]
pub fn rasterize_source_transformed_indexed<'r, S: PathSource + ?Sized>(
    canvas: &'r mut Raster<'_>,
    source: &S,
    glyph: u16,
    px: f32,
    scale: f32,
    transform: Transform,
    pen: (f32, f32),
) -> (TransformedMetrics, BitmapIter<'r>) {
    if px == 0.0 {
        canvas.resize(0, 0);
        return (TransformedMetrics::default(), canvas.get_bitmap_iter());
    }
    let metrics = rasterize_source_transformed(canvas, source, glyph, scale, transform, pen);
    (metrics, canvas.get_bitmap_iter())
}

/// Glyph indices and unrounded pen offsets along the baseline for one line of text, with
/// kerning: each offset is the previous one plus the previous glyph's advance plus the pair's
/// kerning. No line breaking and no control-character handling. Place glyph `i` with its offset
/// `o` at `origin + transform.apply(o, 0.0)`. After the iterator is exhausted,
/// [`advance`](PenOffsets::advance) is the total advance, for centring.
pub struct PenOffsets<'a, F: FontRepr + ?Sized> {
    font: &'a F,
    chars: core::str::Chars<'a>,
    px: f32,
    scale: f32,
    previous: Option<u16>,
    pen: f32,
}

impl<'a, F: FontRepr + ?Sized> PenOffsets<'a, F> {
    pub fn new(font: &'a F, text: &'a str, px: f32) -> Self {
        PenOffsets {
            font,
            chars: text.chars(),
            px,
            scale: font.scale_factor(px),
            previous: None,
            pen: 0.0,
        }
    }

    /// The pen offset after the glyphs yielded so far.
    pub fn advance(&self) -> f32 {
        self.pen
    }
}

impl<F: FontRepr + ?Sized> Iterator for PenOffsets<'_, F> {
    type Item = (u16, f32);

    fn next(&mut self) -> Option<(u16, f32)> {
        let index = self.font.lookup_glyph_index(self.chars.next()?);
        if let Some(previous) = self.previous {
            self.pen += self.font.horizontal_kern_indexed(previous, index, self.px).unwrap_or(0.0);
        }
        let offset = self.pen;
        self.pen += self.font.get_glyph_at_index(index).info().advance_width * self.scale;
        self.previous = Some(index);
        Some((index, offset))
    }
}

/// Raster buffer length, in f32s and including the three slack slots, that fits every glyph of
/// `text` at `px` under `transform` followed by any rotation, at any pen. For
/// [`Raster::from_slice`] over a buffer sized once.
///
/// A rotated point set's bounding box is no wider than the set's diameter, which is at most the
/// diagonal of the upright bounds, and the transform lengthens that by at most its largest
/// singular value. The pen's fraction and the floor and ceiling add a pixel per side, and one
/// more covers rounding in the transformed points.
pub fn transformed_raster_capacity<F: FontRepr + ?Sized>(
    font: &F,
    text: &str,
    px: f32,
    transform: Transform,
) -> usize {
    let scale = font.scale_factor(px);
    let stretch = transform.stretch();
    let mut most = 0;
    for c in text.chars() {
        let info = font.get_glyph_at_index(font.lookup_glyph_index(c)).info();
        let b = info.bounds.scale(scale * info.unit);
        let side = ceil(stretch * sqrt(b.width * b.width + b.height * b.height)) + 2.0;
        assert!((0.0..=MAX_DIMENSION).contains(&side), "px or transform out of range");
        // SAFETY: the range check bounds `side` inside `i32` and rejects NaN.
        let side = unsafe { as_i32_unchecked(side) } as usize;
        most = most.max(side.checked_mul(side).expect("raster dimensions overflow usize"));
    }
    most.checked_add(3).expect("raster dimensions overflow usize")
}
