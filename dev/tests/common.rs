pub mod modules;

use fontdue::{Font, FontRepr, FontSettings};
use fontdue_macros::fontdue_font_from_file;

fontdue_font_from_file!(BakedRoboto, "../resources/fonts/Roboto-Regular.ttf", scale: 32);
fontdue_font_from_file!(BakedRobotoAscii, "../resources/fonts/Roboto-Regular.ttf", scale: 32, chars: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789");
fontdue_font_from_file!(BakedRobotoEscaped, "../resources/fonts/Roboto-Regular.ttf", scale: 32, chars: "AB\u{00b0}");
fontdue_font_from_file!(BakedRobotoRaw, "../resources/fonts/Roboto-Regular.ttf", scale: 32, chars: r"AB\");

#[test]
fn baked_and_runtime_render_identically() {
    let runtime = Font::from_bytes(
        &include_bytes!("../resources/fonts/Roboto-Regular.ttf")[..],
        FontSettings {
            scale: 32.0,
            ..FontSettings::default()
        },
    )
    .unwrap();
    let baked = BakedRoboto;
    let mut runtime_raster = fontdue::raster::Raster::empty();
    let mut baked_raster = fontdue::raster::Raster::empty();

    assert_eq!(runtime.glyph_count(), baked.glyph_count());
    for index in 0..runtime.glyph_count() {
        let (runtime_metrics, runtime_bitmap) = runtime.rasterize_indexed(&mut runtime_raster, index, 32.0);
        let (baked_metrics, baked_bitmap) = baked.rasterize_indexed(&mut baked_raster, index, 32.0);
        assert_eq!(runtime_metrics, baked_metrics, "glyph {index}");
        assert_eq!(runtime_bitmap.collect::<Vec<_>>(), baked_bitmap.collect::<Vec<_>>(), "glyph {index}");
    }
}

#[test]
fn baked_subset_contains_only_requested_characters() {
    let baked = BakedRobotoAscii;
    assert!(baked.glyph_count() < BakedRoboto.glyph_count());
    assert_ne!(baked.lookup_glyph_index('A'), 0);
    assert_eq!(baked.lookup_glyph_index('é'), 0);
}

#[test]
fn raster_accepts_exact_caller_storage() {
    let mut storage = [1.0; 4];
    let raster = fontdue::raster::Raster::from_slice(&mut storage, 1, 1).unwrap();
    drop(raster);
    assert_eq!(storage, [0.0; 4]);
    assert!(fontdue::raster::Raster::from_slice(&mut storage[..3], 1, 1).is_none());
}

#[test]
fn subpixel_raster_has_three_columns_per_pixel() {
    let font = Font::from_bytes(
        &include_bytes!("../resources/fonts/Roboto-Regular.ttf")[..],
        FontSettings::default(),
    )
    .unwrap();
    let mut raster = fontdue::raster::Raster::empty();
    let (metrics, bitmap) = font.rasterize_subpixel(&mut raster, 'A', 32.0);
    assert_eq!(bitmap.count(), metrics.width * metrics.height * 3);
}

#[test]
fn subpixel_triplet_average_matches_grayscale() {
    let font = Font::from_bytes(
        &include_bytes!("../resources/fonts/Roboto-Regular.ttf")[..],
        FontSettings::default(),
    )
    .unwrap();
    for character in ['B', 'D', 'E', 'F'] {
        for px in [12.0, 32.0, 64.0] {
            let mut raster = fontdue::raster::Raster::empty();
            let (gray_metrics, gray_bitmap) = font.rasterize(&mut raster, character, px);
            let gray = gray_bitmap.collect::<Vec<u8>>();
            let (subpixel_metrics, subpixel_bitmap) = font.rasterize_subpixel(&mut raster, character, px);
            let subpixel = subpixel_bitmap.collect::<Vec<u8>>();

            assert_eq!(subpixel_metrics.width, gray_metrics.width, "{character} at {px}px");
            assert_eq!(subpixel_metrics.height, gray_metrics.height, "{character} at {px}px");
            let mut max_diff = 0.0f32;
            let mut total_diff = 0.0f32;
            for (gray, rgb) in gray.iter().zip(subpixel.chunks_exact(3)) {
                let average = ((u16::from(rgb[0]) + u16::from(rgb[1]) + u16::from(rgb[2]) + 1) / 3) as f32;
                let diff = (average - f32::from(*gray)).abs();
                max_diff = max_diff.max(diff);
                total_diff += diff;
            }
            let mean_diff = total_diff / gray.len() as f32;
            assert!(max_diff <= 1.0, "{character} at {px}px max diff {max_diff}");
            assert!(mean_diff < 0.1, "{character} at {px}px mean diff {mean_diff}");
        }
    }
}

#[test]
fn stretched_raster_covers_every_column_it_writes() {
    let font = Font::from_bytes(
        &include_bytes!("../resources/fonts/Roboto-Regular.ttf")[..],
        FontSettings::default(),
    )
    .unwrap();
    let glyph = font.get_glyph_at_index(font.lookup_glyph_index('W'));
    // 1.0 and 3.0 are the only stretches the public entry points use and both land on whole
    // columns. 1.5 and 2.5 do not, and `rasterize_inner` is reachable on its own.
    for stretch in [1.0f32, 1.5, 2.5, 3.0] {
        for px in [101.0f32, 202.0] {
            let scale = font.scale_factor(px);
            let mut raster = fontdue::raster::Raster::empty();
            let metrics = fontdue::rasterize_inner(&mut raster, &glyph, scale, stretch);
            let columns = raster.get_bitmap_iter().count() / metrics.height;
            let needed = (metrics.width as f32 * stretch).ceil() as usize;
            assert_eq!(columns, needed, "stretch {stretch} at {px}px");
        }
    }
}

#[test]
fn baked_subset_unescapes_the_char_literal() {
    // Trimming the quotes off the literal's source text would put `\`, `u`, `{`, `0`, `b` in the
    // subset and leave the degree sign out.
    assert_eq!(BakedRobotoEscaped.glyph_count(), 4);
    assert_ne!(BakedRobotoEscaped.lookup_glyph_index('\u{00b0}'), 0);
    assert_eq!(BakedRobotoEscaped.lookup_glyph_index('u'), 0);
    assert_eq!(BakedRobotoEscaped.lookup_glyph_index('\\'), 0);

    assert_eq!(BakedRobotoRaw.glyph_count(), 4);
    assert_ne!(BakedRobotoRaw.lookup_glyph_index('\\'), 0);
}

#[test]
fn baked_subset_tolerates_repeated_characters() {
    assert_eq!(BakedRobotoEscaped.glyph_count(), BakedRobotoRepeat.glyph_count());
}

fontdue_font_from_file!(BakedRobotoRepeat, "../resources/fonts/Roboto-Regular.ttf", scale: 32, chars: "AABB\u{00b0}");

fn roboto() -> Font {
    Font::from_bytes(&include_bytes!("../resources/fonts/Roboto-Regular.ttf")[..], FontSettings::default())
        .unwrap()
}

#[test]
fn invalid_px_panics_instead_of_writing_out_of_bounds() {
    // 1e20 used to saturate the width to usize::MAX while the height truncated to 0, so the
    // raster allocated three floats for a glyph it then indexed at 1e19.
    for px in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -32.0, 1e20, 1e30] {
        let font = roboto();
        assert!(
            std::panic::catch_unwind(move || font.metrics('A', px)).is_err(),
            "metrics should reject px = {px}"
        );
        let font = roboto();
        assert!(
            std::panic::catch_unwind(move || {
                let mut raster = fontdue::raster::Raster::empty();
                font.rasterize(&mut raster, 'A', px).1.count()
            })
            .is_err(),
            "rasterize should reject px = {px}"
        );
    }
}

#[test]
fn extreme_but_valid_px_still_renders() {
    let font = roboto();
    for px in [0.0, 1.0, 2000.0] {
        let mut raster = fontdue::raster::Raster::empty();
        let (metrics, bitmap) = font.rasterize(&mut raster, 'A', px);
        assert_eq!(bitmap.count(), metrics.width * metrics.height, "px = {px}");
    }
}

/// Replays a font's own lines as a streaming source, in half font units so the unit is exercised.
/// Vertical lines go first, as the stored-lines path draws them, so renders must match exactly.
struct ReplayedFont(Font);

// SAFETY: the points are the font's own lines, which lie inside its bounds, halved along with the
// bounds. Halving is exact. `Geometry` drops lines with no vertical extent, and Roboto's deltas
// are far from subnormal.
unsafe impl fontdue::OutlineSource for ReplayedFont {
    fn info(&self, glyph: u16) -> fontdue::OutlineInfo {
        let g = &self.0.internal_glyph_slice()[glyph as usize];
        let b = g.bounds();
        fontdue::OutlineInfo {
            bounds: fontdue::OutlineBounds {
                xmin: b.xmin / 2.0,
                ymin: b.ymin / 2.0,
                width: b.width / 2.0,
                height: b.height / 2.0,
            },
            unit: 2.0,
            advance_width: g.advance_width(),
            advance_height: g.advance_height(),
        }
    }

    fn draw(&self, glyph: u16, sink: &mut fontdue::raster::Sink<'_, '_>) {
        for segment in fontdue::SegmentSource::segments(self, glyph) {
            sink.segment(segment);
        }
    }
}

// SAFETY: the same segments `draw` passes, which lie inside the halved bounds.
unsafe impl fontdue::SegmentSource for ReplayedFont {
    type Segments<'a> = Box<dyn Iterator<Item = [f32; 4]> + 'a>;

    fn segments(&self, glyph: u16) -> Self::Segments<'_> {
        let g = &self.0.internal_glyph_slice()[glyph as usize];
        Box::new(g.v_lines().iter().chain(g.m_lines()).map(|line| {
            let (x0, y0, x1, y1) = line.coords().copied();
            [x0 / 2.0, y0 / 2.0, x1 / 2.0, y1 / 2.0]
        }))
    }
}

