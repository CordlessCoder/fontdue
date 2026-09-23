//! Drawing a glyph under a linear transform, such as a rotation, about its pen point.
//!
//! The raster is sized from the transformed points themselves, not the transformed bounding box,
//! and every point is clamped into it before the line walk. So a transform cannot make the raster
//! write out of bounds, whatever the source yields.

use crate::FontRepr;
use crate::font::MAX_DIMENSION;
use crate::math::Point;
use crate::outline::{GlyphRef, OutlineInfo, PathEvent, PathSource};
use crate::platform::{as_i32_unchecked, ceil, floor, sqrt};
use crate::raster::{BitmapIter, Raster, Sink};
use core::mem::MaybeUninit;

/// Points the transformed draw keeps from its measuring pass, 3 KB of stack. Octowhere's largest
/// glyph has 70 segments; a larger glyph's first `count - RING` points are visited twice.
const RING: usize = 256;

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

/// A glyph's outline, visited once. The transformed draw takes two, one to measure the glyph
/// and one to draw it, both made before the raster is sized.
pub(crate) trait Outline {
    fn each(self, f: impl FnMut(PathEvent));

    /// The first `n` events only. A streamed source stops decoding after them.
    fn each_first(self, n: usize, f: impl FnMut(PathEvent));
}

pub(crate) struct Iter<I>(pub I);

impl<I: Iterator<Item = PathEvent>> Outline for Iter<I> {
    #[inline(always)]
    fn each(self, mut f: impl FnMut(PathEvent)) {
        for event in self.0 {
            f(event);
        }
    }

    #[inline(always)]
    fn each_first(self, n: usize, mut f: impl FnMut(PathEvent)) {
        for event in self.0.take(n) {
            f(event);
        }
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

/// The least coordinate `clamp` gives, 2^-100, unless `hi` is below it. Distinct floats at least
/// this large differ by at least 2^-123, so every delta between clamped points is zero or normal,
/// as the unguarded line walk needs.
const LEAST: i32 = (127 - 100) << 23;

/// `v` clamped into `[2^-100, hi]`, or to `hi` when `hi` is smaller, for a finite `hi >= 0`.
/// Non-negative floats order as their bits do as integers and negative ones as negative
/// integers, so this is an integer max and min. A NaN lands at an end by its sign.
#[inline(always)]
fn clamp(v: f32, hi: f32) -> f32 {
    f32::from_bits((v.to_bits() as i32).max(LEAST).min(hi.to_bits() as i32) as u32)
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
    measure: impl Outline,
    draw: impl Outline,
) -> TransformedMetrics {
    let (pen_x, fx) = split_pen(pen.0);
    let (pen_y, fy) = split_pen(pen.1);
    let map = Map::new(info, scale, transform, (fx, fy));

    let (mut min_x, mut min_y, mut max_x, mut max_y) =
        (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    // The last `RING` points, transformed, with whether each starts a contour: point `i` is in
    // slot `i % RING`. A glyph that fits is visited once, and one that does not is visited again
    // only up to where the ring starts, so a streamed source decodes nothing after that twice.
    // Written before read, in slots `first..count` modulo `RING`.
    let mut ring = [MaybeUninit::<([f32; 2], bool)>::uninit(); RING];
    let mut count = 0;
    measure.each(|event| {
        let ([x, y], start) = match event {
            PathEvent::MoveTo(p) => (p, true),
            PathEvent::LineTo(p) => (p, false),
        };
        let (qx, qy) = map.apply(x, y);
        ring[count % RING].write(([qx, qy], start));
        count += 1;
        if qx < min_x {
            min_x = qx;
        }
        if qx > max_x {
            max_x = qx;
        }
        if qy < min_y {
            min_y = qy;
        }
        if qy > max_y {
            max_y = qy;
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
    let first = count.saturating_sub(RING);
    // Before the first `MoveTo` the previous point is the raster's corner, which is inside it.
    let mut last = Point::new(0.0, 0.0);
    if first > 0 {
        // The points the ring overwrote, from a second visit that stops where the ring begins.
        // The origin is folded into the map, so the walk holds six floats across points rather
        // than ten; with ten the crossing loop ran a register short. The fold rounds differently
        // from the measuring pass, which the clamp absorbs.
        let map = Map {
            cx: map.cx - ox,
            cy: map.cy - oy,
            ..map
        };
        // Its own sink and previous point: the visit can reach a `dyn` source, which keeps what
        // the closure captures in memory, and the ring loop would then read them from there.
        let mut sink = Sink::new(canvas, 1.0, 1.0, 0.0, 0.0);
        let mut previous = last;
        draw.each_first(first, |event| {
            let ([x, y], start) = match event {
                PathEvent::MoveTo(p) => (p, true),
                PathEvent::LineTo(p) => (p, false),
            };
            let (qx, qy) = map.apply(x, y);
            let p = Point::new(clamp(qx, w), clamp(qy, h));
            if !start {
                sink.edge(previous, p);
            }
            previous = p;
        });
        last = previous;
    }
    let mut sink = Sink::new(canvas, 1.0, 1.0, 0.0, 0.0);
    for k in first..count {
        // SAFETY: the measuring pass wrote slot `k % RING` for every `k` in `first..count`.
        let ([qx, qy], start) = unsafe { ring[k % RING].assume_init() };
        let p = Point::new(clamp(qx - ox, w), clamp(qy - oy, h));
        if !start {
            sink.edge(last, p);
        }
        last = p;
    }
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

#[cfg(test)]
mod tests {
    use super::{LEAST, clamp};

    /// The integer clamp against the float comparison it replaces, NaN included.
    #[test]
    fn clamp_matches_float_comparison() {
        let least = f32::from_bits(LEAST as u32);
        let values = [
            0.0,
            -0.0,
            1e-40,
            -1e-40,
            0.5,
            1.0,
            6.99,
            7.0,
            7.01,
            -3.0,
            f32::MAX,
            f32::MIN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        for hi in [0.0f32, 1.0, 7.0, 1e30] {
            for v in values {
                let want = v.max(least).min(hi);
                assert_eq!(clamp(v, hi).to_bits(), want.to_bits(), "{v} into [2^-100, {hi}]");
            }
            for nan in [f32::NAN, -f32::NAN] {
                let got = clamp(nan, hi);
                assert!(got == least.min(hi) || got == hi, "{nan} into [2^-100, {hi}] gave {got}");
            }
        }
    }
}
