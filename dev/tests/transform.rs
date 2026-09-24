use fontdue::raster::Raster;
use fontdue::{Font, FontRepr, FontSettings, Transform};
use fontdue_macros::fontdue_font_from_file;
use std::collections::HashMap;

fontdue_font_from_file!(BakedRoboto, "../resources/fonts/Roboto-Regular.ttf", scale: 32);
fontdue_font_from_file!(StoreRoboto, "../resources/fonts/Roboto-Regular.ttf", scale: 32, store: true);

/// Coverage by device pixel, pen-relative, zeros left out.
type Image = HashMap<(i32, i32), u8>;

fn collect(bitmap: impl Iterator<Item = u8>, width: usize, x: i32, y: i32) -> Image {
    let mut image = Image::new();
    for (i, c) in bitmap.enumerate() {
        if c != 0 {
            image.insert((x + (i % width) as i32, y + (i / width) as i32), c);
        }
    }
    image
}

fn upright(font: &dyn FontRepr, canvas: &mut Raster, index: u16, px: f32) -> (fontdue::Metrics, Image) {
    let (m, bitmap) = font.rasterize_indexed(canvas, index, px);
    let image = collect(bitmap, m.width, m.xmin, -(m.ymin + m.height as i32));
    (m, image)
}

/// Whether the glyph's top or bottom is within rounding of a whole pixel. The upright path sizes
/// the raster from the bounds and the transformed path from the points, so there either can
/// round the edge past the pixel line and add an empty row.
fn row_edge_is_near_whole_pixel(m: &fontdue::Metrics) -> bool {
    let near = |v: f32| (v - v.round()).abs() < 1e-4;
    near(m.bounds.ymin) || near(m.bounds.ymin + m.bounds.height)
}

/// As `row_edge_is_near_whole_pixel`, for the right edge and an empty column.
fn column_edge_is_near_whole_pixel(m: &fontdue::Metrics) -> bool {
    let right = m.bounds.xmin + m.bounds.width;
    (right - right.round()).abs() < 1e-4
}

fn transformed(
    font: &dyn FontRepr,
    canvas: &mut Raster,
    index: u16,
    px: f32,
    transform: Transform,
    pen: (f32, f32),
) -> Image {
    let (m, bitmap) = font.rasterize_indexed_transformed(canvas, index, px, transform, pen);
    collect(bitmap, m.width, m.x, m.y)
}

fn max_diff(a: &Image, b: &Image) -> u8 {
    a.keys()
        .chain(b.keys())
        .map(|k| a.get(k).copied().unwrap_or(0).abs_diff(b.get(k).copied().unwrap_or(0)))
        .max()
        .unwrap_or(0)
}

fn dev_fonts() -> Vec<(String, Font)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/fonts");
    let mut fonts = vec![];
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if matches!(path.extension().and_then(|e| e.to_str()), Some("ttf" | "otf")) {
            let font = Font::from_bytes(std::fs::read(&path).unwrap(), FontSettings::default()).unwrap();
            fonts.push((path.file_name().unwrap().to_string_lossy().into_owned(), font));
        }
    }
    fonts
}

fn roboto() -> Font {
    Font::from_bytes(&include_bytes!("../resources/fonts/Roboto-Regular.ttf")[..], FontSettings::default())
        .unwrap()
}

/// Roboto at the settings scale the baked fonts use.
fn roboto_32() -> Font {
    let settings = FontSettings {
        scale: 32.0,
        ..FontSettings::default()
    };
    Font::from_bytes(&include_bytes!("../resources/fonts/Roboto-Regular.ttf")[..], settings).unwrap()
}

/// Upright pixels mapped through `f`, a map of whole pixels.
fn moved(image: &Image, f: impl Fn(i32, i32) -> (i32, i32)) -> Image {
    image.iter().map(|(&(x, y), &c)| (f(x, y), c)).collect()
}

