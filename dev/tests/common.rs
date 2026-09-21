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
