use fontdue::{Font, FontRepr, FontSettings};
use std::{env, fs};

const FONT: &[u8] = include_bytes!("../../dev/resources/fonts/Roboto-Regular.ttf");
const CASES: [(char, f32); 6] = [('A', 8.0), ('A', 32.0), ('A', 64.0), ('g', 8.0), ('@', 32.0), ('0', 64.0)];

#[test]
fn render_fixture() {
    let font = Font::from_bytes(&FONT[..], FontSettings::default()).unwrap();
    let mut raster = fontdue::raster::Raster::empty();
    let mut bytes = Vec::new();

    for (character, px) in CASES {
        let (metrics, bitmap) = font.rasterize(&mut raster, character, px);
        bytes.extend_from_slice(&metrics.width.to_le_bytes());
        bytes.extend_from_slice(&metrics.height.to_le_bytes());
        bytes.extend(bitmap);
    }

    if let Some(output) = env::var_os("FONTDUE_FEATURE_AGREEMENT_OUTPUT") {
        fs::write(output, bytes).unwrap();
    } else {
        assert!(!bytes.is_empty());
    }
}
