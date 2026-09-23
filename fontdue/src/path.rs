//! Filling a path under a transform, with the raster sized from the path itself.
//!
//! The raster is sized from the placed points, and every point is clamped into it before the line
//! walk. So neither the transform nor the path can make the raster write out of bounds.

use crate::font::MAX_DIMENSION;
use crate::math::Point;
use crate::outline::PathEvent;
use crate::platform::{as_i32_unchecked, ceil, floor, sqrt};
use crate::raster::{Lines, Raster};
use core::mem::MaybeUninit;

/// Points the fill keeps from its measuring pass, 3 KB of stack. Octowhere's largest glyph has 70
/// segments; a larger path's first `count - RING` points are visited twice.
const RING: usize = 256;

/// A linear map in y-down device coordinates, applied about the pen point: `x' = a·x + b·y`,
/// `y' = c·x + d·y`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Transform {
    pub(crate) m: [f32; 4],
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

    /// Maps a vector the way the rasterizer maps outlines and paths, for placing glyphs along a
    /// transformed baseline with the same convention.
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d] = self.m;
        (a * x + b * y, c * x + d * y)
    }

    /// The most the transform lengthens any vector by: its largest singular value.
    pub(crate) fn stretch(&self) -> f32 {
        let [a, b, c, d] = self.m;
        let sum = a * a + b * b + c * c + d * d;
        let det = a * d - b * c;
        sqrt((sum + sqrt((sum * sum - 4.0 * det * det).max(0.0))) / 2.0)
    }
}

/// Where a transformed glyph's or path's bitmap lies.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct TransformedMetrics {
    /// Absolute device position of the bitmap's top-left pixel.
    pub x: i32,
    pub y: i32,
    /// The bitmap is row-major, `width` pixels per row.
    pub width: usize,
    pub height: usize,
}

/// An affine map: `x' = ax·x + bx·y + cx`, `y' = ay·x + by·y + cy`.
#[derive(Copy, Clone)]
pub(crate) struct Affine {
    pub(crate) ax: f32,
    pub(crate) bx: f32,
    pub(crate) cx: f32,
    pub(crate) ay: f32,
    pub(crate) by: f32,
    pub(crate) cy: f32,
}

impl Affine {
    #[inline(always)]
    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (self.ax * x + self.bx * y + self.cx, self.ay * x + self.by * y + self.cy)
    }
}

/// A path, visited once. The fill takes two, one to measure the path and one to draw it, both
/// made before the raster is sized.
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

/// The least coordinate `clamp` gives, 2^-100, unless `hi` is below it. Distinct floats at least
/// this large differ by at least 2^-123, so every delta between clamped points is zero or normal,
/// as the line walk needs.
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
pub(crate) fn split_pen(pen: f32) -> (i32, f32) {
    let whole = floor(pen);
    assert!((-MAX_DIMENSION..=MAX_DIMENSION).contains(&whole), "pen out of range");
    // SAFETY: the range check bounds `whole` inside `i32` and rejects NaN and infinities.
    (unsafe { as_i32_unchecked(whole) }, pen - whole)
}

/// Fills the path `map` places in pixels relative to `whole`, sizing the raster from the placed
/// points.
///
/// # Panics
///
/// If the placed path, or `whole` plus its corner, does not fit `i32`.
#[inline(always)]
pub(crate) fn fill(
    canvas: &mut Raster<'_>,
    map: Affine,
    whole: (i32, i32),
    measure: impl Outline,
    draw: impl Outline,
) -> TransformedMetrics {
    let (mut min_x, mut min_y, mut max_x, mut max_y) =
        (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    // The last `RING` points, placed, with whether each starts a contour: point `i` is in slot
    // `i % RING`. A path that fits is visited once, and one that does not is visited again only up
    // to where the ring starts, so a streamed source decodes nothing after that twice. Written
    // before read, in slots `first..count` modulo `RING`.
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
            x: whole.0,
            y: whole.1,
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
        "px or transform out of range: the transformed outline does not fit i32"
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
        x: whole.0.checked_add(x).expect("pen out of range"),
        y: whole.1.checked_add(y).expect("pen out of range"),
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
        let map = Affine {
            cx: map.cx - ox,
            cy: map.cy - oy,
            ..map
        };
        // Its own copy of the lines and previous point: the visit can reach a `dyn` source, which
        // keeps what the closure captures in memory, and the ring loop would then read them from
        // there.
        let mut overflow = Lines::of(canvas);
        let mut previous = last;
        draw.each_first(first, |event| {
            let ([x, y], start) = match event {
                PathEvent::MoveTo(p) => (p, true),
                PathEvent::LineTo(p) => (p, false),
            };
            let (qx, qy) = map.apply(x, y);
            let p = Point::new(clamp(qx, w), clamp(qy, h));
            if !start {
                // SAFETY: `clamp` puts both points inside the raster, each coordinate zero or at
                // least 2^-100.
                unsafe { overflow.edge(previous, p) };
            }
            previous = p;
        });
        last = previous;
    }
    let mut lines = Lines::of(canvas);
    for k in first..count {
        // SAFETY: the measuring pass wrote slot `k % RING` for every `k` in `first..count`.
        let ([qx, qy], start) = unsafe { ring[k % RING].assume_init() };
        let p = Point::new(clamp(qx - ox, w), clamp(qy - oy, h));
        if !start {
            // SAFETY: as in the overflow pass.
            unsafe { lines.edge(last, p) };
        }
        last = p;
    }
    metrics
}