/// Renders every glyph of every dev font upright and transformed, and requires the transformed
/// image to equal the upright one moved by `f` within `limit`.
fn matches_upright(transform: Transform, f: impl Fn(i32, i32) -> (i32, i32), limit: u8) {
    let (mut a, mut b) = (Raster::empty(), Raster::empty());
    for (name, font) in dev_fonts() {
        for px in [12.0, 32.0] {
            for index in 0..font.glyph_count() {
                let (_, want) = upright(&font, &mut a, index, px);
                let got = transformed(&font, &mut b, index, px, transform, (0.0, 0.0));
                let d = max_diff(&moved(&want, &f), &got);
                assert!(d <= limit, "{name} glyph {index} at {px} px differs by {d}");
            }
        }
    }
}

/// At the identity with a whole-pixel pen, the transformed path is the upright one: same place,
/// same size, and pixels within 1.
#[test]
fn identity_matches_upright() {
    matches_upright(Transform::IDENTITY, |x, y| (x, y), 1);
    let (mut a, mut b) = (Raster::empty(), Raster::empty());
    for (name, font) in dev_fonts() {
        for index in 0..font.glyph_count() {
            let (m, _) = font.rasterize_indexed(&mut a, index, 32.0);
            let (t, _) =
                font.rasterize_indexed_transformed(&mut b, index, 32.0, Transform::IDENTITY, (7.0, -3.0));
            if m.width * m.height == 0 {
                continue;
            }
            let top = -3 - (m.ymin + m.height as i32);
            let (row_slack, column_slack) =
                (row_edge_is_near_whole_pixel(&m) as usize, column_edge_is_near_whole_pixel(&m) as usize);
            assert!(
                t.x == 7 + m.xmin
                    && top.abs_diff(t.y) as usize <= row_slack
                    && m.width.abs_diff(t.width) <= column_slack
                    && m.height.abs_diff(t.height) <= row_slack,
                "{name} glyph {index}: {t:?} against {m:?}"
            );
        }
    }
}

/// A positive rotation turns clockwise on screen: the pixel at `(x, y)` goes to `(-y - 1, x)`.
#[test]
fn quarter_turn_is_clockwise() {
    matches_upright(Transform::rotation(0.0, 1.0), |x, y| (-y - 1, x), 1);
}

/// A mirror reverses winding, which negates the accumulated area; coverage takes its magnitude.
#[test]
fn mirror_matches_upright() {
    matches_upright(Transform::new(-1.0, 0.0, 0.0, 1.0), |x, y| (-x - 1, y), 1);
}

/// Near-singular transforms give rasters one pixel thin or less. Every write must stay inside;
/// `Sink::add` checks that in builds with debug assertions, which is how these tests run.
#[test]
fn near_singular_transforms_stay_in_bounds() {
    let font = roboto();
    let mut canvas = Raster::empty();
    let (c, s) = (30f32.to_radians().cos(), 30f32.to_radians().sin());
    for transform in [
        Transform::new(1.0, 0.0, 0.0, 1e-3),
        Transform::new(1e-3, 0.0, 0.0, 1.0),
        Transform::new(1.0, 1.0, 1.0, 1.0 + 1e-6),
        Transform::new(1.0, 0.0, 0.0, 1e-3).then(Transform::rotation(c, s)),
        Transform::rotation(c, s).then(Transform::new(1e-3, 0.0, 0.0, 1.0)),
    ] {
        for index in 0..font.glyph_count() {
            for pen in [(0.0, 0.0), (0.37, 0.91), (-12.5, 1e4 + 0.25)] {
                let (m, bitmap) =
                    font.rasterize_indexed_transformed(&mut canvas, index, 32.0, transform, pen);
                assert_eq!(bitmap.count(), m.width * m.height);
            }
        }
    }
}

