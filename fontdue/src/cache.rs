//! A cache of decoded glyphs that wraps any font.

use crate::font::{LineMetrics, Metrics, metrics_raw_stretched};
use crate::outline::{GlyphRef, OutlineInfo, OutlineSource, PathEvent, PathGlyph};
use crate::raster::{BitmapIter, Raster};
use crate::{FontRepr, OutlineBounds, Transform, TransformedMetrics};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::marker::PhantomData;
use critical_section::Mutex;

/// Slots in the table that finds a glyph's record without walking the arena.
const HINTS: usize = 128;

/// Words in a record before its contour ends: the glyph and its pin count, the record's length,
/// the seven `OutlineInfo` floats, and the number of contours.
const HEADER: usize = 10;

/// A font whose glyphs are kept, once decoded, in a word arena the caller owns: outline, bounds
/// and advances, in the font's own units, so one entry serves every px size and transform. Metrics
/// and drawing both read it, and a miss in either fills it. Output is byte-identical to the
/// wrapped font's.
///
/// Glyphs are packed into the arena in the order they were first asked for, and the oldest are
/// evicted to make room. A glyph larger than the arena is drawn without being kept. Each lookup
/// takes a critical section; drawing happens outside it, from an entry pinned against eviction
/// while it is read. A glyph that would have to evict a pinned entry is drawn without being kept.
///
/// A glyph takes 10 words, one per contour, and two per point.
pub struct Cached<'c, F> {
    font: F,
    words: *mut u32,
    len: usize,
    ring: Mutex<RefCell<Ring>>,
    _words: PhantomData<&'c mut [u32]>,
}

// SAFETY: the arena is written only inside a critical section, and only where no pinned record
// lies. A pinned record is read outside it, and nothing writes it until its pin is released, also
// inside a critical section, which orders the reads before any later write.
unsafe impl<F: Sync> Sync for Cached<'_, F> {}
// SAFETY: the arena pointer comes from a `&mut [u32]`, which is `Send`.
unsafe impl<F: Send> Send for Cached<'_, F> {}

/// Where the records lie. Unwrapped, they fill `tail..head`. Wrapped, they fill `tail..wrap` and
/// then `0..head`, with `head <= tail`.
struct Ring {
    tail: usize,
    head: usize,
    wrap: usize,
    wrapped: bool,
    count: usize,
    /// One past the offset of a record for a glyph whose index is the slot's modulo `HINTS`, or
    /// zero. A slot is cleared when its record is evicted, so it only ever names a live record,
    /// though that record may be another glyph's with the same slot.
    hints: [u32; HINTS],
}

impl Ring {
    const EMPTY: Ring = Ring {
        tail: 0,
        head: 0,
        wrap: 0,
        wrapped: false,
        count: 0,
        hints: [0; HINTS],
    };

    fn hint(&mut self, index: u16) -> &mut u32 {
        &mut self.hints[index as usize % HINTS]
    }
}

/// A glyph as a record stores it, decoded but not yet stored.
struct Decoded {
    info: OutlineInfo,
    points: Vec<[f32; 2]>,
    contours: Vec<u32>,
}

impl Decoded {
    fn words(&self) -> usize {
        HEADER + self.contours.len() + 2 * self.points.len()
    }

    fn glyph(&self) -> GlyphRef<'_> {
        glyph_ref(self.info, &self.points, &self.contours)
    }
}

/// Stored points as a glyph, with `info`'s bounds.
fn glyph_ref<'a>(info: OutlineInfo, points: &'a [[f32; 2]], contours: &'a [u32]) -> GlyphRef<'a> {
    // SAFETY: every caller's points and info were copied from one `GlyphRef`, whose construction
    // promised every point inside its bounds with no coordinate between zero and
    // `SMALLEST_COORDINATE`. The unit carries over in `info`, which `metrics_raw` and the draws
    // read from the `GlyphRef`, not from `PathGlyph`.
    let glyph =
        unsafe { PathGlyph::new(points, contours, info.bounds, info.advance_width, info.advance_height) };
    GlyphRef::from(glyph).with_info(info)
}

impl<'c, F: FontRepr> Cached<'c, F> {
    /// Keeps `font`'s glyphs in `words`, whose contents it ignores.
    pub fn new(font: F, words: &'c mut [u32]) -> Self {
        Cached {
            font,
            // Offsets, and one past them, fit a word.
            len: words.len().min(u32::MAX as usize - 1),
            words: words.as_mut_ptr(),
            ring: Mutex::new(RefCell::new(Ring::EMPTY)),
            _words: PhantomData,
        }
    }

    pub fn font(&self) -> &F {
        &self.font
    }

    /// The wrapped font, giving the arena back.
    pub fn into_inner(self) -> F {
        self.font
    }

