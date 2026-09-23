use fontdue::raster::Raster;
use fontdue::{Cached, Font, FontRepr, FontSettings, GlyphRef, LazyFont, LineMetrics, Transform};
use fontdue_macros::fontdue_font_from_file;
use std::sync::atomic::{AtomicUsize, Ordering};

fontdue_font_from_file!(StoreRoboto, "../resources/fonts/Roboto-Regular.ttf", scale: 32, store: true);

const ROBOTO: &[u8] = include_bytes!("../resources/fonts/Roboto-Regular.ttf");

fn settings() -> FontSettings {
    FontSettings {
        scale: 32.0,
        ..FontSettings::default()
    }
}

/// Every way a glyph is measured or drawn, as bytes.
fn render(font: &dyn FontRepr, canvas: &mut Raster, index: u16) -> Vec<Vec<u8>> {
    let mut out = vec![];
    for px in [12.0, 40.0] {
        out.push(format!("{:?}", font.metrics_indexed(index, px)).into_bytes());
        let (m, bitmap) = font.rasterize_indexed(canvas, index, px);
        out.push(format!("{m:?}").into_bytes());
        out.push(bitmap.collect());
    }
    let (m, bitmap) = font.rasterize_indexed_subpixel(canvas, index, 20.0);
    out.push(format!("{m:?}").into_bytes());
    out.push(bitmap.collect());
    let turn = Transform::rotation(0.8, 0.6);
    let (m, bitmap) = font.rasterize_indexed_transformed(canvas, index, 24.0, turn, (1.5, 2.25));
    out.push(format!("{m:?}").into_bytes());
    out.push(bitmap.collect());
    let glyph = font.get_glyph_at_index(index);
    out.push(format!("{:?}", glyph.info()).into_bytes());
    fontdue::rasterize_inner(canvas, &glyph, font.scale_factor(16.0), 1.0);
    out.push(canvas.get_bitmap_iter().collect());
    out
}

fn roboto_indices() -> Vec<u16> {
    let font = Font::from_bytes(ROBOTO, settings()).unwrap();
    let mut indices: Vec<u16> = font.chars().values().map(|i| i.get()).collect();
    indices.push(0);
    indices.sort();
    indices.dedup();
    indices
}

#[test]
fn cached_lazy_font_renders_like_the_eager_font() {
    let eager = Font::from_bytes(ROBOTO, settings()).unwrap();
    let mut words = vec![0u32; 20_000];
    let cached = Cached::new(LazyFont::from_bytes(ROBOTO, settings()).unwrap(), &mut words);
    let (mut a, mut b) = (Raster::empty(), Raster::empty());
    let indices = roboto_indices();
    // Twice: the first pass fills the arena and evicts, the second finds some glyphs and not others.
    for _ in 0..2 {
        for &index in &indices {
            assert_eq!(render(&cached, &mut b, index), render(&eager, &mut a, index), "glyph {index}");
        }
    }
}

#[test]
fn cached_store_renders_like_the_store() {
    let mut words = vec![0u32; 4_000];
    let cached = Cached::new(StoreRoboto, &mut words);
    let (mut a, mut b) = (Raster::empty(), Raster::empty());
    for _ in 0..2 {
        for index in 0..StoreRoboto.glyph_count() {
            assert_eq!(render(&cached, &mut b, index), render(&StoreRoboto, &mut a, index), "glyph {index}");
        }
    }
}

#[test]
fn glyphs_larger_than_the_arena_draw_uncached() {
    let eager = Font::from_bytes(ROBOTO, settings()).unwrap();
    let mut words = [0u32; 12];
    let cached = Cached::new(LazyFont::from_bytes(ROBOTO, settings()).unwrap(), &mut words);
    let (mut a, mut b) = (Raster::empty(), Raster::empty());
    for c in ['A', 'g', '@', ' '] {
        let index = eager.lookup_glyph_index(c);
        assert_eq!(render(&cached, &mut b, index), render(&eager, &mut a, index), "{c:?}");
    }
}

/// A font that counts its decodes.
struct Counting<'a> {
    font: LazyFont<'a>,
    decodes: AtomicUsize,
}

impl FontRepr for Counting<'_> {
    fn name(&self) -> Option<&str> {
        self.font.name()
    }
    fn file_hash(&self) -> usize {
        self.font.file_hash()
    }
    fn horizontal_line_metrics_em(&self) -> Option<LineMetrics> {
        self.font.horizontal_line_metrics_em()
    }
    fn vertical_line_metrics_em(&self) -> Option<LineMetrics> {
        self.font.vertical_line_metrics_em()
    }
    fn units_per_em(&self) -> f32 {
        self.font.units_per_em()
    }
    fn horizontal_kern_indexed(&self, left: u16, right: u16, px: f32) -> Option<f32> {
        self.font.horizontal_kern_indexed(left, right, px)
    }
    fn lookup_glyph_index(&self, character: char) -> u16 {
        self.font.lookup_glyph_index(character)
    }
    fn glyph_count(&self) -> u16 {
        self.font.glyph_count()
    }
    fn get_glyph_at_index(&self, index: u16) -> GlyphRef<'_> {
        self.font.get_glyph_at_index(index)
    }
    fn with_glyph_at_index(&self, index: u16, f: &mut dyn FnMut(&GlyphRef<'_>)) {
        self.decodes.fetch_add(1, Ordering::Relaxed);
        self.font.with_glyph_at_index(index, f)
    }
}

