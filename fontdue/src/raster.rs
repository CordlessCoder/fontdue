/* Notice to anyone that wants to repurpose the raster for your library:
 * Please don't reuse this raster. Fontdue's raster is very unsafe, with nuanced invariants that
 * need to be accounted for. Fontdue sanitizes the input that the raster will consume to ensure it
 * is safe. Please be aware of this.
 */

use crate::math::{Line, Point};
use crate::platform::{abs, as_i32_unchecked, copysign, f32x4, mul_add};
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

    pub(crate) fn resize(&mut self, w: usize, h: usize) {
        // Checked, because `add` indexes with `get_unchecked_mut` off `w` and `h`. A wrapped
        // length would leave the buffer shorter than the dimensions the raster then trusts.
        let len =
            w.checked_mul(h).and_then(|area| area.checked_add(3)).expect("raster dimensions overflow usize");
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

    #[inline(always)]
    pub fn get_bitmap_iter<'r>(&'r self) -> BitmapIter<'r> {
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
        }
    }
}

/// Float-to-int for the line loops.
///
/// Lines are only drawn through a `Sink`, which `rasterize_with` makes for a raster sized from
/// `metrics_raw`, and every glyph kind promises its points lie inside the bounds those metrics came
/// from (see `outline`). Every value converted here is a pixel coordinate inside those bounds, so
/// the unchecked convert has its precondition. This
/// is the same trust `add` already places in the caller, and the notice at the top of the file is
/// about exactly this.
#[inline(always)]
fn index_of(value: f32) -> i32 {
    unsafe { as_i32_unchecked(value) }
}

/// Where an [`OutlineSource`](crate::OutlineSource) sends a glyph's segments. Only the raster
/// makes one, sized and scaled for the glyph being drawn.
pub struct Sink<'s, 'b> {
    /// The raster's cells, taken out of it once so nothing in the line walk can alias them. The
    /// buffer holds `w * h + 3` cells, `resize` having sized it for the glyph.
    cells: *mut f32,
    w: i32,
    /// Scalars, not the `f32x4`s the walk takes: read back from memory in a source's out-of-line
    /// `draw`, duplicated lanes are separate loads and separate live registers.
    scale_x: f32,
    scale_y: f32,
    inv_x: f32,
    inv_y: f32,
    offset_x: f32,
    offset_y: f32,
    _raster: PhantomData<&'s mut Raster<'b>>,
}

impl<'s, 'b> Sink<'s, 'b> {
    #[inline(always)]
    pub(crate) fn new(
        raster: &'s mut Raster<'b>,
        scale_x: f32,
        scale_y: f32,
        offset_x: f32,
        offset_y: f32,
    ) -> Self {
        let w = raster.w as i32;
        let cells = match &mut raster.a {
            RasterBuffer::Owned(a) => a.as_mut_ptr(),
            RasterBuffer::Borrowed(a) => a.as_mut_ptr(),
        };
        Sink {
            cells,
            w,
            scale_x,
            scale_y,
            inv_x: 1.0 / scale_x,
            inv_y: 1.0 / scale_y,
            offset_x,
            offset_y,
            _raster: PhantomData,
        }
    }

    /// Draws one segment, `[x0, y0, x1, y1]` in the source's point units.
    #[inline(always)]
    pub fn segment(&mut self, [x0, y0, x1, y1]: [f32; 4]) {
        let line = Line::new(Point::new(x0, y0), Point::new(x1, y1));
        if x0 == x1 {
            self.v(&line);
        } else {
            self.m(&line);
        }
    }

    #[inline(always)]
    pub(crate) fn lines(&mut self, v_lines: &[Line], m_lines: &[Line]) {
        for line in v_lines {
            self.v(line);
        }
        for line in m_lines {
            self.m(line);
        }
    }

    /// The line's coordinates in raster space.
    #[inline(always)]
    fn place(&self, line: &Line) -> f32x4 {
        let (sx, sy, ox, oy) = (self.scale_x, self.scale_y, self.offset_x, self.offset_y);
        line.coords * f32x4::new(sx, sy, sx, sy) + f32x4::new(ox, oy, ox, oy)
    }

