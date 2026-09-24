//! Isolates the coverage pass that turns raster area deltas into bytes.
//!
//! Both arms include the draw phase, since `Raster::draw` is crate-private. Draw costs the same on
//! either side of a `BitmapIter` change, so the delta between two runs is attributable, but the
//! absolute numbers are not the cost of the coverage pass alone.

#[macro_use]
extern crate criterion;

use criterion::{black_box, BenchmarkId, Criterion};
use fontdue::raster::Raster;
use fontdue::{Font, FontRepr, FontSettings};

const MESSAGE: &str = "Sphinx of black quartz, judge my vow.";
const FONT: &[u8] = include_bytes!("../resources/fonts/Exo2-Regular.ttf");
const SIZES: [f32; 4] = [20.0, 40.0, 80.0, 200.0];

fn setup(c: &mut Criterion) {
    let mut group = c.benchmark_group("bitmap");
    group.measurement_time(core::time::Duration::from_secs(4));
    for size in SIZES.iter().copied() {
        let settings = FontSettings {
            scale: size,
            ..FontSettings::default()
        };
        let font = Font::from_bytes(FONT, settings).unwrap();

        // What dev's tests and most callers do: materialise a Vec per glyph. Goes through `next`.
        group.bench_function(BenchmarkId::from_parameter(format!("collect {}px", size)), |b| {
            b.iter(|| {
                let mut canvas = Raster::empty();
                let mut len = 0;
                for character in MESSAGE.chars() {
                    let (_, bitmap) = font.rasterize(&mut canvas, character, size);
                    len += black_box(bitmap.collect::<Vec<u8>>()).len();
                }
                len
            })
        });

        // Internal iteration, which reaches the `fold` override and allocates nothing.
        group.bench_function(BenchmarkId::from_parameter(format!("fold {}px", size)), |b| {
            b.iter(|| {
                let mut canvas = Raster::empty();
                let mut sum = 0u64;
                for character in MESSAGE.chars() {
                    let (_, bitmap) = font.rasterize(&mut canvas, character, size);
                    sum += bitmap.fold(0u64, |a, b| a + b as u64);
                }
                sum
            })
        });

        // A rotated raster, whose corners are empty row ends.
        let turn = fontdue::Transform::rotation(0.70710677, 0.70710677);
        group.bench_function(BenchmarkId::from_parameter(format!("fold 45deg {}px", size)), |b| {
            b.iter(|| {
                let mut canvas = Raster::empty();
                let mut sum = 0u64;
                for character in MESSAGE.chars() {
                    let (_, bitmap) =
                        font.rasterize_transformed(&mut canvas, character, size, turn, (0.3, 0.7));
                    sum += bitmap.fold(0u64, |a, b| a + b as u64);
                }
                sum
            })
        });

        // The same through `rows`, which hands over only each row's covered span.
        group.bench_function(BenchmarkId::from_parameter(format!("rows 45deg {}px", size)), |b| {
            let mut row = vec![0u8; 1024];
            b.iter(|| {
                let mut canvas = Raster::empty();
                let mut sum = 0u64;
                for character in MESSAGE.chars() {
                    let (_, bitmap) =
                        font.rasterize_transformed(&mut canvas, character, size, turn, (0.3, 0.7));
                    bitmap.rows(&mut row, |_, _, bytes| sum += bytes.iter().map(|&c| c as u64).sum::<u64>());
                }
                sum
            })
        });
    }
    group.finish();
}

criterion_group!(benches, setup);
criterion_main!(benches);