/// Hits do not decode; filling the arena evicts the oldest glyph first.
#[test]
fn keeps_glyphs_until_the_oldest_must_go() {
    let eager = Font::from_bytes(ROBOTO, settings()).unwrap();
    // The words the cache stores for a glyph, from the eager font's identical copy.
    let words_for = |c: char| {
        let g = &eager.internal_glyph_slice()[eager.lookup_glyph_index(c) as usize];
        10 + g.contours().len() + 2 * g.points().len()
    };
    let (l, o, i, x) = (words_for('l'), words_for('o'), words_for('i'), words_for('x'));
    assert!(x <= l + o, "the test needs 'x' to fit where 'l' and 'o' were");
    let font = Counting {
        font: LazyFont::from_bytes(ROBOTO, settings()).unwrap(),
        decodes: AtomicUsize::new(0),
    };
    // Room for exactly 'l', 'o' and 'i'.
    let mut words = vec![0u32; l + o + i];
    let cached = Cached::new(font, &mut words);
    let decodes = || cached.font().decodes.load(Ordering::Relaxed);
    let metrics = |c: char| cached.metrics(c, 20.0);
    for c in ['l', 'o', 'i'] {
        metrics(c);
    }
    assert_eq!(decodes(), 3);
    for c in ['i', 'l', 'o', 'l'] {
        metrics(c);
    }
    assert_eq!(decodes(), 3, "hits decode nothing");
    metrics('x');
    assert_eq!(decodes(), 4);
    metrics('i');
    assert_eq!(decodes(), 4, "'i', the newest, survives making room for 'x'");
    metrics('l');
    assert_eq!(decodes(), 5, "'l', the oldest, was evicted");
}

/// A glyph being read is pinned: a miss that would have to evict it is drawn without being kept,
/// and the pinned glyph stays.
#[test]
fn pinned_glyphs_are_not_evicted() {
    let eager = Font::from_bytes(ROBOTO, settings()).unwrap();
    let words_for = |c: char| {
        let g = &eager.internal_glyph_slice()[eager.lookup_glyph_index(c) as usize];
        10 + g.contours().len() + 2 * g.points().len()
    };
    let font = Counting {
        font: LazyFont::from_bytes(ROBOTO, settings()).unwrap(),
        decodes: AtomicUsize::new(0),
    };
    let mut words = vec![0u32; words_for('l') + words_for('o')];
    let cached = Cached::new(font, &mut words);
    let decodes = || cached.font().decodes.load(Ordering::Relaxed);
    cached.metrics('l', 20.0);
    cached.metrics('o', 20.0);
    assert_eq!(decodes(), 2);
    let (mut a, mut b) = (Raster::empty(), Raster::empty());
    let x = eager.lookup_glyph_index('x');
    let mut first = true;
    fontdue::OutlineSource::visit(&cached, eager.lookup_glyph_index('l'), &mut |_| {
        if std::mem::take(&mut first) {
            // 'l' is pinned, and 'x' can only fit by evicting it.
            assert_eq!(render(&cached, &mut b, x), render(&eager, &mut a, x));
        }
    });
    let after_x = decodes();
    assert!(after_x > 2, "'x' was decoded");
    cached.metrics('l', 20.0);
    cached.metrics('o', 20.0);
    assert_eq!(decodes(), after_x, "'l' and 'o' are still kept");
}

#[test]
fn threads_share_a_cached_font() {
    fn assert_sync<T: Sync>(_: &T) {}
    let eager = Font::from_bytes(ROBOTO, settings()).unwrap();
    let mut words = vec![0u32; 3_000];
    let cached = Cached::new(LazyFont::from_bytes(ROBOTO, settings()).unwrap(), &mut words);
    assert_sync(&cached);
    let indices = roboto_indices();
    std::thread::scope(|s| {
        for t in 0..4 {
            let (cached, eager, indices) = (&cached, &eager, &indices);
            s.spawn(move || {
                let (mut a, mut b) = (Raster::empty(), Raster::empty());
                for round in 0..3 {
                    for (i, &index) in indices.iter().enumerate() {
                        if (i + t + round) % 3 == 0 {
                            assert_eq!(render(cached, &mut b, index), render(eager, &mut a, index));
                        }
                    }
                }
            });
        }
    });
}