    /// Calls `f` with the glyph at `index`, from the arena when it is there, and keeps it there if
    /// it was not and it fits.
    fn with_entry<R>(&self, index: u16, f: impl for<'g> FnOnce(&GlyphRef<'g>) -> R) -> R {
        let found = critical_section::with(|cs| {
            let ring = &mut self.ring.borrow_ref_mut(cs);
            self.find(ring, index).filter(|&at| self.pin(at))
        });
        if let Some(at) = found {
            let _pin = Pin(self, at);
            return f(&self.record(at));
        }
        let decoded = self.decode(index);
        let stored = critical_section::with(|cs| {
            let ring = &mut self.ring.borrow_ref_mut(cs);
            let at = match self.find(ring, index) {
                Some(at) => at,
                None => {
                    let at = self.reserve(ring, decoded.words())?;
                    self.write(at, index, &decoded);
                    *ring.hint(index) = at as u32 + 1;
                    at
                }
            };
            self.pin(at).then_some(at)
        });
        match stored {
            Some(at) => {
                let _pin = Pin(self, at);
                f(&self.record(at))
            }
            None => f(&decoded.glyph()),
        }
    }

    /// The wrapped font's glyph, copied out.
    fn decode(&self, index: u16) -> Decoded {
        let mut decoded = None;
        self.font.with_glyph_at_index(index, &mut |glyph| {
            let (mut points, mut contours) = (Vec::new(), Vec::new());
            glyph.visit(|event| match event {
                PathEvent::MoveTo(p) => {
                    if !points.is_empty() {
                        contours.push(points.len() as u32);
                    }
                    points.push(p);
                }
                PathEvent::LineTo(p) => {
                    // A segment before any `MoveTo` starts at the origin, as the draws start.
                    if points.is_empty() {
                        points.push([0.0; 2]);
                    }
                    points.push(p);
                }
            });
            if !points.is_empty() {
                contours.push(points.len() as u32);
            }
            decoded = Some(Decoded {
                info: glyph.info(),
                points,
                contours,
            });
        });
        decoded.expect("with_glyph_at_index calls its closure")
    }

    #[inline]
    fn word(&self, at: usize) -> u32 {
        debug_assert!(at < self.len);
        // SAFETY: `at` is inside the arena. See `Sync` for why the read does not race a write.
        unsafe { self.words.add(at).read() }
    }

    #[inline]
    fn set_word(&self, at: usize, value: u32) {
        debug_assert!(at < self.len);
        // SAFETY: as in `word`; only called inside a critical section, on unpinned records.
        unsafe { self.words.add(at).write(value) }
    }

    /// The record for glyph `index`. In a critical section.
    fn find(&self, ring: &mut Ring, index: u16) -> Option<usize> {
        let hint = *ring.hint(index);
        if hint != 0 && self.word(hint as usize - 1) as u16 == index {
            return Some(hint as usize - 1);
        }
        let mut at = ring.tail;
        for _ in 0..ring.count {
            if ring.wrapped && at == ring.wrap {
                at = 0;
            }
            if self.word(at) as u16 == index {
                *ring.hint(index) = at as u32 + 1;
                return Some(at);
            }
            at += self.word(at + 1) as usize;
        }
        None
    }

    /// Pins the record at `at`, unless it already has as many pins as the count holds. In a
    /// critical section.
    fn pin(&self, at: usize) -> bool {
        let word = self.word(at);
        let pinned = word >> 16 < u16::MAX as u32;
        if pinned {
            self.set_word(at, word + (1 << 16));
        }
        pinned
    }

    /// Evicts the oldest record, unless it is pinned. In a critical section. A reservation that
    /// then fails keeps the evictions: the records are gone, and the ring is consistent.
    fn evict(&self, ring: &mut Ring) -> bool {
        if ring.count == 0 || self.word(ring.tail) >> 16 != 0 {
            return false;
        }
        let tail = ring.tail;
        let hint = ring.hint(self.word(tail) as u16);
        if *hint == tail as u32 + 1 {
            *hint = 0;
        }
        ring.tail += self.word(ring.tail + 1) as usize;
        ring.count -= 1;
        if ring.count == 0 {
            // Every hint was cleared as its record went.
            (ring.tail, ring.head, ring.wrapped) = (0, 0, false);
        } else if ring.wrapped && ring.tail == ring.wrap {
            ring.tail = 0;
            ring.wrapped = false;
        }
        true
    }

    /// Room for a record of `need` words, evicting the oldest records to make it. In a critical
    /// section.
    fn reserve(&self, ring: &mut Ring, need: usize) -> Option<usize> {
        if need > self.len {
            return None;
        }
        loop {
            if !ring.wrapped {
                if ring.head + need <= self.len {
                    break;
                }
                ring.wrap = ring.head;
                ring.head = 0;
                ring.wrapped = true;
            } else if ring.head + need <= ring.tail {
                break;
            }
            if !self.evict(ring) {
                return None;
            }
        }
        let at = ring.head;
        ring.head += need;
        ring.count += 1;
        Some(at)
    }

    /// Stores `decoded` as glyph `index` at `at`, unpinned. In a critical section.
    fn write(&self, at: usize, index: u16, decoded: &Decoded) {
        let info = decoded.info;
        let b = info.bounds;
        let header = [
            index as u32,
            decoded.words() as u32,
            b.xmin.to_bits(),
            b.ymin.to_bits(),
            b.width.to_bits(),
            b.height.to_bits(),
            info.unit.to_bits(),
            info.advance_width.to_bits(),
            info.advance_height.to_bits(),
            decoded.contours.len() as u32,
        ];
        let body = decoded.contours.iter().copied();
        let points = decoded.points.iter().flat_map(|p| [p[0].to_bits(), p[1].to_bits()]);
        for (i, word) in header.into_iter().chain(body).chain(points).enumerate() {
            self.set_word(at + i, word);
        }
    }

    /// The glyph in the record at `at`, which the caller keeps pinned while it uses it.
    fn record(&self, at: usize) -> GlyphRef<'_> {
        let float = |i: usize| f32::from_bits(self.word(at + i));
        let info = OutlineInfo {
            bounds: OutlineBounds {
                xmin: float(2),
                ymin: float(3),
                width: float(4),
                height: float(5),
            },
            unit: float(6),
            advance_width: float(7),
            advance_height: float(8),
        };
        let contours = self.word(at + 9) as usize;
        let points = (self.word(at + 1) as usize - HEADER - contours) / 2;
        // SAFETY: `write` put `contours` ends and then `points` pairs of floats in the record, which
        // lies inside the arena. `u32` and `[f32; 2]` both have alignment 4, and the arena is
        // aligned for `u32`. Nothing writes a pinned record.
        let (contours, points) = unsafe {
            let ends = self.words.add(at + HEADER);
            (
                core::slice::from_raw_parts(ends as *const u32, contours),
                core::slice::from_raw_parts(ends.add(contours) as *const [f32; 2], points),
            )
        };
        glyph_ref(info, points, contours)
    }
}

