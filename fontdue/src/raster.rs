//! The raster and its line walk. The walk writes without bounds checks: `Lines::line` is the one
//! way in, and its contract is what keeps the writes inside the buffer. Everything else in the
//! crate reaches it through sized rasters and placed or clamped points;
//! [`rasterize_path`](crate::rasterize_path) is the safe way to fill a path.

use crate::math::Point;

/// Whether a scale keeps every coordinate the sources may have, zero or at least 2^-60, at zero or
/// at least 2^-80.
fn safe_scale(scale: f32) -> bool {
    scale >= f32::from_bits((127 - 20) << 23)
}

/// Whether an offset is zero or at least 2^-100, so a placed coordinate is too.
fn safe_offset(offset: f32) -> bool {
    offset == 0.0 || offset >= f32::from_bits((127 - 100) << 23)
}
use crate::platform::{abs, as_i32_unchecked, f32x4, floor, half_of, halve, mul_add, recip2};
use alloc::vec::*;
use core::iter::FusedIterator;
use core::marker::PhantomData;

enum RasterBuffer<'a> {
    Owned(Vec<f32>),
    Borrowed(&'a mut [f32]),
}

pub struct Raster<'a> {
    w: usize,
    h: usize,
    a: RasterBuffer<'a>,
}

impl Raster<'static> {
    #[inline]
    pub fn empty() -> Self {
        Raster::new(0, 0)
    }

    #[inline]
    pub fn new(w: usize, h: usize) -> Raster<'static> {
        Raster::from_buf(Vec::new(), w, h)
    }

    #[inline]
    pub fn from_buf(buf: Vec<f32>, w: usize, h: usize) -> Self {
        let mut r = Raster {
            w,
            h,
            a: RasterBuffer::Owned(buf),
        };
        r.resize(w, h);
        r
    }
}

impl<'a> Raster<'a> {
    /// Creates a raster backed by caller-owned storage.
    ///
    /// `buf` must hold `w * h + 3` floats. It must also fit every later size the rasterizer
    /// resizes to, which for a font means the largest glyph at the largest px the caller will
    /// ask for, not the size passed here. A later size that does not fit panics, so a caller
    /// with a fixed buffer should size it from the font's bounding box up front.
    #[inline]
    pub fn from_slice(buf: &'a mut [f32], w: usize, h: usize) -> Option<Self> {
        if buf.len() < w.checked_mul(h)?.checked_add(3)? {
            return None;
        }
        let mut r = Raster {
            w,
            h,
            a: RasterBuffer::Borrowed(buf),
        };
        r.resize(w, h);
        Some(r)
    }

    pub fn width(&self) -> usize {
        self.w
    }

    pub fn height(&self) -> usize {
        self.h
    }

    /// Resizes to `w` by `h` pixels, all empty.
    ///
    /// # Panics
    ///
    /// If `w * h + 3` exceeds `i32::MAX`, or the raster borrows a slice too short for it.
    pub fn resize(&mut self, w: usize, h: usize) {
        // The line walk writes without bounds checks off `w` and `h`, computing cell indices in
        // `i32`. A wrapped length would leave the buffer shorter than the dimensions it trusts,
        // and a wrapped index would write outside it.
        let len = w
            .checked_mul(h)
            .and_then(|area| area.checked_add(3))
            .filter(|&len| len <= i32::MAX as usize)
            .expect("raster dimensions overflow i32");
        self.w = w;
        self.h = h;
        match &mut self.a {
            RasterBuffer::Owned(a) => {
                // Clear first, so the refill writes `len` elements and no more. Zeroing before
                // the resize covers whatever the largest glyph so far grew the buffer to, which
                // is work every smaller glyph after it pays for nothing.
                a.clear();
                a.reserve(len);
                // SAFETY: `reserve` made room for `len` elements, `write_bytes` initialises all of
                // them, and all-zero bits are `0.0`. Unlike `resize`, this is a `memset` at every
                // opt-level; at `opt-level = "s"` `resize` is a store loop.
                unsafe {
                    core::ptr::write_bytes(a.as_mut_ptr(), 0, len);
                    a.set_len(len);
                }
            }
            RasterBuffer::Borrowed(a) => {
                a[..len].fill(0.0);
            }
        }
    }

    /// Coverage by the nonzero rule, which font outlines use.
    #[inline(always)]
    pub fn get_bitmap_iter<'r>(&'r self) -> BitmapIter<'r> {
        self.get_bitmap_iter_with()
    }

    /// Coverage by the even-odd rule: where contours overlap, each overlap toggles coverage.
    #[inline(always)]
    pub fn get_bitmap_iter_even_odd<'r>(&'r self) -> BitmapIter<'r, EvenOdd> {
        self.get_bitmap_iter_with()
    }

    #[inline(always)]
    fn get_bitmap_iter_with<'r, R: FillRule>(&'r self) -> BitmapIter<'r, R> {
        BitmapIter {
            a: match &self.a {
                RasterBuffer::Owned(a) => a,
                RasterBuffer::Borrowed(a) => a,
            },
            pos: 0,
            remaining: self.w * self.h,
            height: 0.0,
            block: 0,
            block_left: 0,
            _rule: PhantomData,
        }
    }
}

