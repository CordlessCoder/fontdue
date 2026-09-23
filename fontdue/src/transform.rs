//! Drawing a glyph under a linear transform, such as a rotation, about its pen point.
//!
//! The raster is sized from the transformed points themselves, not the transformed bounding box,
//! and every point is clamped into it before the line walk. So a transform cannot make the raster
//! write out of bounds, whatever the source yields.

use crate::FontRepr;
use crate::font::MAX_DIMENSION;
use crate::math::{Line, Point};
use crate::outline::{GlyphRef, OutlineInfo, SegmentSource};
use crate::platform::{abs, as_i32_unchecked, ceil, floor, sqrt};
use crate::raster::{BitmapIter, Raster, Sink};

/// A linear map in y-down device coordinates, applied about the pen point: `x' = a·x + b·y`,
/// `y' = c·x + d·y`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Transform {
    m: [f32; 4],
}

impl Transform {
    pub const IDENTITY: Transform = Transform {
        m: [1.0, 0.0, 0.0, 1.0],
    };

    /// # Panics
    ///
    /// If an entry is not finite, or the determinant is zero or not finite. A zero determinant
    /// collapses the glyph onto a line.
    pub fn new(a: f32, b: f32, c: f32, d: f32) -> Transform {
        let det = a * d - b * c;
        assert!(
            a.is_finite() && b.is_finite() && c.is_finite() && d.is_finite() && det.is_finite() && det != 0.0,
            "transform must be finite and invertible"
        );
        Transform {
            m: [a, b, c, d],
        }
    }

    /// Turns clockwise on screen for a positive angle, since y grows down. The caller supplies
    /// the angle's cosine and sine.
    pub fn rotation(cos: f32, sin: f32) -> Transform {
        Transform::new(cos, -sin, sin, cos)
    }

    /// This transform, then `next`: the product `next · self`.
    pub fn then(self, next: Transform) -> Transform {
        let [a, b, c, d] = self.m;
        let [p, q, r, s] = next.m;
        Transform::new(p * a + q * c, p * b + q * d, r * a + s * c, r * b + s * d)
    }

    /// Maps a vector the way the rasterizer maps outlines, for placing glyphs along a transformed
    /// baseline with the same convention.
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d] = self.m;
        (a * x + b * y, c * x + d * y)
    }

    /// The most the transform lengthens any vector by: its largest singular value.
    fn stretch(&self) -> f32 {
        let [a, b, c, d] = self.m;
        let sum = a * a + b * b + c * c + d * d;
        let det = a * d - b * c;
        sqrt((sum + sqrt((sum * sum - 4.0 * det * det).max(0.0))) / 2.0)
    }
}

/// Where a transformed glyph's bitmap lies.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct TransformedMetrics {
    /// Absolute device position of the bitmap's top-left pixel.
    pub x: i32,
    pub y: i32,
    /// The bitmap is row-major, `width` pixels per row.
    pub width: usize,
    pub height: usize,
}

/// A point's position in device pixels, relative to the whole-pixel part of the pen: outline
/// units to pen-relative y-down units, then the transform, then the pen's fraction, folded into
/// one affine map.
#[derive(Copy, Clone)]
struct Map {
    ax: f32,
    bx: f32,
    cx: f32,
    ay: f32,
    by: f32,
    cy: f32,
}

impl Map {
    fn new(info: &OutlineInfo, scale: f32, transform: Transform, fraction: (f32, f32)) -> Map {
        let s = scale * info.unit;
        let b = info.bounds;
        // Points are relative to the bounds' top-left corner, y down; the pen is on the baseline.
        let (x0, y0) = (b.xmin * s, -(b.ymin + b.height) * s);
        let [a, bb, c, d] = transform.m;
        Map {
            ax: a * s,
            bx: bb * s,
            cx: a * x0 + bb * y0 + fraction.0,
            ay: c * s,
            by: d * s,
            cy: c * x0 + d * y0 + fraction.1,
        }
    }

    #[inline(always)]
    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (self.ax * x + self.bx * y + self.cx, self.ay * x + self.by * y + self.cy)
    }
}

/// A glyph's segments, visited once. The transformed draw takes two, one to measure the glyph
/// and one to draw it, both made before the raster is sized.
pub(crate) trait Segments {
    fn each(self, f: impl FnMut([f32; 4]));
}

pub(crate) struct Iter<I>(pub I);

impl<I: Iterator<Item = [f32; 4]>> Segments for Iter<I> {
    #[inline(always)]
    fn each(self, mut f: impl FnMut([f32; 4])) {
        for segment in self.0 {
            f(segment);
        }
    }
}

impl Segments for &GlyphRef<'_> {
    #[inline(always)]
    fn each(self, f: impl FnMut([f32; 4])) {
        self.visit(f)
    }
}

/// `v` clamped into `[0, hi]`, with NaN going to 0.
#[inline(always)]
fn clamp(v: f32, hi: f32) -> f32 {
    if v >= 0.0 {
        if v <= hi {
            v
        } else {
            hi
        }
    } else {
        0.0
    }
}

