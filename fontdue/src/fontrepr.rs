use crate::{
    GlyphRef, LineMetrics, Metrics,
    layout::GlyphRasterConfig,
    raster::{BitmapIter, Raster},
};

pub trait FontRepr {
    // fn units_per_em(&self) -> f32;
    // fn glyphs(&self) -> &[Glyph];
    /// Returns the font's face name if it has one. It is from `Name ID 4` (Full Name) in the name table.
    /// See https://learn.microsoft.com/en-us/typography/opentype/spec/name#name-ids for more info.
    fn name(&self) -> Option<&str>;

    /// Returns a precomputed hash for the font file.
    fn file_hash(&self) -> usize;

    /// New line metrics for fonts that append characters to lines horizontally, and append new
    /// lines vertically (above or below the current line). Only populated for fonts with the
    /// appropriate metrics, none if it's missing.
    fn horizontal_line_metrics_em(&self) -> Option<LineMetrics>;

    /// New line metrics for fonts that append characters to lines horizontally, and append new
    /// lines vertically (above or below the current line). Only populated for fonts with the
    /// appropriate metrics, none if it's missing.
    /// # Arguments
    ///
    /// * `px` - The size to scale the line metrics by. The units of the scale are pixels per Em
    /// unit.
    #[inline]
    fn horizontal_line_metrics(&self, px: f32) -> Option<LineMetrics> {
        let metrics = self.horizontal_line_metrics_em()?;
        Some(metrics.scale(self.scale_factor(px)))
    }

    /// New line metrics for fonts that append characters to lines vertically, and append new
    /// lines horizontally (left or right of the current line). Only populated for fonts with the
    /// appropriate metrics, none if it's missing.
    fn vertical_line_metrics_em(&self) -> Option<LineMetrics>;

    /// New line metrics for fonts that append characters to lines vertically, and append new
    /// lines horizontally (left or right of the current line). Only populated for fonts with the
    /// appropriate metrics, none if it's missing.
    /// # Arguments
    ///
    /// * `px` - The size to scale the line metrics by. The units of the scale are pixels per Em
    /// unit.
    #[inline]
    fn vertical_line_metrics(&self, px: f32) -> Option<LineMetrics> {
        let metrics = self.vertical_line_metrics_em()?;
        Some(metrics.scale(self.scale_factor(px)))
    }

    /// Gets the font's units per em.
    fn units_per_em(&self) -> f32;

    /// Calculates the glyph's outline scale factor for a given px size. The units of the scale are
    /// pixels per Em unit.
    #[inline(always)]
    fn scale_factor(&self, px: f32) -> f32 {
        px / self.units_per_em()
    }

    /// Retrieves the horizontal scaled kerning value for two adjacent characters.
    /// # Arguments
    ///
    /// * `left` - The character on the left hand side of the pairing.
    /// * `right` - The character on the right hand side of the pairing.
    /// * `px` - The size to scale the kerning value for. The units of the scale are pixels per Em
    /// unit.
    /// # Returns
    ///
    /// * `Option<f32>` - The horizontal scaled kerning value if one is present in the font for the
    /// given left and right pair, None otherwise.
    #[inline(always)]
    fn horizontal_kern(&self, left: char, right: char, px: f32) -> Option<f32> {
        self.horizontal_kern_indexed(self.lookup_glyph_index(left), self.lookup_glyph_index(right), px)
    }

    /// Retrieves the horizontal scaled kerning value for two adjacent glyph indicies.
    /// # Arguments
    ///
    /// * `left` - The glyph index on the left hand side of the pairing.
    /// * `right` - The glyph index on the right hand side of the pairing.
    /// * `px` - The size to scale the kerning value for. The units of the scale are pixels per Em
    /// unit.
    /// # Returns
    ///
    /// * `Option<f32>` - The horizontal scaled kerning value if one is present in the font for the
    /// given left and right pair, None otherwise.
    fn horizontal_kern_indexed(&self, left: u16, right: u16, px: f32) -> Option<f32>;

    /// Retrieves the layout metrics for the given character. If the character isn't present in the
    /// font, then the layout for the font's default character is returned instead.
    /// # Arguments
    ///
    /// * `index` - The character in the font to to generate the layout metrics for.
    /// * `px` - The size to generate the layout metrics for the character at. Cannot be negative.
    /// The units of the scale are pixels per Em unit.
    /// # Returns
    ///
    /// * `Metrics` - Sizing and positioning metadata for the glyph.
    #[inline(always)]
    fn metrics(&self, character: char, px: f32) -> Metrics {
        self.metrics_indexed(self.lookup_glyph_index(character), px)
    }