/// Float-to-int for the line loops. Every value converted here is a pixel coordinate inside the
/// raster, by `Lines::line`'s contract, so the unchecked convert has its precondition.
#[inline(always)]
fn index_of(value: f32) -> i32 {
    // SAFETY: as above.
    unsafe { as_i32_unchecked(value) }
}

/// A raster being filled: fontdue's rasterizer core, taking lines in pixels with no checks. Every
/// glyph and path is drawn through one.
///
/// Draw every segment of a closed outline with [`line`](Lines::line), then drop the `Lines` and
/// read coverage from [`Raster::get_bitmap_iter`]. Pixels are filled by the nonzero rule, with
/// partial coverage at the edges.
pub struct Lines<'s, 'b> {
    cells: Cells,
    _raster: PhantomData<&'s mut Raster<'b>>,
}

impl<'s, 'b> Lines<'s, 'b> {
    /// Resizes `raster` to `width` by `height` pixels, all empty.
    ///
    /// # Panics
    ///
    /// If `width * height + 3` exceeds `i32::MAX`, or the raster borrows a slice too short for it.
    #[inline(always)]
    pub fn new(raster: &'s mut Raster<'b>, width: usize, height: usize) -> Self {
        raster.resize(width, height);
        Lines::of(raster)
    }

    /// Lines into `raster` at the size it has.
    #[inline(always)]
    pub(crate) fn of(raster: &'s mut Raster<'b>) -> Self {
        let w = raster.w as i32;
        #[cfg(debug_assertions)]
        let len = raster.w * raster.h + 3;
        let ptr = match &mut raster.a {
            RasterBuffer::Owned(a) => a.as_mut_ptr(),
            RasterBuffer::Borrowed(a) => a.as_mut_ptr(),
        };
        Lines {
            cells: Cells {
                ptr,
                w,
                #[cfg(debug_assertions)]
                len,
            },
            _raster: PhantomData,
        }
    }

    /// Adds the segment from `from` to `to`, in pixels with y down.
    ///
    /// # Safety
    ///
    /// Both points must lie inside `[0, width] x [0, height]`, and in each axis the two coordinates
    /// must be equal or differ by at least `f32::MIN_POSITIVE`. Every coordinate being zero or at
    /// least 2^-100 is enough for the second. The walk writes without bounds checks, and the
    /// reciprocals that steer it are only accurate for normal deltas.
    #[inline(always)]
    pub unsafe fn line(&mut self, from: [f32; 2], to: [f32; 2]) {
        self.cells.edge(Point::new(from[0], from[1]), Point::new(to[0], to[1]));
    }

    /// [`line`](Lines::line) for points already in the walk's type.
    #[inline(always)]
    pub(crate) unsafe fn edge(&mut self, from: Point, to: Point) {
        self.cells.edge(from, to);
    }
}

/// A glyph's points on their way to the raster: scaled and offset into it, with the previous
/// point kept for the next segment.
pub(crate) struct Sink<'s, 'b> {
    cells: Cells,
    place: Place,
    /// Where the next segment starts, placed.
    last: Point,
    /// Whether segments need `placed`'s guards: set when the scale or an offset is too small for
    /// the sources' coordinate rule to keep placed deltas normal.
    guard: bool,
    _raster: PhantomData<&'s mut Raster<'b>>,
}

/// The raster's cells, taken out of it once so nothing in the line walk can alias them. The
/// buffer holds `w * h + 3` cells, `resize` having sized it for the glyph. The draw loops copy it
/// into a local, so the compiler can see that writes to the cells leave it alone.
#[derive(Clone, Copy)]
struct Cells {
    ptr: *mut f32,
    w: i32,
    #[cfg(debug_assertions)]
    len: usize,
}

/// Source units to raster space. Scalars, not the `f32x4`s the walk takes: read back from memory
/// in a `dyn` source's `visit` closure, duplicated lanes are separate loads and separate live
/// registers.
#[derive(Clone, Copy)]
struct Place {
    scale_x: f32,
    scale_y: f32,
    offset_x: f32,
    offset_y: f32,
}

impl Place {
    #[inline(always)]
    fn apply(self, [x, y]: [f32; 2]) -> Point {
        Point::new(x * self.scale_x + self.offset_x, y * self.scale_y + self.offset_y)
    }
}

impl<'s, 'b> Sink<'s, 'b> {
    /// Sources' points go to `lines` scaled and then offset. The raster must be sized so that
    /// every point the glyph's source promises lands inside it.
    #[inline(always)]
    pub(crate) fn new(
        lines: Lines<'s, 'b>,
        scale_x: f32,
        scale_y: f32,
        offset_x: f32,
        offset_y: f32,
    ) -> Self {
        Sink {
            cells: lines.cells,
            place: Place {
                scale_x,
                scale_y,
                offset_x,
                offset_y,
            },
            last: Point::new(offset_x, offset_y),
            guard: !(safe_scale(scale_x)
                && safe_scale(scale_y)
                && safe_offset(offset_x)
                && safe_offset(offset_y)),
            _raster: PhantomData,
        }
    }

    /// Draws stored contours, holding the previous point in registers rather than in the sink.
    #[inline(always)]
    pub(crate) fn contours(&mut self, points: &[[f32; 2]], contours: &[u32]) {
        // Decided once, not per segment: with both edges in one loop body the crossing loop ran a
        // register short.
        if self.guard {
            self.contours_with::<true>(points, contours)
        } else {
            self.contours_with::<false>(points, contours)
        }
    }

