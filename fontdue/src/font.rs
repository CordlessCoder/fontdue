use crate::FontResult;
pub use crate::fontrepr::FontRepr;
use crate::math::Geometry;
use crate::outline::{GlyphRef, OutlineInfo, PathSource};
use crate::platform::{
    abs, as_i32_unchecked, ceil, ceil_i32_unchecked, floor, floor_i32_unchecked, fract, is_negative,
};
use crate::raster::{Lines, Raster, Sink};
use crate::table::{TableKern, load_gsub};
use crate::unicode;
use crate::{HashMap, HashSet};
use alloc::string::String;
use alloc::vec;
use alloc::vec::*;
use core::hash::{Hash, Hasher};
use core::num::NonZeroU16;
use core::ops::Deref;
use ttf_parser::{Face, FaceParsingError, GlyphId, Tag};

#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// Defines the bounds for a glyph's outline in subpixels. A glyph's outline is always contained in
/// its bitmap.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct OutlineBounds {
    /// Subpixel offset of the left-most edge of the glyph's outline.
    pub xmin: f32,
    /// Subpixel offset of the bottom-most edge of the glyph's outline.
    pub ymin: f32,
    /// The width of the outline in subpixels.
    pub width: f32,
    /// The height of the outline in subpixels.
    pub height: f32,
}

impl Default for OutlineBounds {
    fn default() -> Self {
        Self {
            xmin: 0.0,
            ymin: 0.0,
            width: 0.0,
            height: 0.0,
        }
    }
}

impl OutlineBounds {
    /// Scales the bounding box by the given factor.
    #[inline(always)]
    pub fn scale(&self, scale: f32) -> OutlineBounds {
        OutlineBounds {
            xmin: self.xmin * scale,
            ymin: self.ymin * scale,
            width: self.width * scale,
            height: self.height * scale,
        }
    }
}

/// Encapsulates all layout information associated with a glyph for a fixed scale.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Metrics {
    /// Whole pixel offset of the left-most edge of the bitmap. This may be negative to reflect the
    /// glyph is positioned to the left of the origin.
    pub xmin: i32,
    /// Whole pixel offset of the bottom-most edge of the bitmap. This may be negative to reflect
    /// the glyph is positioned below the baseline.
    pub ymin: i32,
    /// The width of the bitmap in whole pixels.
    pub width: usize,
    /// The height of the bitmap in whole pixels.
    pub height: usize,
    /// Advance width of the glyph in subpixels. Used in horizontal fonts.
    pub advance_width: f32,
    /// Advance height of the glyph in subpixels. Used in vertical fonts.
    pub advance_height: f32,
    /// The bounding box that contains the glyph's outline at the offsets specified by the font.
    /// This is always a smaller box than the bitmap bounds.
    pub bounds: OutlineBounds,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            xmin: 0,
            ymin: 0,
            width: 0,
            height: 0,
            advance_width: 0.0,
            advance_height: 0.0,
            bounds: OutlineBounds::default(),
        }
    }
}

/// Metrics associated with line positioning.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct LineMetrics {
    /// The highest point that any glyph in the font extends to above the baseline. Typically
    /// positive.
    pub ascent: f32,
    /// The lowest point that any glyph in the font extends to below the baseline. Typically
    /// negative.
    pub descent: f32,
    /// The gap to leave between the descent of one line and the ascent of the next. This is of
    /// course only a guideline given by the font's designers.
    pub line_gap: f32,
    /// A precalculated value for the height or width of the line depending on if the font is laid
    /// out horizontally or vertically. It's calculated by: ascent - descent + line_gap.
    pub new_line_size: f32,
}

impl LineMetrics {
    /// Creates a new line metrics struct and computes the new line size.
    pub(crate) fn new(ascent: i16, descent: i16, line_gap: i16) -> LineMetrics {
        // Operations between this values can exceed i16, so we extend to i32 here.
        let (ascent, descent, line_gap) = (ascent as i32, descent as i32, line_gap as i32);
        LineMetrics {
            ascent: ascent as f32,
            descent: descent as f32,
            line_gap: line_gap as f32,
            new_line_size: (ascent - descent + line_gap) as f32,
        }
    }

