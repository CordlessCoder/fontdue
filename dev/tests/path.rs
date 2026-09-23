use fontdue::raster::{Lines, Raster};
use fontdue::PathEvent::{self, LineTo, MoveTo};
use fontdue::{flatten, rasterize_path, PathCommand, Transform, TransformedMetrics};

fn polygon(points: &[[f32; 2]]) -> Vec<PathEvent> {
    let mut events = vec![MoveTo(points[0])];
    events.extend(points[1..].iter().map(|&p| LineTo(p)));
    events.push(LineTo(points[0]));
    events
}

fn fill(path: &[PathEvent], transform: Transform, pen: (f32, f32)) -> (TransformedMetrics, Vec<u8>) {
    let mut canvas = Raster::empty();
    let m = rasterize_path(&mut canvas, path.iter().copied(), transform, pen);
    let bitmap: Vec<u8> = canvas.get_bitmap_iter().collect();
    assert_eq!(bitmap.len(), m.width * m.height);
    (m, bitmap)
}

const RECTANGLE: [[f32; 2]; 4] = [[0.0, 0.0], [4.0, 0.0], [4.0, 3.0], [0.0, 3.0]];

#[test]
fn rectangle_fills_whole_pixels() {
    let (m, bitmap) = fill(&polygon(&RECTANGLE), Transform::IDENTITY, (10.0, 20.0));
    assert_eq!(
        m,
        TransformedMetrics {
            x: 10,
            y: 20,
            width: 4,
            height: 3
        }
    );
    assert!(bitmap.iter().all(|&c| c == 255), "{bitmap:?}");
}

#[test]
fn pen_fraction_spreads_coverage() {
    let (m, bitmap) = fill(&polygon(&RECTANGLE), Transform::IDENTITY, (10.5, 20.0));
    assert_eq!(
        m,
        TransformedMetrics {
            x: 10,
            y: 20,
            width: 5,
            height: 3
        }
    );
    for row in bitmap.chunks(5) {
        assert_eq!(row, [127, 255, 255, 255, 127]);
    }
}

#[test]
fn whole_pixel_pens_give_the_same_bitmap() {
    let path = polygon(&[[0.0, 0.0], [7.5, 1.25], [2.0, 6.0]]);
    let t = Transform::rotation(0.8660254, 0.5);
    let (a, bitmap_a) = fill(&path, t, (0.25, 0.75));
    let (b, bitmap_b) = fill(&path, t, (1000.25, -299.25));
    assert_eq!((b.x - a.x, b.y - a.y, b.width, b.height), (1000, -300, a.width, a.height));
    assert_eq!(bitmap_a, bitmap_b);
}

#[test]
fn open_contours_are_closed() {
    let triangle = [[0.0, 0.0], [7.5, 1.25], [2.0, 6.0]];
    let open: Vec<PathEvent> = [MoveTo(triangle[0]), LineTo(triangle[1]), LineTo(triangle[2])].into();
    let leading_line: Vec<PathEvent> = triangle.iter().map(|&p| LineTo(p)).collect();
    let t = Transform::rotation(0.6, 0.8);
    let want = fill(&polygon(&triangle), t, (3.5, 1.25));
    assert_eq!(fill(&open, t, (3.5, 1.25)), want);
    assert_eq!(fill(&leading_line, t, (3.5, 1.25)), want);
}