/// Fills `path` under `transform`, about the pen point `pen` in device pixels, sizing `canvas` to
/// the filled area. Read the coverage from `canvas` afterwards.
///
/// The path's points are in its own units with y down, and each lands at `pen` plus
/// `transform.apply` of it. A contour that does not end on its first point is closed with a
/// segment back to it, and a `LineTo` with no contour open starts one. Pixels are filled by the
/// nonzero rule, with partial coverage at the edges. Every point is clamped into the raster, so any
/// path is safe to draw.
///
/// # Panics
///
/// If `pen` is not finite or its floor does not fit `i32`, or the placed path does not fit `i32`.
pub fn rasterize_path<I>(
    canvas: &mut Raster<'_>,
    path: I,
    transform: Transform,
    pen: (f32, f32),
) -> TransformedMetrics
where
    I: IntoIterator<Item = PathEvent>,
    I::IntoIter: Clone,
{
    let (pen_x, fx) = split_pen(pen.0);
    let (pen_y, fy) = split_pen(pen.1);
    let [a, b, c, d] = transform.m;
    let map = Affine {
        ax: a,
        bx: b,
        cx: fx,
        ay: c,
        by: d,
        cy: fy,
    };
    let path = Closed::new(path.into_iter());
    fill(canvas, map, (pen_x, pen_y), Iter(path.clone()), Iter(path))
}

/// A path with every contour closed.
#[derive(Clone)]
struct Closed<I> {
    events: core::iter::Fuse<I>,
    /// The open contour's first and latest points.
    first: [f32; 2],
    last: [f32; 2],
    open: bool,
    /// A `MoveTo` held back while the contour before it is closed.
    held: Option<[f32; 2]>,
}

impl<I: Iterator<Item = PathEvent>> Closed<I> {
    fn new(events: I) -> Self {
        Closed {
            events: events.fuse(),
            first: [0.0; 2],
            last: [0.0; 2],
            open: false,
            held: None,
        }
    }

    /// The segment back to the open contour's first point, if it ends elsewhere. Ends the contour.
    fn close(&mut self) -> Option<PathEvent> {
        let closing = self.open && self.last != self.first;
        self.open = false;
        closing.then_some(PathEvent::LineTo(self.first))
    }

    fn start(&mut self, point: [f32; 2]) -> PathEvent {
        self.first = point;
        self.last = point;
        self.open = true;
        PathEvent::MoveTo(point)
    }
}

impl<I: Iterator<Item = PathEvent>> Iterator for Closed<I> {
    type Item = PathEvent;

    fn next(&mut self) -> Option<PathEvent> {
        if let Some(point) = self.held.take() {
            return Some(self.start(point));
        }
        match self.events.next() {
            None => self.close(),
            Some(PathEvent::MoveTo(point)) => match self.close() {
                Some(closing) => {
                    self.held = Some(point);
                    Some(closing)
                }
                None => Some(self.start(point)),
            },
            Some(PathEvent::LineTo(point)) if !self.open => Some(self.start(point)),
            Some(PathEvent::LineTo(point)) => {
                self.last = point;
                Some(PathEvent::LineTo(point))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Closed, LEAST, clamp};
    use crate::PathEvent::{LineTo, MoveTo};
    use alloc::vec::Vec;

    #[test]
    fn closed_closes_every_contour() {
        let (a, b, c, d) = ([0.0, 0.0], [4.0, 0.0], [4.0, 3.0], [1.0, 1.0]);
        let events = [LineTo(a), LineTo(b), LineTo(c), MoveTo(d), LineTo(a), LineTo(d), MoveTo(b), LineTo(c)];
        let closed: Vec<_> = Closed::new(events.into_iter()).collect();
        let want = [
            MoveTo(a),
            LineTo(b),
            LineTo(c),
            LineTo(a),
            MoveTo(d),
            LineTo(a),
            LineTo(d),
            MoveTo(b),
            LineTo(c),
            LineTo(b),
        ];
        assert_eq!(closed, want);
    }

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