/// Total coverage does not depend on the angle. Leaked area, from an open contour or a lost row
/// carry, shows up as coverage spread along whole rows.
#[test]
fn coverage_is_the_same_at_every_angle() {
    let font = roboto();
    let mut canvas = Raster::empty();
    for ch in "NESW0123456789".chars() {
        let (_, bitmap) = font.rasterize(&mut canvas, ch, 32.0);
        let want: f64 = bitmap.map(|c| c as f64 / 255.0).sum();
        for degrees in 0..360 {
            let r = (degrees as f32).to_radians();
            let pen = (0.3 + degrees as f32 * 0.01, 0.7);
            let (_, bitmap) =
                font.rasterize_transformed(&mut canvas, ch, 32.0, Transform::rotation(r.cos(), r.sin()), pen);
            let got: f64 = bitmap.map(|c| c as f64 / 255.0).sum();
            assert!((got - want).abs() <= COVERAGE_BOUND * want, "{ch} at {degrees}°: {got} against {want}");
        }
    }
}

/// `fold` and `rows` have their own loops apart from `next`'s, and `rows` leaves each row's
/// empty ends uncomputed.
#[test]
fn fold_and_rows_match_next_on_glyphs() {
    let fold = |bitmap: fontdue::raster::BitmapIter| {
        bitmap.fold(Vec::new(), |mut v, c| {
            v.push(c);
            v
        })
    };
    let rows = |bitmap: fontdue::raster::BitmapIter, width: usize| {
        let mut image = vec![0; bitmap.len()];
        bitmap.rows(&mut vec![0; width], |y, x, bytes| {
            image[y * width + x..][..bytes.len()].copy_from_slice(bytes)
        });
        image
    };
    for (name, font) in dev_fonts() {
        let mut canvas = Raster::empty();
        for index in (0..font.glyph_count()).step_by(7) {
            let (m, bitmap) = font.rasterize_indexed(&mut canvas, index, 24.0);
            let want: Vec<u8> = bitmap.clone().collect();
            assert_eq!(fold(bitmap.clone()), want, "{name} glyph {index} upright");
            assert_eq!(rows(bitmap, m.width), want, "{name} glyph {index} upright, rows");
            for degrees in [15, 45, 100, 290] {
                let r = (degrees as f32).to_radians();
                let t = Transform::rotation(r.cos(), r.sin());
                let (m, bitmap) = font.rasterize_indexed_transformed(&mut canvas, index, 24.0, t, (0.3, 0.7));
                let want: Vec<u8> = bitmap.clone().collect();
                assert_eq!(fold(bitmap.clone()), want, "{name} glyph {index} at {degrees}°");
                assert_eq!(rows(bitmap, m.width as usize), want, "{name} glyph {index} at {degrees}°, rows");
            }
        }
    }
}

/// Relative difference in total coverage allowed between a rotated glyph and the upright one.
/// Measured worst: 0.0018, '1' at 225°. Leaving out horizontal edges gives 0.15 at 1°.
const COVERAGE_BOUND: f64 = 0.005;

/// Baked lines are the runtime lines, so the two render identically at any angle; the store is
/// quantized, so it renders within a bound, and its generic and dynamic paths agree exactly.
#[test]
fn sources_agree_when_transformed() {
    let font = roboto_32();
    let (mut a, mut b, mut c, mut d) = (Raster::empty(), Raster::empty(), Raster::empty(), Raster::empty());
    let store = &StoreRoboto;
    let dynamic: &dyn FontRepr = store;
    let (mut sum, mut count, mut worst) = (0u64, 0u64, 0u8);
    for degrees in [0, 15, 45, 90, 137, 200, 315] {
        let r = (degrees as f32).to_radians();
        let t = Transform::rotation(r.cos(), r.sin());
        for index in 0..font.glyph_count() {
            let runtime = transformed(&font, &mut a, index, 20.0, t, (0.25, 0.5));
            let baked = transformed(&BakedRoboto, &mut b, index, 20.0, t, (0.25, 0.5));
            assert!(runtime == baked, "glyph {index} at {degrees}°");
            let generic = transformed(store, &mut c, index, 20.0, t, (0.25, 0.5));
            let glyph = dynamic.get_glyph_at_index(index);
            let scale = dynamic.scale_factor(20.0);
            let m = fontdue::rasterize_transformed(&mut d, &glyph, scale, t, (0.25, 0.5));
            let viadyn = collect(d.get_bitmap_iter(), m.width, m.x, m.y);
            assert!(generic == viadyn, "store glyph {index} at {degrees}°");
            for k in runtime.keys().chain(generic.keys()) {
                let e = runtime.get(k).copied().unwrap_or(0).abs_diff(generic.get(k).copied().unwrap_or(0));
                sum += e as u64;
                count += 1;
                worst = worst.max(e);
            }
        }
    }
    let mean = sum as f64 / count as f64;
    // Measured: max 1, mean 0.0044 over pixels either covers.
    assert!(worst <= 1 && mean < 0.01, "store against raw lines: max {worst}, mean {mean}");
}