#[test]
fn sources_render_like_stored_lines() {
    let font = roboto();
    let source = ReplayedFont(roboto());
    let (mut want, mut generic, mut dynamic) = (
        fontdue::raster::Raster::empty(),
        fontdue::raster::Raster::empty(),
        fontdue::raster::Raster::empty(),
    );
    for index in 0..font.glyph_count() {
        for (px, stretch) in [(12.0, 1.0), (32.0, 1.0), (32.0, 3.0)] {
            let scale = font.scale_factor(px);
            let stored = font.get_glyph_at_index(index);
            let m = fontdue::rasterize_inner(&mut want, &stored, scale, stretch);
            let g = fontdue::rasterize_source(&mut generic, &source, index, scale, stretch);
            let d = fontdue::rasterize_inner(
                &mut dynamic,
                &fontdue::GlyphRef::from_source(&source, index),
                scale,
                stretch,
            );
            let want: Vec<u8> = want.get_bitmap_iter().collect();
            assert_eq!(
                (m, &want),
                (g, &generic.get_bitmap_iter().collect()),
                "glyph {index} at {px} px x{stretch}"
            );
            assert_eq!(
                (m, &want),
                (d, &dynamic.get_bitmap_iter().collect()),
                "glyph {index} at {px} px x{stretch}"
            );
        }
    }
}

