/* Notice to anyone that wants to repurpose the raster for your library:
 * Please don't reuse this raster. Fontdue's raster is very unsafe, with nuanced invariants that
 * need to be accounted for. Fontdue sanitizes the input that the raster will consume to ensure it
 * is safe. Please be aware of this.
 */

use crate::GlyphRef;
use crate::platform::{abs, as_i32_unchecked, copysign, f32x4, fract};
use alloc::vec::*;
use core::iter::FusedIterator;

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
                a.resize(len, 0.0);
            }
            RasterBuffer::Borrowed(a) => {
                a[..len].fill(0.0);
            }
        }
    }

    pub(crate) fn draw(
        &mut self,
        glyph: &GlyphRef<'_>,
        scale_x: f32,
        scale_y: f32,
        offset_x: f32,
        offset_y: f32,
    ) {
        let params = f32x4::new(1.0 / scale_x, 1.0 / scale_y, scale_x, scale_y);
        let scale = f32x4::new(scale_x, scale_y, scale_x, scale_y);
        let offset = f32x4::new(offset_x, offset_y, offset_x, offset_y);
        for line in glyph.v_lines {
            let (nudge, adjustment, _) = line.raster_parts();
            self.v_line(line.coords * scale + offset, nudge, adjustment);
        }
        for line in glyph.m_lines {
            let (nudge, adjustment, mut line_params) = line.raster_parts();
            line_params = line_params * params;
            self.m_line(line.coords * scale + offset, nudge, adjustment, line_params);
        }
    }

    #[inline(always)]
    fn add(&mut self, index: usize, height: f32, mid_x: f32) {
        // This is fast and hip.
        unsafe {
            let m = height * mid_x;
            let a = match &mut self.a {
                RasterBuffer::Owned(a) => a.as_mut_slice(),
                RasterBuffer::Borrowed(a) => a,
            };
            *a.get_unchecked_mut(index) += height - m;
            *a.get_unchecked_mut(index + 1) += m;
        }

        // This is safe but slow.
        // let m = height * mid_x;
        // self.a[index] += height - m;
        // self.a[index + 1] += m;
    }

    #[inline(always)]
    fn v_line(&mut self, coords: f32x4, nudge: f32x4, adjustment: f32x4) {
        let (x0, y0, _, y1) = coords.copied();
        let temp = coords.sub_integer(nudge).trunc();
        let (start_x, start_y, end_x, end_y) = temp.copied();
        let (_, mut target_y, _, _) = (temp + adjustment).copied();
        let sy = copysign(1f32, y1 - y0);
        let mut y_prev = y0;
        let mut index = index_of(start_x + start_y * self.w as f32);
        let index_y_inc = index_of(copysign(self.w as f32, sy));
        let mut dist = index_of(abs(start_y - end_y));
        let mid_x = fract(x0);
        while dist > 0 {
            dist -= 1;
            self.add(index as usize, y_prev - target_y, mid_x);
            index += index_y_inc;
            y_prev = target_y;
            target_y += sy;
        }
        self.add(index_of(end_x + end_y * self.w as f32) as usize, y_prev - y1, mid_x);
    }

    #[inline(always)]
    fn m_line(&mut self, coords: f32x4, nudge: f32x4, adjustment: f32x4, params: f32x4) {
        let (x0, y0, x1, y1) = coords.copied();
        let temp = coords.sub_integer(nudge).trunc();
        let (start_x, start_y, end_x, end_y) = temp.copied();
        let (tdx, tdy, dx, dy) = params.copied();
        let (mut target_x, mut target_y, _, _) = (temp + adjustment).copied();
        let sx = copysign(1f32, tdx);
        let sy = copysign(1f32, tdy);
        let mut tmx = tdx * (target_x - x0);
        let mut tmy = tdy * (target_y - y0);
        let tdx = abs(tdx);
        let tdy = abs(tdy);
        let mut x_prev = x0;
        let mut y_prev = y0;
        let mut index = index_of(start_x + start_y * self.w as f32);
        let index_x_inc = index_of(sx);
        let index_y_inc = index_of(copysign(self.w as f32, sy));
        let mut dist = index_of(abs(start_x - end_x) + abs(start_y - end_y));
        while dist > 0 {
            dist -= 1;
            let prev_index = index;
            let y_next: f32;
            let x_next: f32;
            if tmx < tmy {
                y_next = tmx * dy + y0; // FMA is not faster.
                x_next = target_x;
                tmx += tdx;
                target_x += sx;
                index += index_x_inc;
            } else {
                y_next = target_y;
                x_next = tmy * dx + x0;
                tmy += tdy;
                target_y += sy;
                index += index_y_inc;
            }
            self.add(prev_index as usize, y_prev - y_next, fract((x_prev + x_next) / 2.0));
            x_prev = x_next;
            y_prev = y_next;
        }
        self.add(index_of(end_x + end_y * self.w as f32) as usize, y_prev - y1, fract((x_prev + x1) / 2.0));
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
            block: [0; BLOCK],
            block_pos: BLOCK,
        }
    }
}

/// Float-to-int for the line loops.
///
/// `draw` is only ever reached through `rasterize_inner`, which sizes the raster from
/// `metrics_raw` and then scales every coordinate into it. Every value converted here is a pixel
/// index or a step count inside those bounds, so the unchecked convert has its precondition. This
/// is the same trust `add` already places in the caller, and the notice at the top of the file is
/// about exactly this.
#[inline(always)]
fn index_of(value: f32) -> i32 {
    unsafe { as_i32_unchecked(value) }
}

const BLOCK: usize = 4;

/// Running prefix sum over the area deltas, yielding one coverage byte per pixel.
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
    block: [u8; BLOCK],
    /// Next byte to yield from `block`; `BLOCK` once it is spent.
    block_pos: usize,
}

impl BitmapIter<'_> {
    /// Folds one delta into the running total and returns its coverage byte.
    #[inline(always)]
    fn step(height: &mut f32, delta: f32) -> u8 {
        *height += delta;
        let coverage = abs(*height) * 255.9;
        // Written as `< 255.0` rather than `> 255.0` so a NaN fails the comparison and lands on
        // 255.0. `abs` covers the lower bound, so the conversion below is total for every input
        // and needs no argument about what the geometry can produce. Both forms cost the same.
        let coverage = if coverage < 255.0 {
            coverage
        } else {
            255.0
        };
        // SAFETY: `coverage` is in `[0.0, 255.0]` by the clamp above.
        unsafe { coverage.to_int_unchecked::<u8>() }
    }

    /// Computes the next four bytes in one go. Spreading the float dependency chain over four
    /// pixels is what makes `next` cheap enough for `collect`.
    #[inline(always)]
    fn refill(&mut self) {
        for i in 0..BLOCK {
            self.block[i] = Self::step(&mut self.height, self.a[self.pos + i]);
        }
        self.pos += BLOCK;
        self.block_pos = 0;
    }
}

impl Iterator for BitmapIter<'_> {
    type Item = u8;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        if self.block_pos == BLOCK {
            self.refill();
        }
        let v = self.block[self.block_pos];
        self.block_pos += 1;
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
        while self.remaining > 0 && self.block_pos < BLOCK {
            acc = f(acc, self.block[self.block_pos]);
            self.block_pos += 1;
            self.remaining -= 1;
        }
        for &delta in &self.a[self.pos..self.pos + self.remaining] {
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
