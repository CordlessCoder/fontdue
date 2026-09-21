pub mod modules;

use fontdue::{Font, FontRepr, FontSettings};
use fontdue_macros::fontdue_font_from_file;

fontdue_font_from_file!(BakedRoboto, "../resources/fonts/Roboto-Regular.ttf", scale: 32);
fontdue_font_from_file!(BakedRobotoAscii, "../resources/fonts/Roboto-Regular.ttf", scale: 32, chars: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789");

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