/// Coverage is by the nonzero rule: contours turning the same way add up to full coverage where
/// they overlap, and one turning the other way inside another cuts a hole.
#[test]
fn fills_by_the_nonzero_rule() {
    let square = |x: f32, y: f32, s: f32| [[x, y], [x + s, y], [x + s, y + s], [x, y + s]];
    let mut overlapping = polygon(&square(0.0, 0.0, 4.0));
    overlapping.extend(polygon(&square(2.0, 2.0, 4.0)));
    let (m, bitmap) = fill(&overlapping, Transform::IDENTITY, (0.0, 0.0));
    assert_eq!((m.width, m.height), (6, 6));
    for (i, &c) in bitmap.iter().enumerate() {
        let (x, y) = (i % 6, i / 6);
        let inside = (x < 4 && y < 4) || (x >= 2 && y >= 2);
        assert_eq!(c, u8::from(inside) * 255, "pixel {x}, {y}");
    }

    let mut ring = polygon(&square(0.0, 0.0, 6.0));
    let mut hole = square(2.0, 2.0, 2.0);
    hole.reverse();
    ring.extend(polygon(&hole));
    let (_, bitmap) = fill(&ring, Transform::IDENTITY, (0.0, 0.0));
    for (i, &c) in bitmap.iter().enumerate() {
        let (x, y) = (i % 6, i / 6);
        let in_hole = (2..4).contains(&x) && (2..4).contains(&y);
        assert_eq!(c, u8::from(!in_hole) * 255, "pixel {x}, {y}");
    }
}

/// Overlapping squares wound the same way cut a hole under even-odd, and a pentagram's centre,
/// wound twice, is empty. A reversed hole is a hole under either rule.
#[test]
fn fills_by_the_even_odd_rule() {
    let even_odd = |path: &[PathEvent]| {
        let mut canvas = Raster::empty();
        let m = rasterize_path(&mut canvas, path.iter().copied(), Transform::IDENTITY, (0.0, 0.0));
        let bitmap: Vec<u8> = canvas.get_bitmap_iter_even_odd().collect();
        assert_eq!(bitmap.len(), m.width * m.height);
        (m, bitmap)
    };
    let square = |x: f32, y: f32, s: f32| [[x, y], [x + s, y], [x + s, y + s], [x, y + s]];
    let mut overlapping = polygon(&square(0.0, 0.0, 4.0));
    overlapping.extend(polygon(&square(2.0, 2.0, 4.0)));
    let (m, bitmap) = even_odd(&overlapping);
    assert_eq!((m.width, m.height), (6, 6));
    for (i, &c) in bitmap.iter().enumerate() {
        let (x, y) = (i % 6, i / 6);
        let inside = (x < 4 && y < 4) != (x >= 2 && y >= 2);
        assert_eq!(c, u8::from(inside) * 255, "pixel {x}, {y}");
    }

    let mut ring = polygon(&square(0.0, 0.0, 6.0));
    let mut hole = square(2.0, 2.0, 2.0);
    hole.reverse();
    ring.extend(polygon(&hole));
    assert_eq!(even_odd(&ring).1, fill(&ring, Transform::IDENTITY, (0.0, 0.0)).1);

    let star: Vec<[f32; 2]> = (0..5)
        .map(|i| {
            let a = std::f32::consts::TAU * (2 * i) as f32 / 5.0;
            [20.0 + 20.0 * a.sin(), 20.0 - 20.0 * a.cos()]
        })
        .collect();
    let (m, odd) = even_odd(&polygon(&star));
    let (_, nonzero) = fill(&polygon(&star), Transform::IDENTITY, (0.0, 0.0));
    let centre = |bitmap: &[u8]| bitmap[(m.height / 2) * m.width + m.width / 2];
    assert_eq!((centre(&odd), centre(&nonzero)), (0, 255));
    assert!(odd.iter().zip(&nonzero).all(|(&o, &n)| o <= n));
}

/// The safe fill and the unchecked core agree on a path already inside the raster.
#[test]
fn matches_lines() {
    let points = [[1.5, 1.25], [9.0, 2.5], [6.75, 7.0], [2.25, 5.5]];
    let (m, want) = fill(&polygon(&points), Transform::IDENTITY, (0.0, 0.0));
    assert_eq!((m.x, m.y, m.width, m.height), (1, 1, 8, 6));
    let mut canvas = Raster::empty();
    let mut lines = Lines::new(&mut canvas, m.width, m.height);
    let placed: Vec<[f32; 2]> = points.iter().map(|&[x, y]| [x - 1.0, y - 1.0]).collect();
    for (i, &from) in placed.iter().enumerate() {
        // SAFETY: every point is inside the 8 by 6 raster, and no two differ by a subnormal.
        unsafe { lines.line(from, placed[(i + 1) % placed.len()]) };
    }
    let got: Vec<u8> = canvas.get_bitmap_iter().collect();
    assert_eq!(got, want);
}