fontdue_font_from_file!(StoreRoboto, "../resources/fonts/Roboto-Regular.ttf", scale: 32, store: true);

/// A store baked by the macro renders within the quantization's error of the raw baked lines, at
/// the same bitmap size.
#[test]
fn store_renders_within_tolerance_of_raw_baked() {
    let (raw, store) = (BakedRoboto, StoreRoboto);
    assert_eq!(raw.glyph_count(), store.glyph_count());
    let (mut a, mut b) = (fontdue::raster::Raster::empty(), fontdue::raster::Raster::empty());
    let (mut max, mut sum, mut n) = (0u8, 0u64, 0u64);
    for index in 0..raw.glyph_count() {
        for px in [12.0, 32.0, 64.0] {
            let (ma, ba) = raw.rasterize_indexed(&mut a, index, px);
            let (mb, bb) = store.rasterize_indexed(&mut b, index, px);
            assert_eq!((ma.width, ma.height), (mb.width, mb.height), "glyph {index} at {px} px");
            for (x, y) in ba.zip(bb) {
                let d = x.abs_diff(y);
                max = max.max(d);
                sum += d as u64;
                n += 1;
            }
        }
    }
    let mean = sum as f64 / n as f64;
    // Measured at max 1, mean 0.0026 on a 1/16 grid.
    assert!(max <= 1 && mean < 0.003, "max {max}, mean {mean}");
}