    #[inline(always)]
    fn v(&mut self, line: &Line) {
        let (nudge, adjustment, _) = line.raster_parts();
        self.v_line(self.place(line), nudge, adjustment);
    }

    #[inline(always)]
    fn m(&mut self, line: &Line) {
        let (nudge, adjustment, line_params) = line.raster_parts();
        let params = f32x4::new(self.inv_x, self.inv_y, self.scale_x, self.scale_y);
        self.m_line(self.place(line), nudge, adjustment, line_params * params);
    }

    #[inline(always)]
    fn add(&mut self, index: usize, height: f32, mid_x: f32) {
        // This is fast and hip.
        unsafe {
            let m = height * mid_x;
            let cell = self.cells.add(index);
            *cell += height - m;
            *cell.add(1) += m;
        }

        // This is safe but slow.
        // let m = height * mid_x;
        // self.a[index] += height - m;
        // self.a[index + 1] += m;
    }

    #[inline(always)]
    fn v_line(&mut self, coords: f32x4, nudge: [u32; 4], adjustment: [i32; 2]) {
        let (x0, y0, _, y1) = coords.copied();
        let (start_x, start_y, end_x, end_y) = cells(coords, nudge);
        let mut target_y = (start_y + adjustment[1]) as f32;
        let sy = copysign(1f32, y1 - y0);
        let w = self.w;
        let mut y_prev = y0;
        let mut index = start_x + start_y * w;
        let index_y_inc = with_sign_of(w, y1 - y0);
        let mut dist = (start_y - end_y).abs();
        let mid_x = fract_of(x0);
        while dist > 0 {
            dist -= 1;
            self.add(index as usize, y_prev - target_y, mid_x);
            index += index_y_inc;
            y_prev = target_y;
            target_y += sy;
        }
        self.add((end_x + end_y * w) as usize, y_prev - y1, mid_x);
    }

    #[inline(always)]
    fn m_line(&mut self, coords: f32x4, nudge: [u32; 4], adjustment: [i32; 2], params: f32x4) {
        let (x0, y0, x1, y1) = coords.copied();
        let (start_x, start_y, end_x, end_y) = cells(coords, nudge);
        let (tdx, tdy, dx, dy) = params.copied();
        // The next column and row edge to cross. They are whole numbers, kept as integers so the
        // loop holds two fewer floats; converting one where it is used is exact.
        let mut target_x = start_x + adjustment[0];
        let mut target_y = start_y + adjustment[1];
        let step_x = with_sign_of(1, tdx);
        let step_y = with_sign_of(1, tdy);
        let mut tmx = tdx * (target_x as f32 - x0);
        let mut tmy = tdy * (target_y as f32 - y0);
        let tdx = abs(tdx);
        let tdy = abs(tdy);
        let w = self.w;
        let mut x_prev = x0;
        let mut y_prev = y0;
        let mut index = start_x + start_y * w;
        let index_y_inc = step_y * w;
        let dist = (start_x - end_x).unsigned_abs() + (start_y - end_y).unsigned_abs();
        for _ in 0..dist {
            let prev_index = index;
            let y_next: f32;
            let x_next: f32;
            if tmx < tmy {
                y_next = mul_add(tmx, dy, y0);
                x_next = target_x as f32;
                tmx += tdx;
                target_x += step_x;
                index += step_x;
            } else {
                y_next = target_y as f32;
                x_next = mul_add(tmy, dx, x0);
                tmy += tdy;
                target_y += step_y;
                index += index_y_inc;
            }
            self.add(prev_index as usize, y_prev - y_next, fract_of((x_prev + x_next) / 2.0));
            x_prev = x_next;
            y_prev = y_next;
        }
        self.add((end_x + end_y * w) as usize, y_prev - y1, fract_of((x_prev + x1) / 2.0));
    }
}