    /// Retrieves the layout metrics at the given index. You normally want to be using
    /// metrics(char, f32) instead, unless your glyphs are pre-indexed.
    /// # Arguments
    ///
    /// * `index` - The glyph index in the font to to generate the layout metrics for.
    /// * `px` - The size to generate the layout metrics for the glyph at. Cannot be negative. The
    /// units of the scale are pixels per Em unit.
    /// # Returns
    ///
    /// * `Metrics` - Sizing and positioning metadata for the glyph.
    fn metrics_indexed(&self, index: u16, px: f32) -> Metrics {
        let glyph = &self.get_glyph_at_index(index);
        let scale = self.scale_factor(px);
        let (metrics, _, _) = crate::font::metrics_raw(scale, glyph, 0.0);
        metrics
    }

    /// Retrieves the layout rasterized bitmap for the given raster config. If the raster config's
    /// character isn't present in the font, then the layout and bitmap for the font's default
    /// character's raster is returned instead.
    /// # Arguments
    ///
    /// * `config` - The settings to render the character at.
    /// # Returns
    ///
    /// * `Metrics` - Sizing and positioning metadata for the rasterized glyph.
    /// * `Vec<u8>` - Coverage vector for the glyph. Coverage is a linear scale where 0 represents
    /// 0% coverage of that pixel by the glyph and 255 represents 100% coverage. The vec starts at
    /// the top left corner of the glyph.
    #[inline]
    fn rasterize_config<'r>(
        &self,
        raster: &'r mut Raster,
        config: GlyphRasterConfig,
    ) -> (Metrics, BitmapIter<'r>) {
        self.rasterize_indexed(raster, config.glyph_index, config.px)
    }

    /// Retrieves the layout metrics and rasterized bitmap for the given character. If the
    /// character isn't present in the font, then the layout and bitmap for the font's default
    /// character is returned instead.
    /// # Arguments
    ///
    /// * `character` - The character to rasterize.
    /// * `px` - The size to render the character at. Cannot be negative. The units of the scale
    /// are pixels per Em unit.
    /// # Returns
    ///
    /// * `Metrics` - Sizing and positioning metadata for the rasterized glyph.
    /// * `Vec<u8>` - Coverage vector for the glyph. Coverage is a linear scale where 0 represents
    /// 0% coverage of that pixel by the glyph and 255 represents 100% coverage. The vec starts at
    /// the top left corner of the glyph.
    #[inline]
    fn rasterize<'r>(&self, canvas: &'r mut Raster, character: char, px: f32) -> (Metrics, BitmapIter<'r>) {
        self.rasterize_indexed(canvas, self.lookup_glyph_index(character), px)
    }

    /// Retrieves the layout rasterized bitmap for the given raster config. If the raster config's
    /// character isn't present in the font, then the layout and bitmap for the font's default
    /// character's raster is returned instead.
    ///
    /// This will perform the operation with the width multiplied by 3, as to simulate subpixels.
    /// Taking these as RGB values will perform subpixel anti aliasing.
    /// # Arguments
    ///
    /// * `config` - The settings to render the character at.
    /// # Returns
    ///
    /// * `Metrics` - Sizing and positioning metadata for the rasterized glyph.
    /// * `Vec<u8>` - Swizzled RGB coverage vector for the glyph. Coverage is a linear scale where 0
    /// represents 0% coverage of that subpixel by the glyph and 255 represents 100% coverage. The
    /// vec starts at the top left corner of the glyph.
    #[inline]
    fn rasterize_config_subpixel<'r>(
        &self,
        canvas: &'r mut Raster,
        config: GlyphRasterConfig,
    ) -> (Metrics, BitmapIter<'r>) {
        self.rasterize_indexed_subpixel(canvas, config.glyph_index, config.px)
    }

    /// Retrieves the layout metrics and rasterized bitmap for the given character. If the
    /// character isn't present in the font, then the layout and bitmap for the font's default
    /// character is returned instead.
    ///
    /// This will perform the operation with the width multiplied by 3, as to simulate subpixels.
    /// Taking these as RGB values will perform subpixel anti aliasing.
    /// # Arguments
    ///
    /// * `character` - The character to rasterize.
    /// * `px` - The size to render the character at. Cannot be negative. The units of the scale
    /// are pixels per Em unit.
    /// # Returns
    ///
    /// * `Metrics` - Sizing and positioning metadata for the rasterized glyph.
    /// * `Vec<u8>` - Swizzled RGB coverage vector for the glyph. Coverage is a linear scale where 0
    /// represents 0% coverage of that subpixel by the glyph and 255 represents 100% coverage. The
    /// vec starts at the top left corner of the glyph.
    #[inline]
    fn rasterize_subpixel<'r>(
        &self,
        canvas: &'r mut Raster,
        character: char,
        px: f32,
    ) -> (Metrics, BitmapIter<'r>) {
        self.rasterize_indexed_subpixel(canvas, self.lookup_glyph_index(character), px)
    }

    /// Retrieves the layout metrics and rasterized bitmap at the given index. You normally want to
    /// be using rasterize(char, f32) instead, unless your glyphs are pre-indexed.
    /// # Arguments
    ///
    /// * `index` - The glyph index in the font to rasterize.
    /// * `px` - The size to render the character at. Cannot be negative. The units of the scale
    /// are pixels per Em unit.
    /// # Returns
    ///
    /// * `Metrics` - Sizing and positioning metadata for the rasterized glyph.
    /// * `Vec<u8>` - Coverage vector for the glyph. Coverage is a linear scale where 0 represents
    /// 0% coverage of that pixel by the glyph and 255 represents 100% coverage. The vec starts at
    /// the top left corner of the glyph.
    fn rasterize_indexed<'r>(
        &self,
        canvas: &'r mut Raster,
        index: u16,
        px: f32,
    ) -> (Metrics, BitmapIter<'r>) {
        if px <= 0.0 {
            canvas.resize(0, 0);
            return (Metrics::default(), canvas.get_bitmap_iter());
        }
        let glyph = &self.get_glyph_at_index(index);
        let scale = self.scale_factor(px);
        let metrics = crate::rasterize_inner(canvas, glyph, scale, 1.0);
        (metrics, canvas.get_bitmap_iter())
    }

    /// Retrieves the layout metrics and rasterized bitmap at the given index. You normally want to
    /// be using rasterize(char, f32) instead, unless your glyphs are pre-indexed.
    ///
    /// This will perform the operation with the width multiplied by 3, as to simulate subpixels.
    /// Taking these as RGB values will perform subpixel anti aliasing.
    /// # Arguments
    ///
    /// * `index` - The glyph index in the font to rasterize.
    /// * `px` - The size to render the character at. Cannot be negative. The units of the scale
    /// are pixels per Em unit.
    /// # Returns
    ///
    /// * `Metrics` - Sizing and positioning metadata for the rasterized glyph.
    /// * `Vec<u8>` - Swizzled RGB coverage vector for the glyph. Coverage is a linear scale where 0
    /// represents 0% coverage of that subpixel by the glyph and 255 represents 100% coverage. The
    /// vec starts at the top left corner of the glyph.
    fn rasterize_indexed_subpixel<'r>(
        &self,
        canvas: &'r mut Raster,
        index: u16,
        px: f32,
    ) -> (Metrics, BitmapIter<'r>) {
        if px <= 0.0 {
            canvas.resize(0, 0);
            return (Metrics::default(), canvas.get_bitmap_iter());
        }
        let glyph = &self.get_glyph_at_index(index);
        let scale = self.scale_factor(px);
        let metrics = crate::font::rasterize_inner(canvas, glyph, scale, 3.0);
        (metrics, canvas.get_bitmap_iter())
    }

    /// Checks if the font has a glyph for the given character.
    #[inline(always)]
    fn has_glyph(&self, character: char) -> bool {
        self.lookup_glyph_index(character) != 0
    }

    /// Finds the internal glyph index for the given character. If the character is not present in
    /// the font then 0 is returned.
    fn lookup_glyph_index(&self, character: char) -> u16;

    /// Gets the total glyphs in the font.
    fn glyph_count(&self) -> u16;

    fn get_glyph_at_index(&self, index: u16) -> GlyphRef<'_>;
}