/// The macro's generic override and `GlyphRef`'s dynamic dispatch draw the same bytes.
#[test]
fn store_generic_and_dyn_agree() {
    let font = StoreRoboto;
    let (mut a, mut b) = (fontdue::raster::Raster::empty(), fontdue::raster::Raster::empty());
    for index in 0..font.glyph_count() {
        for (px, stretch) in [(12.0, 1.0), (32.0, 1.0), (32.0, 3.0)] {
            let generic: Vec<u8> = if stretch == 1.0 {
                font.rasterize_indexed(&mut a, index, px).1.collect()
            } else {
                font.rasterize_indexed_subpixel(&mut a, index, px).1.collect()
            };
            let glyph = font.get_glyph_at_index(index);
            fontdue::rasterize_inner(&mut b, &glyph, font.scale_factor(px), stretch);
            assert_eq!(
                generic,
                b.get_bitmap_iter().collect::<Vec<u8>>(),
                "glyph {index} at {px} px x{stretch}"
            );
        }
    }
}

/// A store encoded at run time, as the macro does, and viewed as the words the decoder reads.
fn encoded_roboto() -> Vec<u32> {
    use fontdue::store::encode;
    let font = roboto();
    let inputs: Vec<_> = font.internal_glyph_slice().iter().map(|g| encode::glyph_input(g, 4)).collect();
    encode::encode(&inputs, 4).chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn store_check(words: &[u32]) -> Option<fontdue::store::Error> {
    // SAFETY: reinterpreting initialized u32s as their bytes, with u32 alignment.
    let view = unsafe { core::slice::from_raw_parts(words.as_ptr() as *const u8, 4 * words.len()) };
    fontdue::store::Store::new(view).err()
}

/// Every decoded point lies inside its glyph's bounds. The raster is sized from the bounds and
/// writes without checks, so a point outside them would write out of bounds.
#[test]
fn store_points_lie_inside_bounds() {
    use fontdue::store::Store;
    let words = encoded_roboto();
    // SAFETY: as in `store_check`.
    let view = unsafe { core::slice::from_raw_parts(words.as_ptr() as *const u8, 4 * words.len()) };
    let store = Store::new(view).unwrap();
    for g in 0..store.glyph_count() {
        let b = store.grid_bounds(g);
        for [x0, y0, x1, y1] in store.lines(g) {
            for (x, y) in [(x0, y0), (x1, y1)] {
                assert!((0.0..=b.width).contains(&x) && (0.0..=b.height).contains(&y), "glyph {g}");
            }
            assert_ne!(y0, y1, "glyph {g} yielded a segment with no vertical extent");
        }
    }
}

/// `Store::new`'s walk is what makes the unchecked reads and writes sound, so it has to reject a
/// store whose decoding would leave the stream or whose points would leave their bounds. So must a
/// malformed table, which every constructor checks.
#[test]
fn corrupt_stores_are_rejected() {
    use fontdue::store::{Error, Store};
    let words = encoded_roboto();
    assert_eq!(store_check(&words), None, "the unmodified store must parse");
    // SAFETY: as in `store_check`.
    let view = unsafe { core::slice::from_raw_parts(words.as_ptr() as *const u8, 4 * words.len()) };
    let store = Store::new(view).unwrap();
    let parts = store.parts();
    let word_of = |s: &[u32]| (s.as_ptr() as usize - words.as_ptr() as usize) / 4;
    let (pools_at, glyphs_at, stream_at) =
        (word_of(parts.pool_offsets), word_of(parts.glyphs), word_of(parts.words));

    let mut far_pool = words.clone();
    far_pool[pools_at] = u32::MAX;
    assert_eq!(store_check(&far_pool), Some(Error::Corrupt));

    let mut far_glyph = words.clone();
    far_glyph[glyphs_at] = 0x7fff_fff0;
    assert_eq!(store_check(&far_glyph), Some(Error::Corrupt));

    // Two stream words, with the length field to match, so real glyphs overrun.
    let mut short = words[..stream_at + 2].to_vec();
    short[stream_at - 1] = 2;
    assert_eq!(store_check(&short), Some(Error::Corrupt));

    // A recorded size smaller than the points reach: the raster is sized from it. Five words per
    // glyph record, the size fourth.
    let g = (0..store.glyph_count()).find(|&g| store.lines(g).next().is_some()).unwrap() as usize;
    let mut small = words.clone();
    small[glyphs_at + 5 * g + 3] = 0;
    assert_eq!(store_check(&small), Some(Error::Corrupt));

    let mut empty_model = words.clone();
    empty_model[2] = 0;
    assert_eq!(store_check(&empty_model), Some(Error::BadModel));
}