/// Releases a record's pin when dropped.
struct Pin<'a, 'c, F: FontRepr>(&'a Cached<'c, F>, usize);

impl<F: FontRepr> Drop for Pin<'_, '_, F> {
    fn drop(&mut self) {
        critical_section::with(|_| self.0.set_word(self.1, self.0.word(self.1) - (1 << 16)));
    }
}

// SAFETY: every glyph is a copy of one the wrapped font returned, points and info together, and the
// same glyph has the same info whether it comes from the arena or is decoded again.
unsafe impl<F: FontRepr> OutlineSource for Cached<'_, F> {
    fn info(&self, glyph: u16) -> OutlineInfo {
        self.with_entry(glyph, |g| g.info())
    }

    fn visit(&self, glyph: u16, f: &mut dyn FnMut(PathEvent)) {
        self.with_entry(glyph, |g| g.visit(f))
    }
}

impl<F: FontRepr> FontRepr for Cached<'_, F> {
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

    fn scale_factor(&self, px: f32) -> f32 {
        self.font.scale_factor(px)
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
        GlyphRef::from_source(self, index)
    }

    fn with_glyph_at_index(&self, index: u16, f: &mut dyn FnMut(&GlyphRef<'_>)) {
        self.with_entry(index, |g| f(g))
    }

    fn metrics_indexed(&self, index: u16, px: f32) -> Metrics {
        let scale = self.scale_factor(px);
        metrics_raw_stretched(scale, &self.with_entry(index, |g| g.info()), 0.0, 1.0).0
    }

    fn rasterize_indexed<'r>(
        &self,
        canvas: &'r mut Raster<'_>,
        index: u16,
        px: f32,
    ) -> (Metrics, BitmapIter<'r>) {
        self.upright(canvas, index, px, 1.0)
    }

    fn rasterize_indexed_subpixel<'r>(
        &self,
        canvas: &'r mut Raster<'_>,
        index: u16,
        px: f32,
    ) -> (Metrics, BitmapIter<'r>) {
        self.upright(canvas, index, px, 3.0)
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
        let metrics =
            self.with_entry(index, |g| crate::rasterize_transformed(canvas, g, scale, transform, pen));
        (metrics, canvas.get_bitmap_iter())
    }
}

impl<F: FontRepr> Cached<'_, F> {
    fn upright<'r>(
        &self,
        canvas: &'r mut Raster<'_>,
        index: u16,
        px: f32,
        stretch: f32,
    ) -> (Metrics, BitmapIter<'r>) {
        if px == 0.0 {
            canvas.resize(0, 0);
            return (Metrics::default(), canvas.get_bitmap_iter());
        }
        let scale = self.scale_factor(px);
        let metrics = self.with_entry(index, |g| crate::rasterize_inner(canvas, g, scale, stretch));
        (metrics, canvas.get_bitmap_iter())
    }
}
