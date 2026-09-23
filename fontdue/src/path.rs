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

/// Fills `path` under `transform` into `canvas` at the size it already has, clipped to it, and
/// adds to what the raster holds. `pen` is in pixels from the raster's top-left corner. Clear the
/// raster with [`Raster::resize`] between frames; left uncleared, layered paths add their
/// coverage, which is their union under the nonzero rule where they wind the same way.
///
/// The path's points are placed and its contours closed as for [`rasterize_path`]. Any path is
/// safe to draw; non-finite points give unreliable coverage and nothing else.
pub fn rasterize_path_clipped<I: IntoIterator<Item = PathEvent>>(
    canvas: &mut Raster<'_>,
    path: I,
    transform: Transform,
    pen: (f32, f32),
) {
    let (w, h) = (at_most(canvas.width()), at_most(canvas.height()));
    let [a, b, c, d] = transform.m;
    let map = Affine {
        ax: a,
        bx: b,
        cx: pen.0,
        ay: c,
        by: d,
        cy: pen.1,
    };
    let mut lines = Lines::of(canvas);
    let mut last = (0.0, 0.0);
    for event in Closed::new(path.into_iter()) {
        match event {
            PathEvent::MoveTo([x, y]) => last = map.apply(x, y),
            PathEvent::LineTo([x, y]) => {
                let next = map.apply(x, y);
                clip(&mut lines, last, next, w, h);
                last = next;
            }
        }
    }
}

/// The largest float not above `n`.
fn at_most(n: usize) -> f32 {
    let f = n as f32;
    if f as usize > n {
        f32::from_bits(f.to_bits() - 1)
    } else {
        f
    }
}