    /// Scales the line metrics by the given factor.
    #[inline(always)]
    #[doc(hidden)]
    pub fn scale(&self, scale: f32) -> LineMetrics {
        LineMetrics {
            ascent: self.ascent * scale,
            descent: self.descent * scale,
            line_gap: self.line_gap * scale,
            new_line_size: self.new_line_size * scale,
        }
    }
}

/// Stores compiled geometry and metric information.
#[derive(Clone)]
#[doc(hidden)]
pub struct Glyph {
    /// Points in bounds-local units, y down, in contour order. Each contour ends on its first
    /// point.
    pub(crate) points: Vec<[f32; 2]>,
    /// End index in `points` of each contour.
    pub(crate) contours: Vec<u32>,
    pub(crate) advance_width: f32,
    pub(crate) advance_height: f32,
    pub(crate) bounds: OutlineBounds,
}

impl Glyph {
    pub fn points(&self) -> &[[f32; 2]] {
        &self.points
    }

    pub fn contours(&self) -> &[u32] {
        &self.contours
    }

    pub fn bounds(&self) -> OutlineBounds {
        self.bounds
    }

    pub fn advance_width(&self) -> f32 {
        self.advance_width
    }

    pub fn advance_height(&self) -> f32 {
        self.advance_height
    }
}

impl Default for Glyph {
    fn default() -> Self {
        Glyph {
            points: Vec::new(),
            contours: Vec::new(),
            advance_width: 0.0,
            advance_height: 0.0,
            bounds: OutlineBounds::default(),
        }
    }
}

/// Settings for controlling specific font and layout behavior.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct FontSettings {
    /// The default is 0. The index of the font to use if parsing a font collection.
    pub collection_index: u32,
    /// The default is 40. The scale in px the font geometry is optimized for. Fonts rendered at
    /// the scale defined here will be the most optimal in terms of looks and performance. Glyphs
    /// rendered smaller than this scale will look the same but perform slightly worse, while
    /// glyphs rendered larger than this will looks worse but perform slightly better. The units of
    /// the scale are pixels per Em unit.
    pub scale: f32,
    /// The default is true. If enabled, will load glyphs for substitutions (liagtures, etc.) from
    /// the gsub table on compatible fonts. Only makes a difference when using indexed operations,
    /// i.e. `Font::raserize_indexed`, as singular characters do not have enough context to be
    /// substituted.
    pub load_substitutions: bool,
}

impl Default for FontSettings {
    fn default() -> FontSettings {
        FontSettings {
            collection_index: 0,
            scale: 40.0,
            load_substitutions: true,
        }
    }
}

/// Represents a font. Fonts are immutable after creation and owns its own copy of the font data.
#[derive(Clone)]
pub struct Font {
    name: Option<String>,
    units_per_em: f32,
    glyphs: Vec<Glyph>,
    // char_to_glyph: CharToGlyph,
    char_to_glyph: HashMap<char, NonZeroU16>,
    horizontal_line_metrics: Option<LineMetrics>,
    horizontal_kern: Option<HashMap<u32, i16>>,
    vertical_line_metrics: Option<LineMetrics>,
    settings: FontSettings,
    hash: usize,
}

impl Font {
    #[doc(hidden)]
    #[inline(always)]
    pub fn internal_horizontal_kern_map(&self) -> &Option<HashMap<u32, i16>> {
        &self.horizontal_kern
    }

    #[doc(hidden)]
    #[inline(always)]
    pub fn internal_glyph_slice(&self) -> &[Glyph] {
        &self.glyphs
    }
}

impl Hash for Font {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

impl core::fmt::Debug for Font {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Font")
            .field("name", &self.name)
            .field("settings", &self.settings)
            .field("units_per_em", &self.units_per_em)
            .field("hash", &self.hash)
            .finish()
    }
}