    #[inline(always)]
    fn contours_with<const GUARD: bool>(&mut self, points: &[[f32; 2]], contours: &[u32]) {
        let (cells, place) = (self.cells, self.place);
        crate::outline::each_contour(points, contours, |&first, rest| {
            let mut last = place.apply(first);
            for &point in rest {
                let placed = place.apply(point);
                cells.edge_with::<GUARD>(last, placed);
                last = placed;
            }
        });
    }

    /// Draws a `dyn` source's outline through its `visit`, one call per point.
    #[inline(always)]
    pub(crate) fn source(&mut self, source: &dyn crate::OutlineSource, glyph: u16) {
        if self.guard {
            self.source_with::<true>(source, glyph)
        } else {
            self.source_with::<false>(source, glyph)
        }
    }

    #[inline(always)]
    fn source_with<const GUARD: bool>(&mut self, source: &dyn crate::OutlineSource, glyph: u16) {
        let (cells, place) = (self.cells, self.place);
        let mut last = self.last;
        source.visit(glyph, &mut move |event| match event {
            crate::PathEvent::MoveTo(p) => last = place.apply(p),
            crate::PathEvent::LineTo(p) => {
                let placed = place.apply(p);
                cells.edge_with::<GUARD>(last, placed);
                last = placed;
            }
        });
    }

    /// Draws a source's path events, as `contours` does stored points.
    #[inline(always)]
    pub(crate) fn path(&mut self, events: impl Iterator<Item = crate::PathEvent>) {
        if self.guard {
            self.path_with::<true>(events)
        } else {
            self.path_with::<false>(events)
        }
    }

    #[inline(always)]
    fn path_with<const GUARD: bool>(&mut self, events: impl Iterator<Item = crate::PathEvent>) {
        let (cells, place) = (self.cells, self.place);
        let mut last = self.last;
        for event in events {
            match event {
                crate::PathEvent::MoveTo(p) => last = place.apply(p),
                crate::PathEvent::LineTo(p) => {
                    let placed = place.apply(p);
                    cells.edge_with::<GUARD>(last, placed);
                    last = placed;
                }
            }
        }
        self.last = last;
    }
}

impl Cells {
    #[inline(always)]
    fn edge_with<const GUARD: bool>(self, start: Point, end: Point) {
        if GUARD {
            self.placed(start, end);
        } else {
            self.edge(start, end);
        }
    }

    /// A placed segment from a source, skipping it when it has no vertical extent. Only for a
    /// sink without `guard`: then the sources' coordinate rule and the scale keep both deltas zero
    /// or normal.
    #[inline(always)]
    fn edge(self, start: Point, end: Point) {
        if start.y == end.y {
            return;
        }
        self.segment(start, end, end.x - start.x, end.y - start.y, start.x == end.x);
    }

    /// Draws a segment whose points are in raster space and inside it, skipping it when it has
    /// no vertical extent. Deltas too small to be normal count as zero, since the reciprocal is
    /// only accurate for normal values; a tiny scale can make them so from normal source deltas.
    #[inline(always)]
    fn placed(self, start: Point, end: Point) {
        let dy = end.y - start.y;
        if !(abs(dy) >= f32::MIN_POSITIVE) {
            return;
        }
        let dx = end.x - start.x;
        let vertical = !(abs(dx) >= f32::MIN_POSITIVE);
        let end_x = if vertical {
            start.x
        } else {
            end.x
        };
        self.segment(start, Point::new(end_x, end.y), dx, dy, vertical);
    }

    /// Walks the segment from `start` to `end`, whose deltas are `dx` and `dy`, `dy` normal. A
    /// `vertical` segment shares x at both ends; any other has `dx` normal.
    #[inline(always)]
    fn segment(self, start: Point, end: Point, dx: f32, dy: f32, vertical: bool) {
        let coords = f32x4::new(start.x, start.y, end.x, end.y);
        // Whether the segment runs up and left, from the deltas' signs.
        let up = dy.to_bits() >> 31;
        let down = 1 - up;
        if vertical {
            self.v_line(coords, up);
        } else {
            let left = dx.to_bits() >> 31;
            let right = 1 - left;
            let (tdx, tdy) = recip2(dx, dy);
            let params = f32x4::new(tdx, tdy, dx, dy);
            let step = [right as i32 - left as i32, down as i32 - up as i32];
            self.m_line(coords, [left, up], step, params);
        }
    }

    /// The cell at `index`. The walks step this pointer rather than an index, which keeps the
    /// buffer's base out of the crossing loop's registers.
    #[inline(always)]
    fn at(self, index: i32) -> *mut f32 {
        self.ptr.wrapping_offset(index as isize)
    }

    #[inline(always)]
    fn add(self, cell: *mut f32, height: f32, mid_x: f32) {
        let m = height * mid_x;
        self.add_parts(cell, height - m, m);
    }

    /// In debug builds, that `cell` and the one after it are inside the buffer.
    #[inline(always)]
    fn check(self, cell: *mut f32) {
        #[cfg(debug_assertions)]
        assert!(
            (cell as usize).wrapping_sub(self.ptr as usize) / 4 + 1 < self.len,
            "raster write out of bounds"
        );
        let _ = cell;
    }