/// Splits a pen coordinate into its whole-pixel part, as an `i32`, and its fraction.
///
/// # Panics
///
/// If the coordinate is not finite or its floor does not fit `i32`.
fn split_pen(pen: f32) -> (i32, f32) {
    let whole = floor(pen);
    assert!((-MAX_DIMENSION..=MAX_DIMENSION).contains(&whole), "pen out of range");
    // SAFETY: the range check bounds `whole` inside `i32` and rejects NaN and infinities.
    (unsafe { as_i32_unchecked(whole) }, pen - whole)
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
    measure: impl Segments,
    draw: impl Segments,
) -> TransformedMetrics {
    let (pen_x, fx) = split_pen(pen.0);
    let (pen_y, fy) = split_pen(pen.1);
    let map = Map::new(info, scale, transform, (fx, fy));

    let (mut min_x, mut min_y, mut max_x, mut max_y) =
        (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    measure.each(|[x0, y0, x1, y1]| {
        for (x, y) in [(x0, y0), (x1, y1)] {
            let (qx, qy) = map.apply(x, y);
            min_x = min_x.min(qx);
            min_y = min_y.min(qy);
            max_x = max_x.max(qx);
            max_y = max_y.max(qy);
        }
    });
    if !(min_x <= max_x) {
        canvas.resize(0, 0);
        return TransformedMetrics {
            x: pen_x,
            y: pen_y,
            width: 0,
            height: 0,
        };
    }
    let (ox, oy) = (floor(min_x), floor(min_y));
    let (w, h) = (ceil(max_x - ox), ceil(max_y - oy));
    // `resize` sizes the buffer from these, and the line walk indexes it without checks.
    assert!(
        (-MAX_DIMENSION..=MAX_DIMENSION).contains(&ox)
            && (-MAX_DIMENSION..=MAX_DIMENSION).contains(&oy)
            && (0.0..=MAX_DIMENSION).contains(&w)
            && (0.0..=MAX_DIMENSION).contains(&h),
        "px or transform out of range: the transformed glyph does not fit i32"
    );
    // SAFETY: the range check bounds all four inside `i32` and rejects NaN.
    let (x, y, width, height) = unsafe {
        (
            as_i32_unchecked(ox),
            as_i32_unchecked(oy),
            as_i32_unchecked(w) as usize,
            as_i32_unchecked(h) as usize,
        )
    };
    let metrics = TransformedMetrics {
        x: pen_x.checked_add(x).expect("pen out of range"),
        y: pen_y.checked_add(y).expect("pen out of range"),
        width,
        height,
    };
    canvas.resize(width, height);
    let mut sink = Sink::new(canvas, 1.0, 1.0, 0.0, 0.0);
    draw.each(|[x0, y0, x1, y1]| {
        let place = |x: f32, y: f32| {
            let (qx, qy) = map.apply(x, y);
            // The same map as the measuring pass, so the clamp is a no-op there; it is what makes
            // the walk stay in bounds when a source yields something else the second time.
            debug_assert!(qx - ox >= 0.0 && qx - ox <= w && qy - oy >= 0.0 && qy - oy <= h);
            Point::new(clamp(qx - ox, w), clamp(qy - oy, h))
        };
        sink.placed(place(x0, y0), place(x1, y1));
    });
    metrics
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
    rasterize_with(canvas, &glyph.info(), scale, transform, pen, glyph, glyph)
}

/// [`rasterize_transformed`] for one glyph of `source`, monomorphized over it.
#[inline(always)]
pub fn rasterize_source_transformed<S: SegmentSource + ?Sized>(
    canvas: &mut Raster<'_>,
    source: &S,
    glyph: u16,
    scale: f32,
    transform: Transform,
    pen: (f32, f32),
) -> TransformedMetrics {
    let (measure, draw) = (source.segments(glyph), source.segments(glyph));
    rasterize_with(canvas, &source.info(glyph), scale, transform, pen, Iter(measure), Iter(draw))
}

/// `FontRepr::rasterize_indexed_transformed` over a source, generically, for fonts the macro
/// backs with a store. `scale` is the font's `scale_factor(px)`.
#[doc(hidden)]
#[inline]
pub fn rasterize_source_transformed_indexed<'r, S: SegmentSource + ?Sized>(
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

/// A transformed segment's line, or `None` when it has no vertical extent. A delta too small to
/// be a normal float counts as zero, since the reciprocal is only accurate for normal values.
#[inline(always)]
pub(crate) fn placed_line(start: Point, end: Point) -> Option<Line> {
    if !(abs(end.y - start.y) >= f32::MIN_POSITIVE) {
        return None;
    }
    let end = if abs(end.x - start.x) >= f32::MIN_POSITIVE {
        end
    } else {
        Point::new(start.x, end.y)
    };
    Some(Line::at_draw(start, end))
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
