use fontdue::raster::Raster;
use fontdue::{Font, FontRepr, FontSettings, LazyFont, Transform};

fn dev_font_files() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/fonts");
    let mut files = vec![];
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if matches!(path.extension().and_then(|e| e.to_str()), Some("ttf" | "otf")) {
            files.push((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&path).unwrap(),
            ));
        }
    }
    files.sort();
    files
}

/// The glyphs `Font` outlined at load: every mapped character's, `.notdef`, and any other that
/// has points, such as a substitution's. `Font` leaves the rest empty; `LazyFont` outlines them.
fn loaded(font: &Font) -> Vec<u16> {
    let mut indices: Vec<u16> = font.chars().values().map(|i| i.get()).collect();
    indices.push(0);
    for (i, g) in font.internal_glyph_slice().iter().enumerate() {
        if !g.points().is_empty() {
            indices.push(i as u16);
        }
    }
    indices.sort();
    indices.dedup();
    indices
}

#[test]
fn renders_like_the_eager_font() {
    let settings = FontSettings {
        scale: 32.0,
        ..FontSettings::default()
    };
    let (mut a, mut b) = (Raster::empty(), Raster::empty());
    let mut compared = 0;
    for (name, data) in dev_font_files() {
        let eager = Font::from_bytes(&data[..], settings).unwrap();
        let lazy = LazyFont::from_bytes(&data, settings).unwrap();
        assert_eq!(lazy.name(), eager.name(), "{name}");
        assert_eq!(lazy.file_hash(), eager.file_hash(), "{name}");
        assert_eq!(lazy.units_per_em(), eager.units_per_em(), "{name}");
        assert_eq!(lazy.glyph_count(), eager.glyph_count(), "{name}");
        assert_eq!(lazy.horizontal_line_metrics(20.0), eager.horizontal_line_metrics(20.0), "{name}");
        assert_eq!(lazy.vertical_line_metrics(20.0), eager.vertical_line_metrics(20.0), "{name}");
        for (&c, &index) in eager.chars() {
            assert_eq!(lazy.lookup_glyph_index(c), index.get(), "{name} {c:?}");
        }
        let turn = Transform::rotation(0.9659258, 0.25881904);
        for index in loaded(&eager) {
            assert_eq!(lazy.get_glyph_at_index(index).info(), eager.get_glyph_at_index(index).info());
            for px in [12.0, 32.0, 64.0] {
                assert_eq!(
                    lazy.metrics_indexed(index, px),
                    eager.metrics_indexed(index, px),
                    "{name} {index}"
                );
            }
            for (px, subpixel) in [(12.0, false), (32.0, false), (32.0, true)] {
                let (m, want) = if subpixel {
                    eager.rasterize_indexed_subpixel(&mut a, index, px)
                } else {
                    eager.rasterize_indexed(&mut a, index, px)
                };
                let (n, got) = if subpixel {
                    lazy.rasterize_indexed_subpixel(&mut b, index, px)
                } else {
                    lazy.rasterize_indexed(&mut b, index, px)
                };
                assert_eq!(m, n, "{name} {index} at {px} px");
                assert!(want.eq(got), "{name} {index} at {px} px, subpixel {subpixel}");
            }
            let (m, want) = eager.rasterize_indexed_transformed(&mut a, index, 20.0, turn, (3.25, 7.5));
            let (n, got) = lazy.rasterize_indexed_transformed(&mut b, index, 20.0, turn, (3.25, 7.5));
            assert_eq!(m, n, "{name} {index} turned");
            assert!(want.eq(got), "{name} {index} turned");
            compared += 1;
        }
    }
    assert!(compared > 1000);
}

/// Drawing through `get_glyph_at_index`, which visits the outline through `OutlineSource`, gives
/// the same bytes as the overridden methods.
#[test]
fn dynamic_glyphs_render_like_the_overrides() {
    let data =
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/fonts/Roboto-Regular.ttf")).unwrap();
    let lazy = LazyFont::from_bytes(&data, FontSettings::default()).unwrap();
    let (mut a, mut b) = (Raster::empty(), Raster::empty());
    for c in ('!'..='~').chain(['é', 'ß', 'Ω']) {
        let index = lazy.lookup_glyph_index(c);
        let scale = lazy.scale_factor(24.0);
        let (m, want) = lazy.rasterize_indexed(&mut a, index, 24.0);
        let n = fontdue::rasterize_inner(&mut b, &lazy.get_glyph_at_index(index), scale, 1.0);
        assert_eq!(m, n, "{c:?}");
        assert!(want.eq(b.get_bitmap_iter()), "{c:?}");
    }
}

#[test]
fn kerns_like_the_eager_font() {
    let mut kerned = 0;
    for (name, data) in dev_font_files() {
        let eager = Font::from_bytes(&data[..], FontSettings::default()).unwrap();
        let lazy = LazyFont::from_bytes(&data, FontSettings::default()).unwrap();
        let Some(pairs) = eager.internal_horizontal_kern_map() else {
            continue;
        };
        for &key in pairs.keys() {
            let (left, right) = ((key >> 16) as u16, key as u16);
            let want = eager.horizontal_kern_indexed(left, right, 17.0);
            assert_eq!(lazy.horizontal_kern_indexed(left, right, 17.0), want, "{name} {left} {right}");
            kerned += 1;
        }
        let chars: Vec<char> = ('A'..='z').collect();
        for &l in &chars {
            for &r in &chars {
                assert_eq!(
                    lazy.horizontal_kern(l, r, 17.0),
                    eager.horizontal_kern(l, r, 17.0),
                    "{name} {l}{r}"
                );
            }
        }
    }
    assert!(kerned > 0, "no font in the corpus has a kern table");
}
