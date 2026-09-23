//! Glyphs as the raster draws them: contours of points.
//!
//! The raster writes without bounds checks, and an upright glyph is drawn without clamping, so
//! every glyph carries a promise that every point lies inside its bounds. Glyphs `Font` outlines
//! keep it by construction. Points stored elsewhere, such as the macro's, promise it through
//! [`PathGlyph::new`], and sources that decode their own outlines promise it by implementing
//! [`OutlineSource`].

use crate::raster::Sink;
use crate::{Glyph, OutlineBounds};

/// The smallest non-zero coordinate a glyph's points may have, 2^-60. Placed at a scale of at
/// least 2^-20 with an offset of zero or at least 2^-100, every coordinate is zero or at least
/// 2^-100, so distinct ones differ by at least 2^-123, a normal float.
pub const SMALLEST_COORDINATE: f32 = f32::from_bits((127 - 60) << 23);

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

/// One step of a glyph's outline: a contour starts at `MoveTo`, and each `LineTo` draws a segment
/// from the point before it. A contour should end on the point it started from; the raster does
/// not close it.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum PathEvent {
    MoveTo([f32; 2]),
    LineTo([f32; 2]),
}

/// A glyph source that produces its own outlines, such as a decoder that streams them.
///
/// # Safety
///
/// For every glyph, every point `visit` passes must lie inside `[0, width] x [0, height]` of the
/// bounds `info` returns for that glyph, and `info` must return the same bounds on every call.
/// Every coordinate must be zero or at least [`SMALLEST_COORDINATE`].
///
/// The raster writes without bounds checks. A point outside the bounds writes out of bounds, and
/// so can a line walk whose reciprocals are wrong, which the second rule prevents: it keeps every
/// placed delta a normal float, and the reciprocal the raster uses at draw time on Xtensa is only
/// exact to 1 ulp for normal values.
pub unsafe trait OutlineSource {
    fn info(&self, glyph: u16) -> OutlineInfo;

    /// Passes the glyph's contours to `f`. Segments with no vertical extent belong in them too:
    /// the upright draw skips them, and a transformed draw needs them to close each contour.
    fn visit(&self, glyph: u16, f: &mut dyn FnMut(PathEvent));
}

/// An [`OutlineSource`] whose outline also comes out of an iterator, which is what the generic
/// [`crate::rasterize_source`] draws from. `GlyphRef` and dynamic dispatch use `visit`, since an
/// associated iterator type would have to be named in `dyn OutlineSource`.
///
/// # Safety
///
/// For every glyph, `points` must yield the outline `visit` passes, under the same rules.
pub unsafe trait PathSource: OutlineSource {
    type Points<'a>: Iterator<Item = PathEvent>
    where
        Self: 'a;

    fn points(&self, glyph: u16) -> Self::Points<'_>;
}

/// A glyph stored as contours of points, as `Font` and the macro keep them.
#[derive(Clone, Copy)]
pub struct PathGlyph<'p> {
    points: &'p [[f32; 2]],
    contours: &'p [u32],
    bounds: OutlineBounds,
    advance_width: f32,
    advance_height: f32,
}

impl<'p> PathGlyph<'p> {
    /// `contours` holds the end index in `points` of each contour, in order.
    ///
    /// # Safety
    ///
    /// Every point must lie inside `[0, width] x [0, height]` of `bounds`, and every coordinate
    /// must be zero or at least [`SMALLEST_COORDINATE`]. The raster writes without bounds checks.
    pub const unsafe fn new(
        points: &'p [[f32; 2]],
        contours: &'p [u32],
        bounds: OutlineBounds,
        advance_width: f32,
        advance_height: f32,
    ) -> Self {
        PathGlyph {
            points,
            contours,
            bounds,
            advance_width,
            advance_height,
        }
    }

    #[inline(always)]
    pub fn from_glyph(glyph: &'p Glyph) -> Self {
        // SAFETY: `Font` outlines every `Glyph`, and `Geometry::finalize` places its points inside
        // the bounds it records and flushes coordinates below `SMALLEST_COORDINATE` to zero.
        unsafe {
            Self::new(&glyph.points, &glyph.contours, glyph.bounds, glyph.advance_width, glyph.advance_height)
        }
    }