/// Converts a ttf-parser FaceParsingError into a string.
pub(crate) fn convert_error(error: FaceParsingError) -> &'static str {
    use FaceParsingError::*;
    match error {
        MalformedFont => "An attempt to read out of bounds detected.",
        UnknownMagic => "Face data must start with 0x00010000, 0x74727565, 0x4F54544F or 0x74746366.",
        FaceIndexOutOfBounds => "The face index is larger than the number of faces in the font.",
        NoHeadTable => "The head table is missing or malformed.",
        NoHheaTable => "The hhea table is missing or malformed.",
        NoMaxpTable => "The maxp table is missing or malformed.",
    }
}

pub(crate) fn convert_name(face: &Face) -> Option<String> {
    for name in face.names() {
        if name.name_id == 4 && name.is_unicode() {
            return Some(unicode::decode_utf16(name.name));
        }
    }
    None
}

/// Largest `f32` below `i32::MAX`. Above this `as_i32` saturates instead of converting.
pub(crate) const MAX_DIMENSION: f32 = 2147483520.0;

/// Internal function to generate the metrics, offset_x, and offset_y of the glyph.
///
/// # Panics
///
/// If `scale` puts the glyph's bounds outside `i32`. That covers a NaN, infinite or negative
/// `px`, none of which have a meaningful raster.
#[doc(hidden)]
pub fn metrics_raw(scale: f32, glyph: &GlyphRef<'_>, offset: f32) -> (Metrics, f32, f32) {
    metrics_raw_stretched(scale, &glyph.info(), offset, 1.0)
}

/// The metrics, and the subpixel offsets the draw adds to every point. Bounds and points are
/// scaled by the same `scale * unit`, so a point inside the bounds lands inside the raster.
#[inline(always)]
pub(crate) fn metrics_raw_stretched(
    scale: f32,
    glyph: &OutlineInfo,
    offset: f32,
    stretch: f32,
) -> (Metrics, f32, f32) {
    let bounds = glyph.bounds.scale(scale * glyph.unit);
    // The draw places points down from the glyph's top edge, so `ymin` is the raster's top less
    // its height. Floored from the bottom edge instead, it disagrees by a row wherever rounding
    // puts an edge on the other side of a whole pixel.

    // Below 2^22 every value converted here is under 2^23, where a float has a fraction and its
    // conversions fit `i32`, so one check replaces the guard in each of six conversions. NaN
    // fails it. Past it, `outside` does the same arithmetic with the guarded forms.
    const FAST: f32 = 4194304.0;
    let fast = abs(bounds.xmin) < FAST
        && abs(bounds.ymin) < FAST
        && abs(bounds.width) < FAST
        && abs(bounds.height) < FAST
        && abs(offset) < FAST;
    let ([xmin, ymin, width, height], offset_x, offset_y) = if fast {
        // SAFETY: every argument is under 2^23 in magnitude, by the check above.
        let fract = |v: f32| v - unsafe { as_i32_unchecked(v) } as f32;
        let top = bounds.ymin + bounds.height;
        let mut offset_x = fract(bounds.xmin + offset);
        let mut offset_y = fract(-top);
        // Unlike the guarded `fract`, this one gives -0 for -0; either way no offset is added.
        if offset_x < 0.0 {
            offset_x += 1.0;
        }
        if offset_y < 0.0 {
            offset_y += 1.0;
        }
        // SAFETY: as above; the offsets are in [0, 1).
        let (xmin, top, width, height) = unsafe {
            (
                floor_i32_unchecked(bounds.xmin),
                ceil_i32_unchecked(top),
                ceil_i32_unchecked(bounds.width + offset_x),
                ceil_i32_unchecked(bounds.height + offset_y),
            )
        };
        let ymin = top - height;
        // The guarded path's range check, reduced to what can fail below 2^23.
        if !(width >= 0 && height >= 0 && (0.0..=MAX_DIMENSION).contains(&(width as f32 * stretch))) {
            out_of_range(scale);
        }
        ([xmin, ymin, width, height], offset_x, offset_y)
    } else {
        outside(scale, &bounds, offset, stretch)
    };
    let metrics = Metrics {
        xmin,
        ymin,
        width: width as usize,
        height: height as usize,
        advance_width: scale * glyph.advance_width,
        advance_height: scale * glyph.advance_height,
        bounds,
    };
    (metrics, offset_x, offset_y)
}