    /// `add_parts(cell, rest, m)` in each of `rows` rows from `cell`, stepping `inc` cells a row.
    /// Returns the cell after the last.
    #[inline(always)]
    fn whole_rows(self, cell: *mut f32, rows: i32, inc: i32, rest: f32, m: f32) -> *mut f32 {
        #[cfg(target_arch = "xtensa")]
        {
            for row in 0..rows {
                self.check(cell.wrapping_offset((row * inc) as isize));
            }
            if rows <= 0 {
                return cell;
            }
            let mut cell = cell;
            let mut rows = rows;
            // SAFETY: the rows are the ones the loop below would visit, each inside the raster
            // as `add_parts` requires. Both loads come before both sums, which LLVM does not do:
            // each sum then waits on its load and the store on its sum only once.
            unsafe {
                core::arch::asm!(
                    "1:",
                    "lsi   {a}, {cell}, 0",
                    "lsi   {b}, {cell}, 4",
                    "add.s {a}, {a}, {rest}",
                    "add.s {b}, {b}, {m}",
                    "addi  {rows}, {rows}, -1",
                    "ssi   {a}, {cell}, 0",
                    "ssi   {b}, {cell}, 4",
                    "add   {cell}, {cell}, {inc}",
                    "bnez  {rows}, 1b",
                    cell = inout(reg) cell,
                    rows = inout(reg) rows,
                    inc = in(reg) inc * 4,
                    rest = in(freg) rest,
                    m = in(freg) m,
                    a = out(freg) _,
                    b = out(freg) _,
                    options(nostack)
                );
            }
            let _ = rows;
            cell
        }
        #[cfg(not(target_arch = "xtensa"))]
        {
            let mut cell = cell;
            for _ in 0..rows {
                self.add_parts(cell, rest, m);
                cell = cell.wrapping_offset(inc as isize);
            }
            cell
        }
    }

    /// Adds a segment's area in a cell: `rest` to the cell and `m` to the one after it.
    #[inline(always)]
    fn add_parts(self, cell: *mut f32, rest: f32, m: f32) {
        self.check(cell);
        // Both loads, then both sums, so the second sum's latency overlaps the first's rather
        // than following it.
        // SAFETY: `Lines::line`'s contract puts `cell` inside the raster and `cell + 1` inside
        // its slack, which `check` asserts in debug builds.
        unsafe {
            let (a, b) = (*cell, *cell.add(1));
            *cell = a + rest;
            *cell.add(1) = b + m;
        }
    }

    #[inline(always)]
    fn v_line(self, coords: f32x4, up: u32) {
        let (x0, y0, _, y1) = coords.copied();
        let start_x = index_of(x0);
        let (start_y, end_y, target_y) = span(y0, y1, up);
        let w = self.w;
        let step = 1 - 2 * up as i32;
        let index_y_inc = step * w;
        let dist = (start_y - end_y).abs();
        let mid_x = fract_of(x0);
        let mut cell = self.at(start_x + start_y * w);
        let mut y_prev = y0;
        if dist > 0 {
            self.add(cell, y0 - target_y as f32, mid_x);
            cell = cell.wrapping_offset(index_y_inc as isize);
            // Every row between the first and the last is whole: row edges are integers, so its
            // height is exactly one, against the direction, and so are the two parts of it.
            let height = -step as f32;
            let m = height * mid_x;
            let rest = height - m;
            cell = self.whole_rows(cell, dist - 1, index_y_inc, rest, m);
            y_prev = (target_y + (dist - 1) * step) as f32;
        }
        // `dist` steps of one row from the start cell end in the end cell.
        self.add(cell, y_prev - y1, mid_x);
    }