#[test]
fn transform_composes_and_applies() {
    let quarter = Transform::rotation(0.0, 1.0);
    assert_eq!(quarter.apply(1.0, 0.0), (0.0, 1.0));
    assert_eq!(quarter.then(quarter).apply(1.0, 0.0), (-1.0, 0.0));
    let shear = Transform::new(1.0, 0.5, 0.0, 1.0);
    let (x, y) = shear.then(quarter).apply(2.0, 2.0);
    assert_eq!((x, y), quarter.apply(3.0, 2.0));
    assert!(std::panic::catch_unwind(|| Transform::new(1.0, 2.0, 2.0, 4.0)).is_err());
    assert!(std::panic::catch_unwind(|| Transform::new(f32::NAN, 0.0, 0.0, 1.0)).is_err());
}

#[test]
fn transformed_rejects_bad_pens() {
    for pen in [(f32::NAN, 0.0), (0.0, f32::INFINITY), (3e9, 0.0)] {
        let result = std::panic::catch_unwind(move || {
            let mut canvas = Raster::empty();
            roboto().rasterize_transformed(&mut canvas, 'A', 32.0, Transform::IDENTITY, pen).0
        });
        assert!(result.is_err(), "{pen:?}");
    }
}

/// Roboto with a fixed kerning for one pair, since no dev font has a `kern` table.
struct Kerned(Font, (u16, u16), f32);

impl FontRepr for Kerned {
    fn name(&self) -> Option<&str> {
        self.0.name()
    }
    fn file_hash(&self) -> usize {
        self.0.file_hash()
    }
    fn horizontal_line_metrics_em(&self) -> Option<fontdue::LineMetrics> {
        self.0.horizontal_line_metrics_em()
    }
    fn vertical_line_metrics_em(&self) -> Option<fontdue::LineMetrics> {
        self.0.vertical_line_metrics_em()
    }
    fn units_per_em(&self) -> f32 {
        self.0.units_per_em()
    }
    fn horizontal_kern_indexed(&self, left: u16, right: u16, px: f32) -> Option<f32> {
        ((left, right) == self.1).then(|| self.2 * self.0.scale_factor(px))
    }
    fn lookup_glyph_index(&self, character: char) -> u16 {
        self.0.lookup_glyph_index(character)
    }
    fn glyph_count(&self) -> u16 {
        self.0.glyph_count()
    }
    fn get_glyph_at_index(&self, index: u16) -> fontdue::GlyphRef<'_> {
        self.0.get_glyph_at_index(index)
    }
}

