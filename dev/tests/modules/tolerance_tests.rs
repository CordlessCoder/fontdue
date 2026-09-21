//! Approximate baseline comparison, for changes that legitimately move coverage values.
//!
//! `baseline_tests::baseline_all` is exact and stays that way. This one decodes the same
//! reference PNGs and measures how far the current render is from them, so a rounding change can
//! be judged by how much it moved rather than by whether it moved at all. Geometry is still
//! exact: a glyph whose dimensions changed fails outright, because that is a metrics change
//! wearing a rounding change's clothes.
//!
//! Neither test replaces the other. Run the exact one to prove nothing moved; run this one to
//! bound the damage when something was always going to.

use crate::modules::{FONTS, FONT_NAMES};
use fontdue::{raster::Raster, Font, FontRepr, FontSettings};
use std::{fmt::Write as _, fs, path::Path};

extern crate png;

/// Sizes to compare. Mirrors `baseline_tests::SIZES`.
const SIZES: [f32; 4] = [8.0, 12.0, 32.0, 64.0];

/// Largest per-pixel difference any single glyph may show, in coverage units out of 255.
///
/// Zero today. The integer DDA is expected to need room here, so raise it in the commit that
/// needs it and say in the message what the new number buys. Raising it quietly defeats the test.
const MAX_ABS_DIFF: u8 = 0;

/// Largest mean absolute difference over every pixel of every compared glyph.
const MAX_MEAN_ABS_DIFF: f64 = 0.0;

struct Comparison {
    worst: Vec<(String, u8, f64)>,
    total_abs: f64,
    total_pixels: u64,
    compared: u64,
    hard_failures: Vec<String>,
}

// png 0.16's reader hands back the info up front; 0.18 moved it onto the reader. Bumping the
// crate is its own change, because the encoder in `baseline_tests` writes the 23,016 reference
// files and a different filter or compression default rewrites all of them.
fn decode(path: &Path) -> Option<(usize, usize, Vec<u8>)> {
    let decoder = png::Decoder::new(fs::File::open(path).ok()?);
    let (info, mut reader) = decoder.read_info().ok()?;
    let mut buf = vec![0; info.buffer_size()];
    reader.next_frame(&mut buf).ok()?;
    Some((info.width as usize, info.height as usize, buf))
}

fn compare_glyph(cmp: &mut Comparison, font: &Font, name: &str, index: u16, size: f32) {
    let testcase = format!("{name}/{index}-{size}px.png");
    let reference_path = format!("./resources/baselines/reference/characters/{testcase}");
    let reference_path = Path::new(&reference_path);

    let mut canvas = Raster::empty();
    let (metrics, bitmap) = font.rasterize_indexed(&mut canvas, index, size);
    let rendered = bitmap.collect::<Vec<u8>>();

    if metrics.width == 0 || metrics.height == 0 {
        if reference_path.exists() {
            cmp.hard_failures.push(format!("{testcase} no longer renders a glyph"));
        }
        return;
    }
    if !reference_path.exists() {
        cmp.hard_failures.push(format!("{testcase} has no reference baseline"));
        return;
    }

    let Some((width, height, reference)) = decode(reference_path) else {
        cmp.hard_failures.push(format!("{testcase} reference could not be decoded"));
        return;
    };
    if width != metrics.width || height != metrics.height {
        cmp.hard_failures.push(format!(
            "{testcase} is {}x{} against a {width}x{height} reference; that is a metrics change, not a rounding change",
            metrics.width, metrics.height
        ));
        return;
    }

    let mut worst = 0u8;
    let mut sum = 0u64;
    for (a, b) in rendered.iter().zip(reference.iter()) {
        let diff = a.abs_diff(*b);
        worst = worst.max(diff);
        sum += u64::from(diff);
    }
    let pixels = rendered.len() as u64;
    cmp.total_abs += sum as f64;
    cmp.total_pixels += pixels;
    cmp.compared += 1;
    if worst > 0 {
        cmp.worst.push((testcase, worst, sum as f64 / pixels as f64));
    }
}

#[test]
fn baseline_within_tolerance() {
    let mut cmp = Comparison {
        worst: vec![],
        total_abs: 0.0,
        total_pixels: 0,
        compared: 0,
        hard_failures: vec![],
    };
    for (index, bytes) in FONTS.iter().enumerate() {
        let font = Font::from_bytes(*bytes, FontSettings::default()).unwrap();
        for g in 0..font.glyph_count() {
            for size in &SIZES {
                compare_glyph(&mut cmp, &font, FONT_NAMES[index], g, *size);
            }
        }
    }

    cmp.worst.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.total_cmp(&a.2)));
    let mean = if cmp.total_pixels == 0 {
        0.0
    } else {
        cmp.total_abs / cmp.total_pixels as f64
    };
    let worst = cmp.worst.first().map_or(0, |entry| entry.1);

    // Printed on every run, not only on failure: the headroom is the number worth watching.
    println!(
        "compared {} glyphs, {} pixels: max abs diff {worst} (limit {MAX_ABS_DIFF}), mean abs diff {mean:.6} (limit {MAX_MEAN_ABS_DIFF})",
        cmp.compared, cmp.total_pixels
    );

    let mut report = String::new();
    if !cmp.hard_failures.is_empty() {
        let _ = writeln!(report, "{} glyphs could not be compared:", cmp.hard_failures.len());
        for failure in cmp.hard_failures.iter().take(20) {
            let _ = writeln!(report, "  {failure}");
        }
    }
    if worst > MAX_ABS_DIFF || mean > MAX_MEAN_ABS_DIFF {
        let _ = writeln!(
            report,
            "{} glyphs differ. max abs diff {worst} (limit {MAX_ABS_DIFF}), mean abs diff {mean:.6} (limit {MAX_MEAN_ABS_DIFF}). Worst:",
            cmp.worst.len()
        );
        for (testcase, glyph_worst, glyph_mean) in cmp.worst.iter().take(20) {
            let _ = writeln!(report, "  {testcase}: max {glyph_worst}, mean {glyph_mean:.4}");
        }
    }
    assert!(report.is_empty(), "{report}");
}