    #[inline(always)]
    fn m_line(self, coords: f32x4, back: [u32; 2], step: [i32; 2], params: f32x4) {
        let (x0, y0, x1, y1) = coords.copied();
        // With the next column and row edge to cross. They are whole numbers, kept as integers so
        // the loop holds two fewer floats; converting one where it is used is exact.
        let (start_x, end_x, mut target_x) = span(x0, x1, back[0]);
        let (start_y, end_y, mut target_y) = span(y0, y1, back[1]);
        let (tdx, tdy, dx, dy) = params.copied();
        let [step_x, step_y] = step;
        let mut tmx = tdx * (target_x as f32 - x0);
        let mut tmy = tdy * (target_y as f32 - y0);
        let tdx = abs(tdx);
        let tdy = abs(tdy);
        let w = self.w;
        // Halved, so each crossing's midpoint is a sum; halving is exact, so the midpoints are
        // the same floats.
        let hx0 = halve(x0);
        let hdx = halve(dx);
        let mut hx_prev = hx0;
        let mut y_prev = y0;
        let mut cell = self.at(start_x + start_y * w);
        let index_y_inc = step_y * w;
        let dist = (start_x - end_x).unsigned_abs() + (start_y - end_y).unsigned_abs();
        // The next crossing: the cell it leaves, and the point it crosses at.
        macro_rules! cross {
            () => {{
                let left = cell;
                let y_next: f32;
                let hx_next: f32;
                if tmx < tmy {
                    y_next = mul_add(tmx, dy, y0);
                    hx_next = half_of(target_x);
                    tmx += tdx;
                    target_x += step_x;
                    cell = cell.wrapping_offset(step_x as isize);
                } else {
                    y_next = target_y as f32;
                    hx_next = mul_add(tmy, hdx, hx0);
                    tmy += tdy;
                    target_y += step_y;
                    cell = cell.wrapping_offset(index_y_inc as isize);
                }
                (left, y_next, hx_next)
            }};
        }
        let pairs = dist / 2;
        // Two crossings a trip, so the points alternate between two sets of registers instead of
        // being moved from one to the other each crossing.
        #[cfg(target_arch = "xtensa")]
        let rust_pairs = if pairs >= 2 {
            0
        } else {
            pairs
        };
        #[cfg(not(target_arch = "xtensa"))]
        let rust_pairs = pairs;
        for _ in 0..rust_pairs {
            let (cell_a, y_a, hx_a) = cross!();
            self.add(cell_a, y_prev - y_a, fract_of(hx_prev + hx_a));
            let (cell_b, y_b, hx_b) = cross!();
            self.add(cell_b, y_a - y_b, fract_of(hx_a + hx_b));
            hx_prev = hx_b;
            y_prev = y_b;
        }
        // The same two crossings, scheduled by hand: every float operation waits four cycles on
        // the one it depends on, and this core issues in order, so the first crossing's area is
        // worked into the second's step instead of after it. The operations, and the order of the
        // writes, are the loop's above: the second crossing's cell can be the first's plus one,
        // so its loads come after the first's stores.
        #[cfg(target_arch = "xtensa")]
        if pairs >= 2 {
            let mut n = pairs;
            // SAFETY: the cells written are the ones the loop above writes, each inside the
            // raster as `add_parts` requires.
            unsafe {
                core::arch::asm!(
                    "1:",
                    "olt.s   b0, {tmx}, {tmy}",
                    "bf      b0, 2f",
                    "mov.s   {ya}, {y0}",
                    "madd.s  {ya}, {tmx}, {dy}",
                    "float.s {ha}, {tx}, 1",
                    "add.s   {tmx}, {tmx}, {tdx}",
                    "mov     {la}, {cell}",
                    "addx4   {cell}, {sx}, {cell}",
                    "add     {tx}, {tx}, {sx}",
                    "j       3f",
                    "2:",
                    "mov.s   {ha}, {hx0}",
                    "madd.s  {ha}, {tmy}, {hdx}",
                    "float.s {ya}, {ty}, 0",
                    "add.s   {tmy}, {tmy}, {tdy}",
                    "mov     {la}, {cell}",
                    "addx4   {cell}, {iy}, {cell}",
                    "add     {ty}, {ty}, {sy}",
                    "3:",
                    // The first crossing's midpoint and height, over the previous point.
                    "add.s   {hp}, {hp}, {ha}",
                    "sub.s   {yp}, {yp}, {ya}",
                    "olt.s   b0, {tmx}, {tmy}",
                    "bf      b0, 4f",
                    "mov.s   {yb}, {y0}",
                    "madd.s  {yb}, {tmx}, {dy}",
                    "trunc.s {ia}, {hp}, 0",
                    "float.s {hb}, {tx}, 1",
                    "add.s   {tmx}, {tmx}, {tdx}",
                    "mov     {lb}, {cell}",
                    "addx4   {cell}, {sx}, {cell}",
                    "add     {tx}, {tx}, {sx}",
                    "float.s {t1}, {ia}, 0",
                    "j       5f",
                    "4:",
                    "mov.s   {hb}, {hx0}",
                    "madd.s  {hb}, {tmy}, {hdx}",
                    "trunc.s {ia}, {hp}, 0",
                    "float.s {yb}, {ty}, 0",
                    "add.s   {tmy}, {tmy}, {tdy}",
                    "mov     {lb}, {cell}",
                    "addx4   {cell}, {iy}, {cell}",
                    "add     {ty}, {ty}, {sy}",
                    "float.s {t1}, {ia}, 0",
                    "5:",
                    // Both crossings' fractions and parts, interleaved; the first's writes, then
                    // the second's.
                    "sub.s   {t1}, {hp}, {t1}",
                    "add.s   {hp}, {ha}, {hb}",
                    "sub.s   {ha}, {ya}, {yb}",
                    "mul.s   {t1}, {yp}, {t1}",
                    "trunc.s {ib}, {hp}, 0",
                    "lsi     {ya}, {la}, 4",
                    "float.s {t2}, {ib}, 0",
                    "sub.s   {yp}, {yp}, {t1}",
                    "add.s   {ya}, {ya}, {t1}",
                    "sub.s   {t2}, {hp}, {t2}",
                    "lsi     {hp}, {la}, 0",
                    "mul.s   {t2}, {ha}, {t2}",
                    "add.s   {hp}, {hp}, {yp}",
                    "ssi     {ya}, {la}, 4",
                    "sub.s   {ha}, {ha}, {t2}",
                    "ssi     {hp}, {la}, 0",
                    "lsi     {ya}, {lb}, 4",
                    "lsi     {hp}, {lb}, 0",
                    "add.s   {ya}, {ya}, {t2}",
                    "add.s   {hp}, {hp}, {ha}",
                    "addi    {n}, {n}, -1",
                    "mov.s   {yp}, {yb}",
                    "ssi     {ya}, {lb}, 4",
                    "ssi     {hp}, {lb}, 0",
                    "mov.s   {hp}, {hb}",
                    "bnez    {n}, 1b",
                    tmx = inout(freg) tmx,
                    tmy = inout(freg) tmy,
                    yp = inout(freg) y_prev,
                    hp = inout(freg) hx_prev,
                    tdx = in(freg) tdx,
                    tdy = in(freg) tdy,
                    dy = in(freg) dy,
                    y0 = in(freg) y0,
                    hdx = in(freg) hdx,
                    hx0 = in(freg) hx0,
                    ya = out(freg) _,
                    ha = out(freg) _,
                    yb = out(freg) _,
                    hb = out(freg) _,
                    t1 = out(freg) _,
                    t2 = out(freg) _,
                    cell = inout(reg) cell,
                    tx = inout(reg) target_x,
                    ty = inout(reg) target_y,
                    n = inout(reg) n,
                    sx = in(reg) step_x,
                    sy = in(reg) step_y,
                    iy = in(reg) index_y_inc,
                    la = out(reg) _,
                    lb = out(reg) _,
                    ia = out(reg) _,
                    ib = out(reg) _,
                    out("b0") _,
                    options(nostack)
                );
            }
            let _ = n;
        }
        if dist % 2 == 1 {
            // The last crossing: the walk's state after it is not read.
            #[allow(unused_assignments)]
            let (cell_a, y_a, hx_a) = cross!();
            self.add(cell_a, y_prev - y_a, fract_of(hx_prev + hx_a));
            hx_prev = hx_a;
            y_prev = y_a;
        }
        self.add(self.at(end_x + end_y * w), y_prev - y1, fract_of(hx_prev + halve(x1)));
    }
}