/// Adds the part of the segment from `p` to `q` that lies in rows `[0, h]`, with what lies left or
/// right of `[0, w]` moved onto that edge. A row's coverage is a running sum from the left, so a
/// segment left of the raster covers the row as it would at `x = 0`, and one right of it covers
/// none of it, as at `x = w`.
fn clip(lines: &mut Lines<'_, '_>, mut p: (f32, f32), mut q: (f32, f32), w: f32, h: f32) {
    if (p.1 <= 0.0 && q.1 <= 0.0) || (p.1 >= h && q.1 >= h) {
        return;
    }
    let at_row = |p: (f32, f32), q: (f32, f32), y: f32| (p.0 + (y - p.1) / (q.1 - p.1) * (q.0 - p.0), y);
    let (from, to) = (p, q);
    for end in [&mut p, &mut q] {
        if end.1 < 0.0 {
            *end = at_row(from, to, 0.0);
        } else if end.1 > h {
            *end = at_row(from, to, h);
        }
    }
    // The points where the segment crosses x = 0 and x = w, in the order it meets them.
    let mut points = [p; 4];
    let mut n = 1;
    let edges = if p.0 <= q.0 {
        [0.0, w]
    } else {
        [w, 0.0]
    };
    for x in edges {
        if (p.0 < x) != (q.0 < x) {
            points[n] = (x, p.1 + (x - p.0) / (q.0 - p.0) * (q.1 - p.1));
            n += 1;
        }
    }
    points[n] = q;
    let place = |(x, y): (f32, f32)| Point::new(clamp(x, w), clamp(y, h));
    let mut from = place(points[0]);
    for &point in &points[1..=n] {
        let to = place(point);
        // SAFETY: `clamp` puts both points inside the raster, each coordinate zero or at least
        // 2^-100, and `at_most` keeps `w` and `h` within its size.
        unsafe { lines.edge(from, to) };
        from = to;
    }
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

/// One step of a path with curves, for [`flatten`]. Points are in the path's own units, as for
/// [`rasterize_path`].
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum PathCommand {
    MoveTo([f32; 2]),
    LineTo([f32; 2]),
    /// A quadratic Bézier from the current point: its control point, then its end.
    QuadTo([f32; 2], [f32; 2]),
    /// A cubic Bézier from the current point: its two control points, then its end.
    CubicTo([f32; 2], [f32; 2], [f32; 2]),
}

/// The most lines [`flatten`] draws for one curve.
pub const MAX_CURVE_SEGMENTS: u32 = 1024;

/// `commands` with every curve replaced by lines, for [`rasterize_path`]. No line strays more than
/// `tolerance` from its curve, in the path's units, so under a transform that scales by `s` the
/// error in pixels is `s * tolerance`. A curve that needs more than [`MAX_CURVE_SEGMENTS`] lines
/// gets that many, and may stray further. Nothing is allocated, so the result can be walked
/// twice, as `rasterize_path` does.
///
/// Each curve is split into equal steps of its parameter, the count from Wang's formula. A curve
/// with no current point starts a contour at its end, as a `LineTo` does.
///
/// # Panics
///
/// If `tolerance` is not positive.
pub fn flatten<I: IntoIterator<Item = PathCommand>>(commands: I, tolerance: f32) -> Flatten<I::IntoIter> {
    assert!(tolerance > 0.0, "flatten tolerance must be positive");
    let k = 0.75 / tolerance;
    Flatten {
        commands: commands.into_iter(),
        bound: k * k,
        current: None,
        curve: [[0.0; 2]; 4],
        step: 0,
        steps: 0,
        at: 0.0,
        dt: 0.0,
    }
}

/// The lines of a path with curves; see [`flatten`].
#[derive(Clone, Debug)]
pub struct Flatten<I> {
    commands: I,
    /// `(3/4 / tolerance)^2`, for `segments`.
    bound: f32,
    current: Option<[f32; 2]>,
    /// The curve being drawn, as a cubic, and how many of its `steps` lines are drawn.
    curve: [[f32; 2]; 4],
    step: u32,
    steps: u32,
    /// `step` as a float, and `1 / steps`: the parameter is their product.
    at: f32,
    dt: f32,
}

impl<I> Flatten<I> {
    /// Starts drawing the cubic from the current point through `c1`, `c2` to `to`, or starts a
    /// contour at `to` if there is no current point.
    fn curve(&mut self, c1: [f32; 2], c2: [f32; 2], to: [f32; 2]) -> PathEvent {
        let Some(from) = self.current.replace(to) else {
            return PathEvent::LineTo(to);
        };
        self.curve = [from, c1, c2, to];
        let (steps, whole) = segments(&self.curve, self.bound);
        self.steps = steps;
        self.step = 0;
        self.at = 0.0;
        self.dt = 1.0 / whole;
        self.line()
    }

    fn line(&mut self) -> PathEvent {
        self.step += 1;
        if self.step == self.steps {
            return PathEvent::LineTo(self.curve[3]);
        }
        self.at += 1.0;
        let t = self.at * self.dt;
        let u = 1.0 - t;
        let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
        let [p0, p1, p2, p3] = self.curve;
        let at = |i: usize| w[0] * p0[i] + w[1] * p1[i] + w[2] * p2[i] + w[3] * p3[i];
        PathEvent::LineTo([at(0), at(1)])
    }
}

/// Wang's formula for a cubic: uniform steps in the parameter, enough that no line is farther
/// than the tolerance from the curve, which is `3/4 * max |p[i] - 2 p[i+1] + p[i+2]| / n^2` at
/// most. So `n` is the least with `n^4 >= bound * max^2`, `bound` being `(3/4 / tolerance)^2`,
/// found by counting up rather than by two square roots, which are software on some targets; the
/// count costs a few multiplications per line the curve then draws. Returned as an integer and
/// as a float. A quadratic raised to a cubic gets the count the quadratic's own formula gives.
fn segments(p: &[[f32; 2]; 4], bound: f32) -> (u32, f32) {
    let second = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
        let (x, y) = (a[0] - 2.0 * b[0] + c[0], a[1] - 2.0 * b[1] + c[1]);
        x * x + y * y
    };
    // NaN, from a non-finite point, fails the comparison at once and gets one line.
    let target = bound * second(p[0], p[1], p[2]).max(second(p[1], p[2], p[3]));
    let (mut n, mut whole) = (1, 1.0f32);
    while n < MAX_CURVE_SEGMENTS && (whole * whole) * (whole * whole) < target {
        n += 1;
        whole += 1.0;
    }
    (n, whole)
}