/// The pixel column and row each end of a line lies in, after the nudges. The loops keep indices
/// and step counts as integers from here on and convert nothing back.
///
/// A nudge of 1 steps a coordinate down to the next float by subtracting one from its bits. The
/// nudges stay integers throughout: as `f32` bit patterns they were a choice between two float
/// constants, which LLVM at opt-level 2 and above turns into a constant-pool table that the Xtensa
/// backend fails to select.
#[inline(always)]
fn cells(coords: f32x4, nudge: [u32; 4]) -> (i32, i32, i32, i32) {
    let (x0, y0, x1, y1) = coords.copied();
    let step = |v: f32, n: u32| index_of(f32::from_bits(v.to_bits().wrapping_sub(n)));
    (step(x0, nudge[0]), step(y0, nudge[1]), step(x1, nudge[2]), step(y1, nudge[3]))
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

/// Running prefix sum over the area deltas, yielding one coverage byte per pixel.
#[derive(Clone)]
pub struct BitmapIter<'r> {
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
}

impl BitmapIter<'_> {
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

    /// Folds one delta into the running total and returns its coverage byte.
    #[inline(always)]
    fn step(height: &mut f32, delta: f32) -> u8 {
        *height += delta;
        Self::coverage(*height)
    }

    /// Four pixels in one go, each at most 255. The running total is serial, the four conversions
    /// are not. Written out rather than looped, because at `opt-level = "s"` a loop here is not
    /// unrolled, and the conversions then cannot overlap the next addition.
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
        let h0 = *height + deltas[0];
        let h1 = h0 + deltas[1];
        let h2 = h1 + deltas[2];
        let h3 = h2 + deltas[3];
        *height = h3;
        [h0, h1, h2, h3].map(|h| Self::coverage(h) as u32)
    }

    /// Computes the next four bytes.
    #[inline(always)]
    fn refill(&mut self) {
        debug_assert!(self.pos + BLOCK <= self.a.len());
        // SAFETY: `next` refills only while pixels remain, so `pos < w * h`, and `resize` keeps `a`
        // at `w * h + 3` floats. The four from `pos` are therefore in bounds.
        let deltas = unsafe { &*(self.a.as_ptr().add(self.pos) as *const [f32; BLOCK]) };
        let [c0, c1, c2, c3] = Self::block(&mut self.height, deltas);
        // Each is at most 255, so the fields do not overlap.
        self.block = c0 | c1 << 8 | c2 << 16 | c3 << 24;
        self.pos += BLOCK;
        self.block_left = BLOCK;
    }
}

impl Iterator for BitmapIter<'_> {
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
            let [c0, c1, c2, c3] = Self::block(&mut self.height, chunk.try_into().unwrap());
            acc = f(acc, c0 as u8);
            acc = f(acc, c1 as u8);
            acc = f(acc, c2 as u8);
            acc = f(acc, c3 as u8);
        }
        for &delta in blocks.remainder() {
            acc = f(acc, Self::step(&mut self.height, delta));
        }
        acc
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl FusedIterator for BitmapIter<'_> {}
impl ExactSizeIterator for BitmapIter<'_> {
    fn len(&self) -> usize {
        self.remaining
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iter(a: &[f32], len: usize) -> BitmapIter<'_> {
        BitmapIter {
            a,
            pos: 0,
            remaining: len,
            height: 0.0,
            block: 0,
            block_left: 0,
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
            assert_eq!(BitmapIter::coverage(h), float_clamp(h), "height {h:e}");
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
        for len in 0..=deltas.len() - 3 {
            let a = &deltas[..len + 3];
            let by_next: Vec<u8> = iter(a, len).collect();
            for taken in 0..=len.min(6) {
                let mut it = iter(a, len);
                let mut by_fold: Vec<u8> = it.by_ref().take(taken).collect();
                by_fold = it.fold(by_fold, |mut v, c| {
                    v.push(c);
                    v
                });
                assert_eq!(by_fold, by_next, "len {len}, {taken} taken through next first");
            }
        }
    }
}