    pub fn points(&self) -> &'p [[f32; 2]] {
        self.points
    }

    pub fn contours(&self) -> &'p [u32] {
        self.contours
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

/// Each contour of `points`, split at the end indices in `contours`. An end past the points or
/// before the previous one ends the walk.
#[inline(always)]
pub(crate) fn each_contour<'p>(
    points: &'p [[f32; 2]],
    contours: &'p [u32],
    mut f: impl FnMut(&'p [f32; 2], &'p [[f32; 2]]),
) {
    let mut start = 0;
    for &end in contours {
        let Some(contour) = points.get(start..end as usize) else {
            return;
        };
        start = end as usize;
        if let Some((first, rest)) = contour.split_first() {
            f(first, rest);
        }
    }
}

/// The events of stored contours, as a [`PathSource`] yields them. An end past the points or
/// before the previous one ends the walk, as in [`each_contour`].
#[derive(Clone)]
pub(crate) struct PathEvents<'p> {
    /// The points after the current contour.
    points: &'p [[f32; 2]],
    contours: core::slice::Iter<'p, u32>,
    /// The index in the glyph's points of the first of `points`.
    start: usize,
    /// The current contour's points not yet yielded.
    rest: core::slice::Iter<'p, [f32; 2]>,
}

impl<'p> PathEvents<'p> {
    #[inline(always)]
    pub(crate) fn new(points: &'p [[f32; 2]], contours: &'p [u32]) -> Self {
        PathEvents {
            points,
            contours: contours.iter(),
            start: 0,
            rest: [].iter(),
        }
    }
}

impl Iterator for PathEvents<'_> {
    type Item = PathEvent;

    #[inline(always)]
    fn next(&mut self) -> Option<PathEvent> {
        if let Some(&point) = self.rest.next() {
            return Some(PathEvent::LineTo(point));
        }
        loop {
            let end = *self.contours.next()? as usize;
            let Some((contour, after)) =
                end.checked_sub(self.start).and_then(|len| self.points.split_at_checked(len))
            else {
                self.contours = [].iter();
                return None;
            };
            self.points = after;
            self.start = end;
            if let Some((&first, rest)) = contour.split_first() {
                self.rest = rest.iter();
                return Some(PathEvent::MoveTo(first));
            }
        }
    }
}

/// A glyph ready to measure or draw: stored points, or one glyph of an [`OutlineSource`].
#[derive(Clone, Copy)]
pub struct GlyphRef<'a> {
    info: OutlineInfo,
    outline: Outline<'a>,
}

#[derive(Clone, Copy)]
enum Outline<'a> {
    Path(&'a [[f32; 2]], &'a [u32]),
    Source(&'a dyn OutlineSource, u16),
}

impl<'a> GlyphRef<'a> {
    #[inline(always)]
    pub fn from_glyph(glyph: &'a Glyph) -> Self {
        PathGlyph::from_glyph(glyph).into()
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

    /// The stored contours, when the glyph is not a source's.
    #[inline(always)]
    pub(crate) fn path(&self) -> Option<PathEvents<'a>> {
        match self.outline {
            Outline::Path(points, contours) => Some(PathEvents::new(points, contours)),
            Outline::Source(..) => None,
        }
    }

    #[inline(always)]
    pub(crate) fn visit(&self, mut f: impl FnMut(PathEvent)) {
        match self.outline {
            Outline::Path(points, contours) => each_contour(points, contours, |&first, rest| {
                f(PathEvent::MoveTo(first));
                for &p in rest {
                    f(PathEvent::LineTo(p));
                }
            }),
            Outline::Source(source, glyph) => source.visit(glyph, &mut f),
        }
    }

    #[inline(always)]
    pub(crate) fn draw(&self, sink: &mut Sink<'_, '_>) {
        match self.outline {
            Outline::Path(points, contours) => sink.contours(points, contours),
            Outline::Source(source, glyph) => sink.source(source, glyph),
        }
    }
}

impl<'p> From<PathGlyph<'p>> for GlyphRef<'p> {
    #[inline(always)]
    fn from(glyph: PathGlyph<'p>) -> Self {
        GlyphRef {
            info: glyph.info(),
            outline: Outline::Path(glyph.points, glyph.contours),
        }
    }
}