/// `metrics_raw_stretched` for bounds or an offset of 2^22 or more, or NaN.
#[cold]
fn outside(scale: f32, bounds: &OutlineBounds, offset: f32, stretch: f32) -> ([i32; 4], f32, f32) {
    let top = bounds.ymin + bounds.height;
    let mut offset_x = fract(bounds.xmin + offset);
    let mut offset_y = fract(-top);
    if is_negative(offset_x) {
        offset_x += 1.0;
    }
    if is_negative(offset_y) {
        offset_y += 1.0;
    }
    let xmin = floor(bounds.xmin);
    let width = ceil(bounds.width + offset_x);
    let height = ceil(bounds.height + offset_y);
    let ymin = ceil(top) - height;
    // Every later stage trusts these dimensions: `resize` sizes the buffer from them and `add`
    // indexes it with `get_unchecked_mut`. A px large enough to saturate the width while the
    // height truncates to zero would size the buffer at three floats and then write past it. The
    // range check rejects that, and rejects NaN, infinite and negative px with it, because none
    // of those compare inside the range. It is also what makes the conversions below sound.
    if !((-MAX_DIMENSION..=MAX_DIMENSION).contains(&xmin)
        && (-MAX_DIMENSION..=MAX_DIMENSION).contains(&ymin)
        && (0.0..=MAX_DIMENSION).contains(&width)
        && (0.0..=MAX_DIMENSION).contains(&height)
        && (0.0..=MAX_DIMENSION).contains(&(width * stretch)))
    {
        out_of_range(scale);
    }
    // SAFETY: the range check above bounds all four inside `i32` and rejects NaN.
    let dimensions = unsafe {
        [as_i32_unchecked(xmin), as_i32_unchecked(ymin), as_i32_unchecked(width), as_i32_unchecked(height)]
    };
    (dimensions, offset_x, offset_y)
}

#[cold]
#[inline(never)]
fn out_of_range(scale: f32) -> ! {
    panic!("px out of range: this glyph at scale {scale} does not fit i32")
}

#[inline(always)]
pub fn rasterize_inner(canvas: &mut Raster<'_>, glyph: &GlyphRef<'_>, scale: f32, stretch: f32) -> Metrics {
    rasterize_with(canvas, &glyph.info(), scale, stretch, |sink| glyph.draw(sink))
}

/// Rasterizes one glyph of `source`, monomorphized over the source. [`GlyphRef::from_source`] is
/// the dynamically dispatched form that `FontRepr` returns.
#[inline(always)]
pub fn rasterize_source<S: PathSource + ?Sized>(
    canvas: &mut Raster<'_>,
    source: &S,
    glyph: u16,
    scale: f32,
    stretch: f32,
) -> Metrics {
    // Taken before the glyph is measured and the raster sized, not inside the draw. Built after
    // those steps, the iterator's state cost the line walk two registers on Xtensa, reloaded on
    // every cell crossing.
    let points = source.points(glyph);
    rasterize_with(canvas, &source.info(glyph), scale, stretch, |sink| sink.path(points))
}

/// `FontRepr::rasterize_indexed` over a source, generically, for fonts the macro backs with a
/// store. `scale` is the font's `scale_factor(px)`.
#[doc(hidden)]
#[inline]
pub fn rasterize_source_indexed<'r, S: PathSource + ?Sized>(
    canvas: &'r mut Raster<'_>,
    source: &S,
    glyph: u16,
    px: f32,
    scale: f32,
    stretch: f32,
) -> (Metrics, crate::raster::BitmapIter<'r>) {
    if px == 0.0 {
        canvas.resize(0, 0);
        return (Metrics::default(), canvas.get_bitmap_iter());
    }
    let metrics = rasterize_source(canvas, source, glyph, scale, stretch);
    (metrics, canvas.get_bitmap_iter())
}

