//! Glyphs as the raster draws them.
//!
//! The raster writes without bounds checks, so everything it draws carries a promise that every
//! point lies inside the glyph's bounds. Glyphs `Font` outlines keep it by construction. Lines
//! built elsewhere, such as the macro's, promise it through [`LineGlyph::new`], and sources that
//! decode their own outlines promise it by implementing [`OutlineSource`].

use crate::math::Line;
use crate::raster::Sink;
use crate::{Glyph, OutlineBounds};

/// What the raster needs to know about a glyph before drawing it.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct OutlineInfo {
    /// Bounds of the glyph's points, in units of `unit` font units. Points are relative to the
    /// bounds' corner, with y growing down, and span `[0, width] x [0, height]`.
    pub bounds: OutlineBounds,
    /// Font units per point unit: 1 for points in font units.
    pub unit: f32,
    /// Advance width in font units.
    pub advance_width: f32,
    /// Advance height in font units.
    pub advance_height: f32,
}

/// A glyph source that draws its own outlines, such as a decoder that streams them.
///
/// # Safety
///
/// For every glyph, each segment `draw` passes to the sink must have both endpoints inside
/// `[0, width] x [0, height]` of the bounds `info` returns for that glyph, and `info` must return
/// the same bounds on every call. The raster writes without bounds checks, so a point outside
/// them writes out of bounds.
pub unsafe trait OutlineSource {
    fn info(&self, glyph: u16) -> OutlineInfo;

    /// Passes each of the glyph's segments to `sink`, in any order.
    fn draw(&self, glyph: u16, sink: &mut Sink<'_, '_>);
}

/// A glyph stored as lines, split into vertical lines and all others, as `Font` and the macro
/// keep them.
#[derive(Clone, Copy)]
pub struct LineGlyph<'l> {
    v_lines: &'l [Line],
    m_lines: &'l [Line],
    bounds: OutlineBounds,
    advance_width: f32,
    advance_height: f32,
}

impl<'l> LineGlyph<'l> {
    /// # Safety
    ///
    /// Every line's endpoints must lie inside `[0, width] x [0, height]` of `bounds`, and every
    /// line in `v_lines` must be vertical. The raster writes without bounds checks.
    pub const unsafe fn new(
        v_lines: &'l [Line],
        m_lines: &'l [Line],
        bounds: OutlineBounds,
        advance_width: f32,
        advance_height: f32,
    ) -> Self {
        LineGlyph {
            v_lines,
            m_lines,
            bounds,
            advance_width,
            advance_height,
        }
    }

    #[inline(always)]
    pub fn from_glyph(glyph: &'l Glyph) -> Self {
        // SAFETY: `Font` outlines every `Glyph`, and `Geometry::finalize` positions its lines
        // inside the bounds it records and puts only vertical lines in `v_lines`.
        unsafe {
            Self::new(&glyph.v_lines, &glyph.m_lines, glyph.bounds, glyph.advance_width, glyph.advance_height)
        }
    }

    pub fn v_lines(&self) -> &'l [Line] {
        self.v_lines
    }

    pub fn m_lines(&self) -> &'l [Line] {
        self.m_lines
    }

    #[inline(always)]
    pub fn info(&self) -> OutlineInfo {
        OutlineInfo {
            bounds: self.bounds,
            unit: 1.0,
            advance_width: self.advance_width,
            advance_height: self.advance_height,
        }
    }
}

/// A glyph ready to measure or draw: stored lines, or one glyph of an [`OutlineSource`].
#[derive(Clone, Copy)]
pub struct GlyphRef<'a> {
    info: OutlineInfo,
    outline: Outline<'a>,
}

#[derive(Clone, Copy)]
enum Outline<'a> {
    Lines(&'a [Line], &'a [Line]),
    Source(&'a dyn OutlineSource, u16),
}

impl<'a> GlyphRef<'a> {
    #[inline(always)]
    pub fn from_glyph(glyph: &'a Glyph) -> Self {
        LineGlyph::from_glyph(glyph).into()
    }

    /// One glyph of a source, drawn through dynamic dispatch. [`crate::rasterize_source`] is the
    /// generic form.
    pub fn from_source(source: &'a dyn OutlineSource, glyph: u16) -> Self {
        GlyphRef {
            info: source.info(glyph),
            outline: Outline::Source(source, glyph),
        }
    }

    #[inline(always)]
    pub fn info(&self) -> OutlineInfo {
        self.info
    }

    #[inline(always)]
    pub(crate) fn draw(&self, sink: &mut Sink<'_, '_>) {
        match self.outline {
            Outline::Lines(v_lines, m_lines) => sink.lines(v_lines, m_lines),
            Outline::Source(source, glyph) => source.draw(glyph, sink),
        }
    }
}

impl<'l> From<LineGlyph<'l>> for GlyphRef<'l> {
    #[inline(always)]
    fn from(glyph: LineGlyph<'l>) -> Self {
        GlyphRef {
            info: glyph.info(),
            outline: Outline::Lines(glyph.v_lines, glyph.m_lines),
        }
    }
}