#[test]
fn empty_and_nan_paths_stay_in_bounds() {
    let (m, bitmap) = fill(&[], Transform::IDENTITY, (5.0, 6.0));
    assert_eq!(
        m,
        TransformedMetrics {
            x: 5,
            y: 6,
            width: 0,
            height: 0
        }
    );
    assert!(bitmap.is_empty());

    let nan = f32::NAN;
    let path = polygon(&[[0.0, 0.0], [nan, 3.0], [4.0, nan], [nan, nan], [2.0, 5.0]]);
    let (m, _) = fill(&path, Transform::rotation(0.6, 0.8), (0.5, 0.5));
    assert!(m.width <= 6 && m.height <= 6, "{m:?}");
    let (m, bitmap) = fill(&polygon(&[[nan, nan], [nan, 1.0]]), Transform::IDENTITY, (0.0, 0.0));
    assert_eq!((m.width, m.height), (0, 0));
    assert!(bitmap.is_empty());
}

#[test]
#[should_panic(expected = "does not fit i32")]
fn rejects_paths_too_large() {
    let far = f32::MAX;
    fill(&polygon(&[[0.0, 0.0], [far, 0.0], [far, far]]), Transform::IDENTITY, (0.0, 0.0));
}

/// A circle of radius `r` about `(r, r)` as four cubics, which stray from it by at most 0.03% of
/// `r`.
fn circle(r: f32) -> Vec<PathCommand> {
    let k = 0.552_284_8 * r;
    let (c, e) = (r, 2.0 * r);
    vec![
        PathCommand::MoveTo([c, 0.0]),
        PathCommand::CubicTo([c + k, 0.0], [e, c - k], [e, c]),
        PathCommand::CubicTo([e, c + k], [c + k, e], [c, e]),
        PathCommand::CubicTo([c - k, e], [0.0, c + k], [0.0, c]),
        PathCommand::CubicTo([0.0, c - k], [c - k, 0.0], [c, 0.0]),
    ]
}

/// No chord of the flattened circle is farther from the circle than the tolerance, the circle's
/// own error aside.
#[test]
fn flattened_curves_stay_within_tolerance() {
    let r = 100.0f32;
    for tolerance in [1.0, 0.1, 0.01] {
        let mut points = vec![];
        for e in flatten(circle(r), tolerance) {
            let (MoveTo(p) | LineTo(p)) = e;
            points.push(p);
        }
        let mut worst = 0.0f32;
        for pair in points.windows(2) {
            for i in 0..=8 {
                let t = i as f32 / 8.0;
                let x = pair[0][0] + t * (pair[1][0] - pair[0][0]) - r;
                let y = pair[0][1] + t * (pair[1][1] - pair[0][1]) - r;
                worst = worst.max(((x * x + y * y).sqrt() - r).abs());
            }
        }
        assert!(worst <= tolerance + 0.0003 * r, "{worst} at tolerance {tolerance}");
    }
}

#[test]
fn fills_a_flattened_circle() {
    let r = 20.0;
    let mut canvas = Raster::empty();
    let m = rasterize_path(&mut canvas, flatten(circle(r), 0.05), Transform::IDENTITY, (0.0, 0.0));
    assert_eq!((m.x, m.y, m.width, m.height), (0, 0, 40, 40));
    let area: f32 = canvas.get_bitmap_iter().map(|c| c as f32 / 255.0).sum();
    let want = std::f32::consts::PI * r * r;
    assert!((area - want).abs() < 0.005 * want, "{area} against {want}");
}