#[inline(always)]
fn rasterize_with(
    canvas: &mut Raster<'_>,
    info: &OutlineInfo,
    scale: f32,
    stretch: f32,
    draw: impl FnOnce(&mut Sink<'_, '_>),
) -> Metrics {
    let (metrics, offset_x, offset_y) = metrics_raw_stretched(scale, info, 0.0, stretch);
    // Ceiling, not truncation. `draw` scales x by `stretch`, so a fractional product still writes
    // into the column it lands inside, and a truncated width hands `add` a row shorter than it
    // fills. Only 1.0 and 3.0 reach this today and both are exact, so nothing here is load-bearing
    // for the shipped paths; it stops the general form from being wrong.
    // SAFETY: `metrics_raw_stretched` checked this product is in [0, MAX_DIMENSION].
    let raster_width = unsafe { ceil_i32_unchecked(metrics.width as f32 * stretch) } as usize;
    let lines = Lines::new(canvas, raster_width, metrics.height);
    let scale = scale * info.unit;
    draw(&mut Sink::new(lines, scale * stretch, scale, offset_x * stretch, offset_y));
    metrics
}

/// Glyph `index` of `face`, flattened for a px size of `scale`, as `Font` stores it.
pub(crate) fn outline_glyph(face: &Face, index: u16, scale: f32, units_per_em: f32) -> Glyph {
    let mut glyph = Glyph::default();
    let glyph_id = GlyphId(index);
    if let Some(advance_width) = face.glyph_hor_advance(glyph_id) {
        glyph.advance_width = advance_width as f32;
    }
    if let Some(advance_height) = face.glyph_ver_advance(glyph_id) {
        glyph.advance_height = advance_height as f32;
    }
    let mut geometry = Geometry::new(scale, units_per_em);
    face.outline_glyph(glyph_id, &mut geometry);
    geometry.finalize(&mut glyph);
    glyph
}

impl Font {
    /// Constructs a font from an array of bytes.
    pub fn from_bytes<Data: Deref<Target = [u8]>>(data: Data, settings: FontSettings) -> FontResult<Font> {
        let hash = crate::hash::hash(&data);

        let face = match Face::parse(&data, settings.collection_index) {
            Ok(f) => f,
            Err(e) => return Err(convert_error(e)),
        };
        let name = convert_name(&face);

        // Optionally get kerning values for the font. This should be a try block in the future.
        let horizontal_kern: Option<HashMap<u32, i16>> = (|| {
            let table: &[u8] = face.raw_face().table(Tag::from_bytes(&b"kern"))?;
            let table: TableKern = TableKern::new(table)?;
            Some(table.horizontal_mappings)
        })();

        // Collect all the unique codepoint to glyph mappings.
        let glyph_count = face.number_of_glyphs();
        let mut indices_to_load = HashSet::with_capacity(glyph_count as usize);
        let mut char_to_glyph = HashMap::with_capacity(glyph_count as usize);
        indices_to_load.insert(0u16);
        if let Some(subtable) = face.tables().cmap {
            for subtable in subtable.subtables {
                subtable.codepoints(|codepoint| {
                    if let Some(mapping) = subtable.glyph_index(codepoint) {
                        if let Some(mapping) = NonZeroU16::new(mapping.0) {
                            // Invalid indicies are ignored.
                            if let Some(c) = char::from_u32(codepoint) {
                                indices_to_load.insert(mapping.get());
                                char_to_glyph.insert(c, mapping);
                            }
                        }
                    }
                })
            }
        }

        // If the gsub table exists and the user needs it, add all of its glyphs to the glyphs we should load.
        if settings.load_substitutions {
            load_gsub(&face, &mut indices_to_load);
        }

        let units_per_em = face.units_per_em() as f32;

        // Parse and store all unique codepoints.
        let mut glyphs: Vec<Glyph> = vec::from_elem(Glyph::default(), glyph_count as usize);

        let generate_glyph = |index: u16| -> Result<Glyph, &'static str> {
            if index >= glyph_count {
                return Err("Attempted to map a codepoint out of bounds.");
            }
            Ok(outline_glyph(&face, index, settings.scale, units_per_em))
        };

        #[cfg(not(feature = "parallel"))]
        for index in indices_to_load {
            glyphs[index as usize] = generate_glyph(index)?;
        }