impl<I: Iterator<Item = PathCommand>> Iterator for Flatten<I> {
    type Item = PathEvent;

    fn next(&mut self) -> Option<PathEvent> {
        if self.step < self.steps {
            return Some(self.line());
        }
        Some(match self.commands.next()? {
            PathCommand::MoveTo(p) => {
                self.current = Some(p);
                PathEvent::MoveTo(p)
            }
            PathCommand::LineTo(p) => {
                self.current = Some(p);
                PathEvent::LineTo(p)
            }
            PathCommand::QuadTo(c, to) => {
                let from = self.current.unwrap_or(to);
                let raise =
                    |p: [f32; 2]| [p[0] + 2.0 / 3.0 * (c[0] - p[0]), p[1] + 2.0 / 3.0 * (c[1] - p[1])];
                self.curve(raise(from), raise(to), to)
            }
            PathCommand::CubicTo(c1, c2, to) => self.curve(c1, c2, to),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Closed, LEAST, PathCommand, clamp, flatten, segments};
    use crate::PathEvent::{LineTo, MoveTo};
    use alloc::vec::Vec;

    #[test]
    fn curve_segments_follow_wangs_formula() {
        // A straight cubic with evenly spaced controls has no second difference: one line.
        let n = |p: &[[f32; 2]; 4], tolerance: f32| segments(p, (0.75 / tolerance) * (0.75 / tolerance)).0;
        assert_eq!(n(&[[0.0, 0.0], [1.0, 1.0], [2.0, 2.0], [3.0, 3.0]], 0.01), 1);
        // Second difference 12, so 0.75 * 12 / n^2 <= tolerance needs n = 10 at 0.09 and n = 20
        // at a quarter of that; 9 needs 0.1111.
        let bend = [[0.0, 0.0], [0.0, 12.0], [0.0, 12.0], [0.0, 0.0]];
        assert_eq!(n(&bend, 0.09), 10);
        assert_eq!(n(&bend, 0.0225), 20);
        assert_eq!(n(&bend, 0.111), 10);
        assert_eq!(n(&bend, 0.1112), 9);
        assert_eq!(n(&bend, 1e-9), super::MAX_CURVE_SEGMENTS);
        assert_eq!(n(&[[0.0, 0.0], [f32::NAN, 0.0], [1.0, 1.0], [2.0, 0.0]], 0.1), 1);
        assert_eq!(n(&[[0.0, 0.0], [f32::INFINITY, 0.0], [1.0, 1.0], [2.0, 0.0]], 0.1), 1024);
        let (count, whole) = segments(&bend, (0.75 / 100.0) * (0.75 / 100.0));
        assert_eq!((count, whole), (1, 1.0));
    }

    #[test]
    fn flatten_draws_curves_to_their_ends() {
        let quad = [PathCommand::MoveTo([0.0, 0.0]), PathCommand::QuadTo([10.0, 20.0], [20.0, 0.0])];
        let events: Vec<_> = flatten(quad, 0.12).collect();
        assert_eq!(events[0], MoveTo([0.0, 0.0]));
        assert_eq!(*events.last().unwrap(), LineTo([20.0, 0.0]));
        // Every point is on the parabola y = 2x - x^2 / 10, and the middle one is its apex.
        for e in &events[1..] {
            let LineTo([x, y]) = *e else {
                panic!("{e:?}")
            };
            assert!((y - (2.0 * x - x * x / 10.0)).abs() < 1e-4, "{x}, {y}");
        }
        assert!(events.len() % 2 == 1 && events[events.len() / 2] == LineTo([10.0, 10.0]));
        // With no current point, a curve starts a contour at its end.
        let lone: Vec<_> = flatten([PathCommand::CubicTo([1.0, 1.0], [2.0, 1.0], [3.0, 0.0])], 0.1).collect();
        assert_eq!(lone, [LineTo([3.0, 0.0])]);
    }

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