/// The cells a segment starts and ends in along one axis, and the first cell edge it crosses,
/// for a segment from `v0` to `v1` that runs toward lower coordinates when `back` is 1. A
/// coordinate exactly on an edge belongs to the cell the segment is inside of there: the one
/// below the edge at the start of a segment running back, or at the end of one running forward.
/// Neither of those coordinates is zero, since the other end is lower still.
#[inline(always)]
fn span(v0: f32, v1: f32, back: u32) -> (i32, i32, i32) {
    if back != 0 {
        let start = ceil_index(v0) - 1;
        (start, index_of(v1), start)
    } else {
        let start = index_of(v0);
        (start, ceil_index(v1) - 1, start + 1)
    }
}

/// `ceil(value)` as an integer, for a coordinate inside the raster, as `index_of` truncates one.
#[inline(always)]
fn ceil_index(value: f32) -> i32 {
    // SAFETY: as in `index_of`.
    unsafe { crate::platform::ceil_i32_unchecked(value) }
}

/// The fractional part of a coordinate inside the raster, through the same unchecked convert.
#[inline(always)]
fn fract_of(value: f32) -> f32 {
    value - index_of(value) as f32
}

/// `magnitude`, negated when `sign`'s sign bit is set, as `copysign` does for floats.
#[inline(always)]
fn with_sign_of(magnitude: i32, sign: f32) -> i32 {
    let mask = (sign.to_bits() as i32) >> 31;
    (magnitude ^ mask) - mask
}

const BLOCK: usize = 4;

/// How a pixel's accumulated winding area becomes coverage. Implemented by [`NonZero`] and
/// [`EvenOdd`] only.
pub trait FillRule: rule::Rule {}

/// Covered where the winding number is not zero.
#[derive(Clone, Copy, Debug, Default)]
pub struct NonZero;

/// Covered where the winding number is odd.
#[derive(Clone, Copy, Debug, Default)]
pub struct EvenOdd;

impl FillRule for NonZero {}
impl FillRule for EvenOdd {}

mod rule {
    pub trait Rule: Copy {
        /// Four pixels' coverage, each at most 255, advancing the running total.
        fn block(height: &mut f32, deltas: &[f32; super::BLOCK]) -> [u32; super::BLOCK];

        /// One pixel's coverage, advancing the running total.
        fn step(height: &mut f32, delta: f32) -> u8;
    }
}

/// Running prefix sum over the area deltas, yielding one coverage byte per pixel.
#[derive(Clone)]
pub struct BitmapIter<'r, R: FillRule = NonZero> {
    /// Area deltas. `resize` keeps this at `w * h + 3`, and those three slots are what let the
    /// final block read four floats without running off the end. Shortening the slack breaks this
    /// iterator before it breaks `Raster::add`.
    a: &'r [f32],
    /// Index of the next delta to consume.
    pos: usize,
    remaining: usize,
    /// Coverage total accumulated up to `pos`.
    height: f32,
    /// The current block's unread bytes, next one lowest. Packed rather than an array indexed by
    /// position, so the iterator's state can stay in registers while a `for` loop drives `next`.
    block: u32,
    /// Bytes of `block` not yet yielded.
    block_left: usize,
    _rule: PhantomData<R>,
}

impl NonZero {
    /// The coverage byte for a running total.
    #[inline(always)]
    fn coverage(height: f32) -> u8 {
        let coverage = abs(height) * 255.9;
        // The clamp to 255.0 is an integer `min` on the bits, which has no branch. `abs` leaves
        // `coverage` non-negative or NaN. Non-negative floats order as their bits do, and every NaN
        // pattern is above 255.0's bits, so a NaN lands on 255.0 as well. That makes the
        // conversion below total for every input, with no argument about what the geometry can
        // produce.
        let coverage = f32::from_bits(coverage.to_bits().min(255f32.to_bits()));
        // SAFETY: `coverage` is in `[0.0, 255.0]` by the clamp above.
        unsafe { coverage.to_int_unchecked::<u8>() }
    }
}

