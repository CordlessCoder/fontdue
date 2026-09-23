pub use crate::platform::f32x4;
use crate::platform::{self, abs, atan2, sqrt};
use crate::{Glyph, OutlineBounds};
use alloc::vec;
use alloc::vec::*;

#[derive(Copy, Clone, PartialEq, Debug)]
struct AABB {
    /// Coordinate of the left-most edge.
    xmin: f32,
    /// Coordinate of the right-most edge.
    xmax: f32,
    /// Coordinate of the bottom-most edge.
    ymin: f32,
    /// Coordinate of the top-most edge.
    ymax: f32,
}

impl Default for AABB {
    fn default() -> Self {
        AABB {
            xmin: 0.0,
            xmax: 0.0,
            ymin: 0.0,
            ymax: 0.0,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
struct CubeCurve {
    a: Point,
    b: Point,
    c: Point,
    d: Point,
}

impl CubeCurve {
    const fn new(a: Point, b: Point, c: Point, d: Point) -> CubeCurve {
        CubeCurve {
            a,
            b,
            c,
            d,
        }
    }

    fn scale(&self, scale: f32) -> CubeCurve {
        CubeCurve {
            a: self.a.scale(scale),
            b: self.b.scale(scale),
            c: self.c.scale(scale),
            d: self.d.scale(scale),
        }
    }

    fn is_flat(&self, threshold: f32) -> bool {
        let (d1, d2, d3, d4) = f32x4::new(
            self.a.distance_squared(self.b),
            self.b.distance_squared(self.c),
            self.c.distance_squared(self.d),
            self.a.distance_squared(self.d),
        )
        .sqrt()
        .copied();
        (d1 + d2 + d3) < threshold * d4
    }

    fn split(&self) -> (CubeCurve, CubeCurve) {
        let q0 = self.a.midpoint(self.b);
        let q1 = self.b.midpoint(self.c);
        let q2 = self.c.midpoint(self.d);
        let r0 = q0.midpoint(q1);
        let r1 = q1.midpoint(q2);
        let s0 = r0.midpoint(r1);
        (CubeCurve::new(self.a, q0, r0, s0), CubeCurve::new(s0, r1, q2, self.d))
    }

    /// The point at time t in the curve.
    fn point(&self, t: f32) -> Point {
        let tm = 1.0 - t;
        let a = tm * tm * tm;
        let b = 3.0 * (tm * tm) * t;
        let c = 3.0 * tm * (t * t);
        let d = t * t * t;

        let x = a * self.a.x + b * self.b.x + c * self.c.x + d * self.d.x;
        let y = a * self.a.y + b * self.b.y + c * self.c.y + d * self.d.y;
        Point::new(x, y)
    }

    /// The slope of the tangent line at time t.
    fn slope(&self, t: f32) -> (f32, f32) {
        let tm = 1.0 - t;
        let a = 3.0 * (tm * tm);
        let b = 6.0 * tm * t;
        let c = 3.0 * (t * t);

        let x = a * (self.b.x - self.a.x) + b * (self.c.x - self.b.x) + c * (self.d.x - self.c.x);
        let y = a * (self.b.y - self.a.y) + b * (self.c.y - self.b.y) + c * (self.d.y - self.c.y);
        (x, y)
    }

    /// The angle of the tangent line at time t in rads.
    fn angle(&self, t: f32) -> f32 {
        let (x, y) = self.slope(t);
        abs(atan2(x, y))
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
struct QuadCurve {
    a: Point,
    b: Point,
    c: Point,
}

impl QuadCurve {
    fn new(a: Point, b: Point, c: Point) -> QuadCurve {
        QuadCurve {
            a,
            b,
            c,
        }
    }

    fn scale(&self, scale: f32) -> QuadCurve {
        QuadCurve {
            a: self.a.scale(scale),
            b: self.b.scale(scale),
            c: self.c.scale(scale),
        }
    }

    fn is_flat(&self, threshold: f32) -> bool {
        let (d1, d2, d3, _) = f32x4::new(
            self.a.distance_squared(self.b),
            self.b.distance_squared(self.c),
            self.a.distance_squared(self.c),
            1.0,
        )
        .sqrt()
        .copied();
        (d1 + d2) < threshold * d3
    }

    fn split(&self) -> (QuadCurve, QuadCurve) {
        let q0 = self.a.midpoint(self.b);
        let q1 = self.b.midpoint(self.c);
        let r0 = q0.midpoint(q1);
        (QuadCurve::new(self.a, q0, r0), QuadCurve::new(r0, q1, self.c))
    }

    /// The point at time t in the curve.
    fn point(&self, t: f32) -> Point {
        let tm = 1.0 - t;
        let a = tm * tm;
        let b = 2.0 * tm * t;
        let c = t * t;

        let x = a * self.a.x + b * self.b.x + c * self.c.x;
        let y = a * self.a.y + b * self.b.y + c * self.c.y;
        Point::new(x, y)
    }

    /// The slope of the tangent line at time t.
    fn slope(&self, t: f32) -> (f32, f32) {
        let tm = 1.0 - t;
        let a = 2.0 * tm;
        let b = 2.0 * t;

        let x = a * (self.b.x - self.a.x) + b * (self.c.x - self.b.x);
        let y = a * (self.b.y - self.a.y) + b * (self.c.y - self.b.y);
        (x, y)
    }

    /// The angle of the tangent line at time t in rads.
    fn angle(&self, t: f32) -> f32 {
        let (x, y) = self.slope(t);
        abs(atan2(x, y))
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Point {
    /// Absolute X coordinate.
    pub x: f32,
    /// Absolute Y coordinate.
    pub y: f32,
}

impl Default for Point {
    fn default() -> Self {
        Point {
            x: 0.0,
            y: 0.0,
        }
    }
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Point {
        Point {
            x,
            y,
        }
    }

    pub fn scale(&self, scale: f32) -> Point {
        Point {
            x: self.x * scale,
            y: self.y * scale,
        }
    }

    pub fn distance_squared(&self, other: Point) -> f32 {
        let x = self.x - other.x;
        let y = self.y - other.y;
        x * x + y * y
    }

    pub fn distance(&self, other: Point) -> f32 {
        let x = self.x - other.x;
        let y = self.y - other.y;
        sqrt(x * x + y * y)
    }

    pub fn midpoint(&self, other: Point) -> Point {
        Point {
            x: (self.x + other.x) / 2.0,
            y: (self.y + other.y) / 2.0,
        }
    }
}

#[derive(Clone)]
pub struct Geometry {
    points: Vec<[f32; 2]>,
    /// End index in `points` of each finished contour.
    contours: Vec<u32>,
    /// Where the open contour starts in `points`.
    contour_start: usize,
    effective_bounds: AABB,
    start_point: Point,
    previous_point: Point,
    area: f32,
    reverse_points: bool,
    max_area: f32,
}

struct Segment {
    a: Point,
    at: f32,
    c: Point,
    ct: f32,
}

impl Segment {
    const fn new(a: Point, at: f32, c: Point, ct: f32) -> Segment {
        Segment {
            a,
            at,
            c,
            ct,
        }
    }
}

impl ttf_parser::OutlineBuilder for Geometry {
    fn move_to(&mut self, x0: f32, y0: f32) {
        self.end_contour();
        let next_point = Point::new(x0, y0);
        self.start_point = next_point;
        self.previous_point = next_point;
    }

    fn line_to(&mut self, x0: f32, y0: f32) {
        let next_point = Point::new(x0, y0);
        self.push(self.previous_point, next_point);
        self.previous_point = next_point;
    }

    fn quad_to(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        let control_point = Point::new(x0, y0);
        let next_point = Point::new(x1, y1);

        let curve = QuadCurve::new(self.previous_point, control_point, next_point);
        let mut stack = vec![Segment::new(self.previous_point, 0.0, next_point, 1.0)];
        while let Some(seg) = stack.pop() {
            let bt = (seg.at + seg.ct) * 0.5;
            let b = curve.point(bt);
            // This is twice the triangle area
            let area = (b.x - seg.a.x) * (seg.c.y - seg.a.y) - (seg.c.x - seg.a.x) * (b.y - seg.a.y);
            // The second half first, so the first is drawn first and points come in contour order.
            if platform::abs(area) > self.max_area {
                stack.push(Segment::new(b, bt, seg.c, seg.ct));
                stack.push(Segment::new(seg.a, seg.at, b, bt));
            } else {
                self.push(seg.a, seg.c);
            }
        }

        self.previous_point = next_point;
    }

    fn curve_to(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, x2: f32, y2: f32) {
        let first_control = Point::new(x0, y0);
        let second_control = Point::new(x1, y1);
        let next_point = Point::new(x2, y2);

        let curve = CubeCurve::new(self.previous_point, first_control, second_control, next_point);
        let mut stack = vec![Segment::new(self.previous_point, 0.0, next_point, 1.0)];
        while let Some(seg) = stack.pop() {
            let bt = (seg.at + seg.ct) * 0.5;
            let b = curve.point(bt);
            // This is twice the triangle area
            let area = (b.x - seg.a.x) * (seg.c.y - seg.a.y) - (seg.c.x - seg.a.x) * (b.y - seg.a.y);
            // The second half first, so the first is drawn first and points come in contour order.
            if platform::abs(area) > self.max_area {
                stack.push(Segment::new(b, bt, seg.c, seg.ct));
                stack.push(Segment::new(seg.a, seg.at, b, bt));
            } else {
                self.push(seg.a, seg.c);
            }
        }
        self.previous_point = next_point;
    }

    fn close(&mut self) {
        if self.start_point != self.previous_point {
            self.push(self.previous_point, self.start_point);
        }
        self.previous_point = self.start_point;
        self.end_contour();
    }
}

/// A non-negative coordinate, or 0 below [`crate::outline::SMALLEST_COORDINATE`], as sources
/// promise.
fn flush(v: f32) -> f32 {
    const SMALLEST: f32 = crate::outline::SMALLEST_COORDINATE;
    if v < SMALLEST {
        0.0
    } else {
        v
    }
}

impl Geometry {
    // Artisanal bespoke hand carved curves
    pub fn new(scale: f32, units_per_em: f32) -> Geometry {
        const ERROR_THRESHOLD: f32 = 3.0; // In pixels.
        let max_area = ERROR_THRESHOLD * 2.0 * (units_per_em / scale);

        Geometry {
            points: Vec::new(),
            contours: Vec::new(),
            contour_start: 0,
            effective_bounds: AABB {
                xmin: f32::MAX,
                xmax: f32::MIN,
                ymin: f32::MAX,
                ymax: f32::MIN,
            },
            start_point: Point::default(),
            previous_point: Point::default(),
            area: 0.0,
            reverse_points: false,
            max_area,
        }
    }

    fn push(&mut self, start: Point, end: Point) {
        // We're using to_bits here because we only care if they're _exactly_ the same.
        let (x_same, y_same) = (start.x.to_bits() == end.x.to_bits(), start.y.to_bits() == end.y.to_bits());
        if x_same && y_same {
            return;
        }
        let last = self.points.get(self.contour_start..).and_then(|c| c.last()).copied();
        if last != Some([start.x, start.y]) {
            // A segment that does not continue the contour: flattening emits them in order, so
            // only a source's own gap does this. The gap starts a new contour.
            self.end_contour();
            self.points.push([start.x, start.y]);
        }
        self.points.push([end.x, end.y]);
        // Horizontal segments add no area and stay out of the bounds, so the upright glyph is what
        // it was when they were dropped; `finalize` clamps them into the bounds.
        if !y_same {
            self.area += (end.y - start.y) * (end.x + start.x);
            Self::recalculate_bounds(&mut self.effective_bounds, start.x, start.y);
            Self::recalculate_bounds(&mut self.effective_bounds, end.x, end.y);
        }
    }

    /// Finishes the open contour, dropping it if it has no segment.
    fn end_contour(&mut self) {
        if self.points.len() - self.contour_start < 2 {
            self.points.truncate(self.contour_start);
        } else {
            self.contours.push(self.points.len() as u32);
        }
        self.contour_start = self.points.len();
    }

    pub(crate) fn finalize(mut self, glyph: &mut Glyph) {
        self.end_contour();
        let b = self.effective_bounds;
        if b.xmin > b.xmax {
            // No segment with vertical extent: nothing covers anything.
            self.effective_bounds = AABB::default();
            self.points.clear();
            self.contours.clear();
        } else {
            // Points only horizontal segments reach can lie outside the bounds, and are clamped
            // into them. A run of horizontal segments lies on one row and only its two ends, which
            // it shares with the contour's other segments, decide its area under any transform.
            // Clamping moves points along the row and none of those ends, so each contour stays
            // closed with the same area. Every other point is inside already and does not move.
            self.reverse_points = self.area > 0.0;
            let mut start = 0;
            for &end in &self.contours {
                let contour = &mut self.points[start..end as usize];
                if self.reverse_points {
                    contour.reverse();
                }
                for p in contour {
                    let (x, y) = (p[0].max(b.xmin).min(b.xmax), p[1].max(b.ymin).min(b.ymax));
                    *p = [flush(x - b.xmin), flush(abs(y - b.ymax))];
                }
                start = end as usize;
            }
            self.points.shrink_to_fit();
            self.contours.shrink_to_fit();
        }
        glyph.points = self.points;
        glyph.contours = self.contours;
        glyph.bounds = OutlineBounds {
            xmin: self.effective_bounds.xmin,
            ymin: self.effective_bounds.ymin,
            width: self.effective_bounds.xmax - self.effective_bounds.xmin,
            height: self.effective_bounds.ymax - self.effective_bounds.ymin,
        };
    }

    fn recalculate_bounds(bounds: &mut AABB, x: f32, y: f32) {
        if x < bounds.xmin {
            bounds.xmin = x;
        }
        if x > bounds.xmax {
            bounds.xmax = x;
        }
        if y < bounds.ymin {
            bounds.ymin = y;
        }
        if y > bounds.ymax {
            bounds.ymax = y;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ttf_parser::OutlineBuilder;

    #[test]
    fn empty_and_single_point_contours_have_no_lines() {
        let empty = Geometry::new(32.0, 1000.0);
        let mut empty_glyph = Glyph::default();
        empty.finalize(&mut empty_glyph);
        assert!(empty_glyph.points.is_empty() && empty_glyph.contours.is_empty());

        let mut point = Geometry::new(32.0, 1000.0);
        point.move_to(10.0, 20.0);
        point.close();
        let mut point_glyph = Glyph::default();
        point.finalize(&mut point_glyph);
        assert!(point_glyph.points.is_empty() && point_glyph.contours.is_empty());
    }
}