        #[cfg(feature = "parallel")]
        {
            let generated: Vec<(u16, Glyph)> = indices_to_load
                .into_par_iter()
                .map(|index| Ok((index, generate_glyph(index)?)))
                .collect::<Result<_, _>>()?;
            for (index, glyph) in generated {
                glyphs[index as usize] = glyph;
            }
        }

        // New line metrics.
        let horizontal_line_metrics =
            Some(LineMetrics::new(face.ascender(), face.descender(), face.line_gap()));
        let vertical_line_metrics = if let Some(ascender) = face.vertical_ascender() {
            Some(LineMetrics::new(
                ascender,
                face.vertical_descender().unwrap_or(0),
                face.vertical_line_gap().unwrap_or(0),
            ))
        } else {
            None
        };

        Ok(Font {
            name,
            glyphs,
            char_to_glyph,
            units_per_em,
            horizontal_line_metrics,
            horizontal_kern,
            vertical_line_metrics,
            settings,
            hash,
        })
    }

    /// Returns all valid unicode codepoints that have mappings to glyph geometry in the font, along
    /// with their associated index. This does not include grapheme cluster mappings. The mapped
    /// NonZeroU16 index can be used in the _indexed font functions.
    #[inline(always)]
    pub fn chars(&self) -> &HashMap<char, NonZeroU16> {
        &self.char_to_glyph
    }
}

impl FontRepr for Font {
    #[inline(always)]
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    #[inline(always)]
    fn file_hash(&self) -> usize {
        self.hash
    }

    #[inline(always)]
    fn horizontal_line_metrics_em(&self) -> Option<LineMetrics> {
        self.horizontal_line_metrics
    }

    #[inline(always)]
    fn vertical_line_metrics_em(&self) -> Option<LineMetrics> {
        self.vertical_line_metrics
    }

    #[inline(always)]
    fn horizontal_kern_indexed(&self, left: u16, right: u16, px: f32) -> Option<f32> {
        let scale = self.scale_factor(px);
        let map = self.horizontal_kern.as_ref()?;
        let key = u32::from(left) << 16 | u32::from(right);
        let value = map.get(&key)?;
        Some((*value as f32) * scale)
    }

    #[inline(always)]
    fn units_per_em(&self) -> f32 {
        self.units_per_em
    }

    #[inline]
    fn lookup_glyph_index(&self, character: char) -> u16 {
        self.char_to_glyph.get(&character).map_or(0, |index| index.get())
    }

    #[inline]
    fn glyph_count(&self) -> u16 {
        self.glyphs.len() as u16
    }

    #[inline(always)]
    fn get_glyph_at_index(&self, index: u16) -> GlyphRef<'_> {
        GlyphRef::from_glyph(&self.glyphs[index as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checked-once path against the guarded one, over values that pass both.
    #[test]
    fn metrics_fast_path_matches_guarded() {
        let values = [
            -0.0, 0.0, 0.25, 0.5, 1.0, 1.5, 2.0, 7.75, 12.0, 33.3, 1000.0, 4194303.5, -0.25, -1.0, -1.5,
            -7.75, -33.3, -4194303.5,
        ];
        let mut seed = 0x1234_5678u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            values[(seed >> 8) as usize % values.len()]
        };
        for _ in 0..20000 {
            let info = OutlineInfo {
                bounds: OutlineBounds {
                    xmin: next(),
                    ymin: next(),
                    width: abs(next()),
                    height: abs(next()),
                },
                unit: 1.0,
                advance_width: 0.0,
                advance_height: 0.0,
            };
            let offset = next() / 8.0;
            let stretch = if next() < 0.0 {
                3.0
            } else {
                1.0
            };
            let (metrics, ox, oy) = metrics_raw_stretched(1.0, &info, offset, stretch);
            let (dimensions, gx, gy) = outside(1.0, &info.bounds, offset, stretch);
            assert_eq!(
                [metrics.xmin, metrics.ymin, metrics.width as i32, metrics.height as i32],
                dimensions,
                "{info:?} offset {offset}"
            );
            // As floats: the two differ only in the sign of a zero offset.
            assert!(ox == gx && oy == gy, "{info:?} offset {offset}: {ox} {oy} against {gx} {gy}");
        }
    }
}