impl EvenOdd {
    /// The running total folded into `[0, 1]`: its magnitude mod 2, reflected above 1. Infinite or
    /// NaN totals give NaN, which `NonZero::coverage` maps to 255.
    #[inline(always)]
    fn coverage(height: f32) -> u8 {
        let c = abs(height);
        let c = c - 2.0 * floor(c * 0.5);
        NonZero::coverage(if c > 1.0 {
            2.0 - c
        } else {
            c
        })
    }
}

/// Four pixels through `coverage`. The running total is serial, the four conversions are not.
/// Written out rather than looped, because at `opt-level = "s"` a loop here is not unrolled, and
/// the conversions then cannot overlap the next addition.
#[inline(always)]
fn portable_block(height: &mut f32, deltas: &[f32; BLOCK], coverage: fn(f32) -> u8) -> [u32; BLOCK] {
    let h0 = *height + deltas[0];
    let h1 = h0 + deltas[1];
    let h2 = h1 + deltas[2];
    let h3 = h2 + deltas[3];
    *height = h3;
    [h0, h1, h2, h3].map(|h| coverage(h) as u32)
}

impl rule::Rule for EvenOdd {
    #[inline(always)]
    fn block(height: &mut f32, deltas: &[f32; BLOCK]) -> [u32; BLOCK] {
        portable_block(height, deltas, Self::coverage)
    }

    #[inline(always)]
    fn step(height: &mut f32, delta: f32) -> u8 {
        *height += delta;
        Self::coverage(*height)
    }
}

impl rule::Rule for NonZero {
    #[inline(always)]
    fn step(height: &mut f32, delta: f32) -> u8 {
        *height += delta;
        Self::coverage(*height)
    }

    /// `portable_block` with `coverage`, hand-scheduled.
    #[cfg(target_arch = "xtensa")]
    #[inline(always)]
    fn block(height: &mut f32, deltas: &[f32; BLOCK]) -> [u32; BLOCK] {
        let (c0, c1, c2, c3): (u32, u32, u32, u32);
        // Same operations per pixel as the portable form, in the same order for the running total,
        // with each pixel's conversion placed in the stall slots of the next addition.
        // SAFETY: reads the four floats `deltas` points to and nothing else; every result is clamped
        // to 255.0 before `utrunc.s`.
        unsafe {
            core::arch::asm!(
                "lsi {d0}, {p}, 0",
                "lsi {d1}, {p}, 4",
                "lsi {d2}, {p}, 8",
                "lsi {d3}, {p}, 12",
                "add.s {d0}, {h}, {d0}",
                "add.s {d1}, {d0}, {d1}",
                "abs.s {t0}, {d0}",
                "add.s {d2}, {d1}, {d2}",
                "abs.s {t1}, {d1}",
                "add.s {h}, {d2}, {d3}",
                "mul.s {t0}, {t0}, {k}",
                "abs.s {t2}, {d2}",
                "mul.s {t1}, {t1}, {k}",
                "abs.s {t3}, {h}",
                "mul.s {t2}, {t2}, {k}",
                "rfr {c0}, {t0}",
                "mul.s {t3}, {t3}, {k}",
                "rfr {c1}, {t1}",
                "minu {c0}, {c0}, {lim}",
                "rfr {c2}, {t2}",
                "minu {c1}, {c1}, {lim}",
                "rfr {c3}, {t3}",
                "wfr {t0}, {c0}",
                "minu {c2}, {c2}, {lim}",
                "wfr {t1}, {c1}",
                "minu {c3}, {c3}, {lim}",
                "wfr {t2}, {c2}",
                "utrunc.s {c0}, {t0}, 0",
                "wfr {t3}, {c3}",
                "utrunc.s {c1}, {t1}, 0",
                "utrunc.s {c2}, {t2}, 0",
                "utrunc.s {c3}, {t3}, 0",
                p = in(reg) deltas.as_ptr(),
                lim = in(reg) 255f32.to_bits(),
                k = in(freg) 255.9f32,
                h = inout(freg) *height,
                d0 = out(freg) _, d1 = out(freg) _, d2 = out(freg) _, d3 = out(freg) _,
                t0 = out(freg) _, t1 = out(freg) _, t2 = out(freg) _, t3 = out(freg) _,
                c0 = out(reg) c0, c1 = out(reg) c1, c2 = out(reg) c2, c3 = out(reg) c3,
                options(pure, readonly, nostack),
            );
        }
        // SAFETY: `minu` against 255.0's bits before each `utrunc.s` bounds every result by 255.
        unsafe { core::hint::assert_unchecked(c0 <= 255 && c1 <= 255 && c2 <= 255 && c3 <= 255) };
        [c0, c1, c2, c3]
    }

    #[cfg(not(target_arch = "xtensa"))]
    #[inline(always)]
    fn block(height: &mut f32, deltas: &[f32; BLOCK]) -> [u32; BLOCK] {
        portable_block(height, deltas, Self::coverage)
    }
}

