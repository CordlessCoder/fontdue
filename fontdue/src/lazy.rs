//! A font read from its TTF bytes on demand, keeping nothing per glyph.

use crate::font::{LineMetrics, Metrics, convert_error, convert_name, metrics_raw, outline_glyph};
use crate::outline::{GlyphRef, OutlineInfo, OutlineSource, PathEvent, PathGlyph, each_contour};
use crate::raster::{BitmapIter, Raster};
use crate::{FontRepr, FontResult, FontSettings, Glyph, Transform, TransformedMetrics};
use alloc::string::String;
use ttf_parser::kern::{Format, Subtable0};
use ttf_parser::{Face, GlyphId};

/// A font that outlines each glyph from the font file when it is asked for, instead of all of them
/// at load as [`Font`](crate::Font) does. It holds no glyph data, and renders byte-identical to a
/// `Font` loaded with the same settings.
///
/// Every draw and every metrics call outlines the glyph again, into memory freed afterwards. Wrap it
/// in a cache to keep outlines between calls.
pub struct LazyFont<'a> {
    face: Face<'a>,
    name: Option<String>,
    hash: usize,
    units_per_em: f32,
    /// The px size flattening is tuned for, `FontSettings::scale`.
    scale: f32,
    horizontal_line_metrics: Option<LineMetrics>,
    vertical_line_metrics: Option<LineMetrics>,
    kern: Option<Subtable0<'a>>,
}

impl<'a> LazyFont<'a> {
    /// Parses the font's tables from `data`. `settings.load_substitutions` has no effect: every
    /// glyph is available.
    pub fn from_bytes(data: &'a [u8], settings: FontSettings) -> FontResult<LazyFont<'a>> {
        let face = Face::parse(data, settings.collection_index).map_err(convert_error)?;
        // `Font` takes the first horizontal format 0 subtable.
        let kern = face.tables().kern.and_then(|kern| {
            kern.subtables.into_iter().find_map(|subtable| match subtable.format {
                Format::Format0(pairs) if subtable.horizontal => Some(pairs),
                _ => None,
            })
        });
        let vertical_line_metrics = face.vertical_ascender().map(|ascender| {
            LineMetrics::new(
                ascender,
                face.vertical_descender().unwrap_or(0),
                face.vertical_line_gap().unwrap_or(0),
            )
        });
        Ok(LazyFont {
            name: convert_name(&face),
            hash: crate::hash::hash(data),
            units_per_em: face.units_per_em() as f32,
            scale: settings.scale,
            horizontal_line_metrics: Some(LineMetrics::new(
                face.ascender(),
                face.descender(),
                face.line_gap(),
            )),
            vertical_line_metrics,
            kern,
            face,
        })
    }

    /// Glyph `index`, outlined now. An index past the font's glyphs is an empty glyph.
    fn glyph(&self, index: u16) -> Glyph {
        outline_glyph(&self.face, index, self.scale, self.units_per_em)
    }
}

// SAFETY: every glyph comes from `outline_glyph`, whose `Geometry::finalize` places its points
// inside the bounds it records and flushes coordinates below `SMALLEST_COORDINATE` to zero. The
// face does not change, so the same glyph outlines to the same bounds every time.
unsafe impl OutlineSource for LazyFont<'_> {
    fn info(&self, glyph: u16) -> OutlineInfo {
        PathGlyph::from_glyph(&self.glyph(glyph)).info()
    }

    fn visit(&self, glyph: u16, f: &mut dyn FnMut(PathEvent)) {
        let glyph = self.glyph(glyph);
        each_contour(&glyph.points, &glyph.contours, |&first, rest| {
            f(PathEvent::MoveTo(first));
            for &p in rest {
                f(PathEvent::LineTo(p));
            }
        });
    }
}

impl FontRepr for LazyFont<'_> {
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn file_hash(&self) -> usize {
        self.hash
    }

    fn horizontal_line_metrics_em(&self) -> Option<LineMetrics> {
        self.horizontal_line_metrics
    }

    fn vertical_line_metrics_em(&self) -> Option<LineMetrics> {
        self.vertical_line_metrics
    }

    fn units_per_em(&self) -> f32 {
        self.units_per_em
    }

    fn horizontal_kern_indexed(&self, left: u16, right: u16, px: f32) -> Option<f32> {
        let scale = self.scale_factor(px);
        let value = self.kern.as_ref()?.glyphs_kerning(GlyphId(left), GlyphId(right))?;
        Some(value as f32 * scale)
    }

    /// As `Font` builds its map: the last `cmap` subtable that maps `character` to a glyph wins.
    fn lookup_glyph_index(&self, character: char) -> u16 {
        let Some(cmap) = self.face.tables().cmap else {
            return 0;
        };
        let subtables = cmap.subtables;
        (0..subtables.len())
            .rev()
            .find_map(|i| subtables.get(i)?.glyph_index(character as u32).filter(|id| id.0 != 0))
            .map_or(0, |id| id.0)
    }

    fn glyph_count(&self) -> u16 {
        self.face.number_of_glyphs()
    }

    /// Outlines the glyph for its info, and again each time it is drawn. The `metrics` and
    /// `rasterize` methods outline once per call.
    fn get_glyph_at_index(&self, index: u16) -> GlyphRef<'_> {
        GlyphRef::from_source(self, index)
    }

    fn with_glyph_at_index(&self, index: u16, f: &mut dyn FnMut(&GlyphRef<'_>)) {
        f(&GlyphRef::from_glyph(&self.glyph(index)))
    }

    fn metrics_indexed(&self, index: u16, px: f32) -> Metrics {
        let scale = self.scale_factor(px);
        metrics_raw(scale, &GlyphRef::from_glyph(&self.glyph(index)), 0.0).0
    }

    fn rasterize_indexed<'r>(
        &self,
        canvas: &'r mut Raster<'_>,
        index: u16,
        px: f32,
    ) -> (Metrics, BitmapIter<'r>) {
        rasterize_upright(self, canvas, index, px, 1.0)
    }

    fn rasterize_indexed_subpixel<'r>(
        &self,
        canvas: &'r mut Raster<'_>,
        index: u16,
        px: f32,
    ) -> (Metrics, BitmapIter<'r>) {
        rasterize_upright(self, canvas, index, px, 3.0)
    }

    fn rasterize_indexed_transformed<'r>(
        &self,
        canvas: &'r mut Raster<'_>,
        index: u16,
        px: f32,
        transform: Transform,
        pen: (f32, f32),
    ) -> (TransformedMetrics, BitmapIter<'r>) {
        if px == 0.0 {
            canvas.resize(0, 0);
            return (TransformedMetrics::default(), canvas.get_bitmap_iter());
        }
        let scale = self.scale_factor(px);
        let glyph = self.glyph(index);
        let metrics =
            crate::rasterize_transformed(canvas, &GlyphRef::from_glyph(&glyph), scale, transform, pen);
        (metrics, canvas.get_bitmap_iter())
    }
}

fn rasterize_upright<'r>(
    font: &LazyFont<'_>,
    canvas: &'r mut Raster<'_>,
    index: u16,
    px: f32,
    stretch: f32,
) -> (Metrics, BitmapIter<'r>) {
    if px == 0.0 {
        canvas.resize(0, 0);
        return (Metrics::default(), canvas.get_bitmap_iter());
    }
    let scale = font.scale_factor(px);
    let glyph = font.glyph(index);
    let metrics = crate::rasterize_inner(canvas, &GlyphRef::from_glyph(&glyph), scale, stretch);
    (metrics, canvas.get_bitmap_iter())
}