/// Offsets are the unrounded advances summed, with the pair's kerning added before the right
/// glyph, and work through `dyn FontRepr`.
#[test]
fn pen_offsets_sum_kerned_advances() {
    let base = roboto();
    let (a, v) = (base.lookup_glyph_index('A'), base.lookup_glyph_index('V'));
    let font = Kerned(base, (a, v), -74.0);
    let font: &dyn FontRepr = &font;
    let px = 17.0;
    let advance = |c| font.metrics(c, px).advance_width;
    let kern = -74.0 * font.scale_factor(px);
    let mut offsets = fontdue::PenOffsets::new(font, "TAVx", px);
    let want = [
        (font.lookup_glyph_index('T'), 0.0),
        (a, advance('T')),
        (v, advance('T') + advance('A') + kern),
        (font.lookup_glyph_index('x'), advance('T') + advance('A') + kern + advance('V')),
    ];
    for (index, offset) in want {
        let (i, o) = offsets.next().unwrap();
        assert_eq!(i, index);
        assert!((o - offset).abs() < 1e-4, "{o} against {offset}");
    }
    assert!(offsets.next().is_none());
    let total = advance('T') + advance('A') + kern + advance('V') + advance('x');
    assert!((offsets.advance() - total).abs() < 1e-4);
    assert!(advance('T').fract() != 0.0, "the check needs an advance that is not a whole pixel");
}

/// Computed at compile time, so a static buffer can be sized by it.
const BAKED_CAPACITY_24: usize = BakedRoboto::raster_capacity(24.0, 1.0);

/// The macro's `const` capacity is at least the runtime one over every character of the font,
/// and not much more, for raw and store fonts.
#[test]
fn baked_capacity_covers_the_font() {
    let font = roboto_32();
    let text: String = font.chars().keys().collect();
    // The largest singular value of the stretch, as `Transform::stretch` computes it.
    let (a, b, c, d) = (1.7f32, 0.4f32, 0.0f32, 0.8f32);
    let sum = a * a + b * b + c * c + d * d;
    let det = a * d - b * c;
    let stretch = ((sum + (sum * sum - 4.0 * det * det).sqrt()) / 2.0).sqrt();
    let fonts: [(&dyn FontRepr, fn(f32, f32) -> usize); 2] =
        [(&BakedRoboto, BakedRoboto::raster_capacity), (&StoreRoboto, StoreRoboto::raster_capacity)];
    for (font, capacity) in fonts {
        for px in [8.0, 24.0, 64.0, 300.0] {
            for (transform, stretch) in [(Transform::IDENTITY, 1.0), (Transform::new(a, b, c, d), stretch)] {
                let runtime = fontdue::transformed_raster_capacity(font, &text, px, transform);
                let baked = capacity(px, stretch);
                assert!(
                    runtime <= baked && baked <= runtime * 5 / 4 + 64,
                    "{baked} against {runtime} at {px}"
                );
            }
        }
    }
    assert_eq!(BAKED_CAPACITY_24, BakedRoboto::raster_capacity(24.0, 1.0));
}

/// The capacity fits every glyph of the text at every angle and pen, for a rotation and for a
/// rotation after a stretch, and works through `dyn FontRepr`.
#[test]
fn capacity_fits_every_rotation() {
    let font = roboto();
    let font: &dyn FontRepr = &font;
    let text = "NESW0123456789@";
    for (px, shape) in [(24.0, Transform::IDENTITY), (13.0, Transform::new(1.7, 0.4, 0.0, 0.8))] {
        let capacity = fontdue::transformed_raster_capacity(font, text, px, shape);
        let mut buffer = vec![0.0; capacity];
        let mut canvas = Raster::from_slice(&mut buffer, 0, 0).unwrap();
        let mut largest = 0;
        for degrees in 0..360 {
            let r = (degrees as f32).to_radians();
            let t = shape.then(Transform::rotation(r.cos(), r.sin()));
            let pen = (degrees as f32 * 0.37, -(degrees as f32) * 0.61);
            for c in text.chars() {
                // `from_slice` storage panics on a size that does not fit.
                let (m, _) = font.rasterize_transformed(&mut canvas, c, px, t, pen);
                largest = largest.max(m.width * m.height + 3);
            }
        }
        assert!(largest <= capacity && largest * 3 > capacity, "{largest} of {capacity}");
    }
}