impl<R: FillRule> BitmapIter<'_, R> {
    /// Computes the next four bytes.
    #[inline(always)]
    fn refill(&mut self) {
        debug_assert!(self.pos + BLOCK <= self.a.len());
        // SAFETY: `next` refills only while pixels remain, so `pos < w * h`, and `resize` keeps `a`
        // at `w * h + 3` floats. The four from `pos` are therefore in bounds.
        let deltas = unsafe { &*(self.a.as_ptr().add(self.pos) as *const [f32; BLOCK]) };
        let [c0, c1, c2, c3] = R::block(&mut self.height, deltas);
        // Each is at most 255, so the fields do not overlap.
        self.block = c0 | c1 << 8 | c2 << 16 | c3 << 24;
        self.pos += BLOCK;
        self.block_left = BLOCK;
    }
}

impl<R: FillRule> Iterator for BitmapIter<'_, R> {
    type Item = u8;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        if self.block_left == 0 {
            self.refill();
        }
        let v = self.block as u8;
        self.block >>= 8;
        self.block_left -= 1;
        self.remaining -= 1;
        Some(v)
    }

    /// Internal iteration walks the deltas directly, with no buffering at all. This is the path
    /// `for_each`, `count` and `sum` take; `collect` does not, since `Vec` extends through `next`
    /// for iterators that are not `TrustedLen`.
    fn fold<B, F>(mut self, init: B, mut f: F) -> B
    where
        F: FnMut(B, Self::Item) -> B,
    {
        let mut acc = init;
        while self.remaining > 0 && self.block_left > 0 {
            acc = f(acc, self.block as u8);
            self.block >>= 8;
            self.block_left -= 1;
            self.remaining -= 1;
        }
        let deltas = &self.a[self.pos..self.pos + self.remaining];
        let mut blocks = deltas.chunks_exact(BLOCK);
        for chunk in &mut blocks {
            let [c0, c1, c2, c3] = R::block(&mut self.height, chunk.try_into().unwrap());
            acc = f(acc, c0 as u8);
            acc = f(acc, c1 as u8);
            acc = f(acc, c2 as u8);
            acc = f(acc, c3 as u8);
        }
        for &delta in blocks.remainder() {
            acc = f(acc, R::step(&mut self.height, delta));
        }
        acc
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl<R: FillRule> FusedIterator for BitmapIter<'_, R> {}
impl<R: FillRule> ExactSizeIterator for BitmapIter<'_, R> {
    fn len(&self) -> usize {
        self.remaining
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iter<R: FillRule>(a: &[f32], len: usize) -> BitmapIter<'_, R> {
        BitmapIter {
            a,
            pos: 0,
            remaining: len,
            height: 0.0,
            block: 0,
            block_left: 0,
            _rule: PhantomData,
        }
    }

    /// The integer clamp in `coverage` against the float comparison it replaced, NaN included.
    #[test]
    fn coverage_matches_the_float_clamp() {
        let float_clamp = |height: f32| {
            let coverage = abs(height) * 255.9;
            let coverage = if coverage < 255.0 {
                coverage
            } else {
                255.0
            };
            unsafe { coverage.to_int_unchecked::<u8>() }
        };
        let special =
            [0.0, -0.0, 1.0, -1.0, 0.9965, 0.99648, f32::INFINITY, f32::NEG_INFINITY, f32::NAN, -f32::NAN];
        for h in special.into_iter().chain((0..=u32::MAX).step_by(4099).map(f32::from_bits)) {
            assert_eq!(NonZero::coverage(h), float_clamp(h), "height {h:e}");
        }
    }

    /// `fold` has its own blocking and remainder, separate from `next`'s, and must yield the same
    /// bytes: for every length mod 4, after `next` has taken part of a block, and for totals that
    /// overshoot the clamp or go NaN.
    #[test]
    fn fold_matches_next() {
        let deltas: Vec<f32> = (0..40)
            .map(|i| match i {
                17 => f32::NAN,
                23 => -f32::NAN,
                _ => ((i * 37 % 11) as f32 - 5.0) * 0.23,
            })
            .collect();
        fold_matches_next_for::<NonZero>(&deltas);
        fold_matches_next_for::<EvenOdd>(&deltas);
    }

    fn fold_matches_next_for<R: FillRule>(deltas: &[f32]) {
        for len in 0..=deltas.len() - 3 {
            let a = &deltas[..len + 3];
            let by_next: Vec<u8> = iter::<R>(a, len).collect();
            for taken in 0..=len.min(6) {
                let mut it = iter::<R>(a, len);
                let mut by_fold: Vec<u8> = it.by_ref().take(taken).collect();
                by_fold = it.fold(by_fold, |mut v, c| {
                    v.push(c);
                    v
                });
                assert_eq!(by_fold, by_next, "len {len}, {taken} taken through next first");
            }
        }
    }

    /// Even-odd coverage of a running total: whole windings alternate between empty and covered,
    /// and a fraction between them is the distance to the nearer even winding.
    #[test]
    fn even_odd_folds_windings() {
        for (height, want) in [
            (0.0, 0),
            (1.0, 255),
            (-1.0, 255),
            (2.0, 0),
            (-2.0, 0),
            (3.0, 255),
            (0.5, 127),
            (1.5, 127),
            (2.25, 63),
            (-2.25, 63),
            (f32::INFINITY, 255),
            (f32::NAN, 255),
        ] {
            assert_eq!(EvenOdd::coverage(height), want, "height {height}");
        }
    }
}
