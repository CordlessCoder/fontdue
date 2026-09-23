//! The compressed line store: a format for baked glyph outlines, a streaming decoder that needs
//! neither `std` nor `alloc`, and its encoder, which the macro runs.
//!
//! The decoder yields a glyph's line segments as `[x0, y0, x1, y1]` in units of
//! [`Store::unit`] font units, relative to the glyph's bounding box with y growing down, which is
//! the frame fontdue's `Line`s use. The unit is a power of two, so a consumer folds it into its
//! draw scale exactly. It holds no buffer: each segment is decoded from flash as it is requested.
//!
//! # Format (version 5)
//!
//! The whole blob is 32-bit little-endian words and must start 4-byte aligned in memory, which
//! [`include_store!`] arranges.
//!
//! ```text
//! word   version | grid_shift << 8 | id_bits << 16 | xy_bits << 24
//! word   glyph_count | pool_count << 16
//! step model:
//!   word               n, the number of symbols
//!   8 words            codes of each length 0..=15, as u16 pairs, low half first
//!   ceil(n / 2) words  per symbol in canonical order, u16: `StepTable::bits`
//!   3 * 2^FAST_BITS    lookahead table, three words per entry: see `StepTable`
//!     words
//! pool_count words     bit offset of each pool entry in the stream | its step count << 21
//! glyph_count x 5      bit offset of the glyph's record | its placement count << 24; f32 xmin
//!                      and ymin in grid units; width | height << 16 in grid units; f32 advance
//!                      width in font units
//! word                 stream length in words, then the stream, ending in two zero words
//! ```
//!
//! A pool entry is one contour shape: its steps, each relative to the previous point, with the
//! first point implicit at the origin. A step is one symbol of the step model, which names the
//! sign and bit length of dx and of dy, then dx's raw bits, then dy's. A glyph record is its
//! placements, each a pool id in `id_bits` raw bits and a start point in two `xy_bits` raw fields.
//!
//! A value `v` of bit length `b = bits(|v|)` sends `m = b - 1` raw bits (none for zero), and
//! `v = base + raw`: `base` is `2^(b-1)` when `v` is positive and `1 - 2^b` when it is negative,
//! so the raw bits of a negative value are `v - base`. The stream runs from the most significant
//! bit of each word to the least. The step model's rarest symbols may share an escape symbol,
//! which is followed by dx and dy as `ESCAPE_BITS`-bit two's complement fields.

#[doc(hidden)]
pub mod encode;

pub const MAX_CODE_LEN: usize = 15;
/// Bit lengths run from 0 to `MAX_BITS`.
pub const MAX_SYMBOLS: usize = MAX_BITS as usize + 1;
/// Longest zigzagged dx or dy.
pub const MAX_BITS: u32 = 24;
pub const VERSION: u8 = 5;
/// Bits of lookahead in the step table.
pub const FAST_BITS: u32 = 8;
const FAST_SIZE: usize = 1 << FAST_BITS;
/// The stream ends in this many zero words, so the second word of any read stays inside it.
pub const PADDING_WORDS: usize = 2;
const GLYPH_WORDS: usize = 5;
const STEP_ENTRY_WORDS: usize = 3;
/// Set in a step entry's third word when the step needs `slow_step`: a code longer than
/// `FAST_BITS`, or a step whose code and raw bits do not fit one 32-bit peek.
pub const STEP_SLOW: u32 = 1 << 31;
/// The step symbol whose values follow as raw fields.
pub const STEP_ESCAPE: u16 = 0xffff;
/// Width of an escaped value.
pub const ESCAPE_BITS: u32 = MAX_BITS + 1;

/// The base a value of bit length `b` and sign `negative` adds its raw bits to.
pub fn step_base(b: u32, negative: bool) -> i32 {
    match (b, negative) {
        (0, _) => 0,
        (b, false) => 1 << (b - 1),
        (b, true) => 1 - (1 << b),
    }
}
/// Pool words keep a shape's bit offset below this bit and its step count above it.
pub const POOL_SHIFT: u32 = 21;
/// Glyph words keep the record's bit offset below this bit and its placement count above it.
pub const GLYPH_SHIFT: u32 = 24;

#[derive(Debug, PartialEq)]
pub enum Error {
    Truncated,
    Version,
    BadModel,
    /// The blob does not start 4-byte aligned in memory, or is not a whole number of words.
    Misaligned,
    /// Decoding some glyph would read outside the stream or use a pool id past the pool.
    Corrupt,
}

/// Embeds a store file with the 4-byte alignment [`Store::new`] requires, as a `&'static [u8]`.
#[macro_export]
macro_rules! include_store {
    ($path:expr) => {{
        #[repr(C, align(4))]
        struct Aligned<T: ?Sized>(T);
        static STORE: &Aligned<[u8]> = &Aligned(*include_bytes!($path));
        &STORE.0
    }};
}

/// The step model's tables. Each lookahead entry is three words, chosen so a step decodes from one
/// peek `w` with shifts whose amounts come straight from the entry:
///
/// - `e[v]` for v in dx, dy: `base << 5 | m`. The value is `((w << off) >> (32 - m)) + base`,
///   zero raw bits when `m` is 0.
/// - `c`: `len | (len + m_dx) << 8 | (len + m_dx + m_dy) << 16`, the offsets of dx's and dy's
///   raw bits and the whole step's length, so `c >> 16` is the length; or [`STEP_SLOW`].
///
/// `parse` checks every fast entry: `1 <= len <= FAST_BITS`, `len + m_dx <= 31`, a length of at
/// most 32, and `m < MAX_BITS`.
#[derive(Clone, Copy)]
struct StepTable<'a> {
    counts: [u16; MAX_CODE_LEN + 1],
    /// Each symbol in canonical order: dx's bit length, then its sign at bit 5, then dy's the same
    /// way from bit 8; or [`STEP_ESCAPE`].
    bits: &'a [u16],
    fast: &'a [u32],
}

impl StepTable<'_> {
    /// The step's symbol index and code length at the top of `w`, walked a bit at a time.
    fn walk(&self, w: u32) -> (u32, u32) {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..=MAX_CODE_LEN as u32 {
            code |= ((w >> (32 - len)) & 1) as i32;
            let count = self.counts[len as usize] as i32;
            if code - first < count {
                return (self.bits[(index + code - first) as usize] as u32, len);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        (0, MAX_CODE_LEN as u32)
    }
}

/// How the decoder reads the stream. [`Checked`] bounds-checks every read and records one that
/// would leave the stream; [`Store::new`] runs every glyph through it once. [`Trusted`] reads
/// without checks, which is sound only on a store that walk accepted.
///
/// Bit positions wrap at 2^32 on corrupt input rather than panicking. The checked walk and the
/// trusted decode compute the same wrapped positions, so the trusted reads are exactly the ones
/// the walk checked.
pub trait Mode: sealed::Sealed {
    #[doc(hidden)]
    const TRUSTED: bool;
    #[doc(hidden)]
    fn word(&self, words: &[u32], i: usize) -> u32;
    #[doc(hidden)]
    fn require(&self, ok: bool);
    #[doc(hidden)]
    fn failed(&self) -> bool;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Checked {}
    impl Sealed for super::Trusted {}
    impl<M: Sealed> Sealed for &M {}
}

pub struct Checked(core::cell::Cell<bool>);

impl Mode for Checked {
    const TRUSTED: bool = false;

    #[inline(always)]
    fn word(&self, words: &[u32], i: usize) -> u32 {
        match words.get(i) {
            Some(&w) => w,
            None => {
                self.0.set(true);
                0
            }
        }
    }

    fn require(&self, ok: bool) {
        if !ok {
            self.0.set(true);
        }
    }

    fn failed(&self) -> bool {
        self.0.get()
    }
}

#[derive(Clone, Copy)]
pub struct Trusted;

impl Mode for Trusted {
    const TRUSTED: bool = true;

    #[inline(always)]
    fn word(&self, words: &[u32], i: usize) -> u32 {
        debug_assert!(i < words.len());
        // SAFETY: a `Store` built by `new` has decoded every glyph under `Checked`, which reads
        // exactly the words this decode reads, since decoding is deterministic over immutable
        // data. `new_unchecked` moves that obligation to its caller.
        unsafe { *words.get_unchecked(i) }
    }

    #[inline(always)]
    fn require(&self, ok: bool) {
        debug_assert!(ok);
    }

    #[inline(always)]
    fn failed(&self) -> bool {
        false
    }
}

impl<M: Mode> Mode for &M {
    const TRUSTED: bool = M::TRUSTED;

    #[inline(always)]
    fn word(&self, words: &[u32], i: usize) -> u32 {
        (**self).word(words, i)
    }

    #[inline(always)]
    fn require(&self, ok: bool) {
        (**self).require(ok)
    }

    #[inline(always)]
    fn failed(&self) -> bool {
        (**self).failed()
    }
}

/// The 32 bits of the stream starting at bit `pos`.
#[inline(always)]
fn peek<M: Mode>(mode: &M, words: &[u32], pos: u32) -> u32 {
    // LLVM addresses the word as `(pos >> 3) & !3`, with the mask from the literal pool.
    #[cfg(target_arch = "xtensa")]
    if M::TRUSTED {
        let w: u32;
        // SAFETY: the reads `mode.word` makes below, which `Trusted` makes sound.
        unsafe {
            core::arch::asm!(
                "srli  {k}, {pos}, 5",
                "addx4 {k}, {k}, {words}",
                "l32i  {w}, {k}, 0",
                "l32i  {k}, {k}, 4",
                "ssl   {pos}",
                "src   {w}, {w}, {k}",
                pos = in(reg) pos,
                words = in(reg) words.as_ptr(),
                k = out(reg) _,
                w = out(reg) w,
                out("sar") _,
                options(pure, readonly, nostack),
            );
        }
        return w;
    }
    let k = (pos >> 5) as usize;
    let hi = mode.word(words, k);
    let lo = mode.word(words, k + 1);
    let sh = pos & 31;
    // SAFETY: both shift amounts are below 32, since `sh` is. Shifting `lo` right by one first
    // makes `sh == 0` contribute nothing from it.
    unsafe { hi.unchecked_shl(sh) | (lo >> 1).unchecked_shr(31 - sh) }
}

/// A step whose entry is marked slow. Out of line so the hot loop keeps its registers.
#[inline(never)]
#[cold]
fn slow_step<M: Mode>(mode: &M, words: &[u32], pos: u32, t: &StepTable<'_>) -> (i32, i32, u32) {
    let (sym, len) = t.walk(peek(mode, words, pos));
    let pos = pos.wrapping_add(len);
    if sym == STEP_ESCAPE as u32 {
        let field = |pos: u32| (peek(mode, words, pos) as i32) >> (32 - ESCAPE_BITS);
        let dx = field(pos);
        let pos = pos.wrapping_add(ESCAPE_BITS);
        return (dx, field(pos), pos.wrapping_add(ESCAPE_BITS));
    }
    // `parse` bounds both bit lengths by `MAX_BITS`, so the shifts are below 32.
    let value = |pos: u32, sym: u32| {
        let (b, m) = (sym & 31, (sym & 31).saturating_sub(1));
        (((peek(mode, words, pos) >> 1) >> (31 - m)) as i32 + step_base(b, sym & 32 != 0), m)
    };
    let (dx, mx) = value(pos, sym & 0xff);
    let pos = pos.wrapping_add(mx);
    let (dy, my) = value(pos, sym >> 8);
    (dx, dy, pos.wrapping_add(my))
}

/// One step's dx and dy, and the position after it.
#[inline(always)]
fn step<M: Mode>(mode: &M, words: &[u32], pos: u32, t: &StepTable<'_>) -> (i32, i32, u32) {
    #[cfg(target_arch = "xtensa")]
    if M::TRUSTED {
        let (dx, dy, c): (i32, i32, u32);
        // SAFETY: the same reads as `peek` and the same entry as below; `Trusted` makes the
        // word reads sound, and the index is 8 bits into a table of `FAST_SIZE` entries. `ssl`
        // reads the low five bits of its operand, which is what the entry layout relies on, and
        // `ssl` of 0 makes `srl` shift by 32, to zero.
        unsafe {
            core::arch::asm!(
                "srli  {k}, {pos}, 5",
                "addx4 {k}, {k}, {words}",
                "l32i  {t}, {k}, 0",
                "l32i  {k}, {k}, 4",
                "ssl   {pos}",
                "src   {w}, {t}, {k}",
                "extui {k}, {w}, 24, 8",
                "addx2 {k}, {k}, {k}",
                "addx4 {k}, {k}, {fast}",
                "l32i  {c}, {k}, 8",
                "l32i  {dx}, {k}, 0",
                "l32i  {dy}, {k}, 4",
                "ssl   {c}",
                "sll   {t}, {w}",
                "ssl   {dx}",
                "srl   {t}, {t}",
                "srai  {dx}, {dx}, 5",
                "add   {dx}, {dx}, {t}",
                "srli  {k}, {c}, 8",
                "ssl   {k}",
                "sll   {t}, {w}",
                "ssl   {dy}",
                "srl   {t}, {t}",
                "srai  {dy}, {dy}, 5",
                "add   {dy}, {dy}, {t}",
                pos = in(reg) pos,
                words = in(reg) words.as_ptr(),
                fast = in(reg) t.fast.as_ptr(),
                k = out(reg) _,
                t = out(reg) _,
                w = out(reg) _,
                c = out(reg) c,
                dx = out(reg) dx,
                dy = out(reg) dy,
                out("sar") _,
                options(pure, readonly, nostack),
            );
        }
        if c & STEP_SLOW != 0 {
            return slow_step(mode, words, pos, t);
        }
        return (dx, dy, pos.wrapping_add(c >> 16));
    }
    let w = peek(mode, words, pos);
    // SAFETY: `fast` has `STEP_ENTRY_WORDS * FAST_SIZE` entries, checked by `parse`, and the
    // index is the top `FAST_BITS` bits of `w`.
    let e = unsafe { t.fast.as_ptr().add(STEP_ENTRY_WORDS * (w >> (32 - FAST_BITS)) as usize) };
    let (ex, ey, c) = unsafe { (*e, *e.add(1), *e.add(2)) };
    if c & STEP_SLOW != 0 {
        return slow_step(mode, words, pos, t);
    }
    // SAFETY: `parse` checked the entry: both offsets are below 32, and `e & 31` is below
    // `MAX_BITS`.
    let value = |off: u32, e: u32| unsafe {
        (w.unchecked_shl(off) >> 1).unchecked_shr(31 - (e & 31)) as i32 + (e as i32 >> 5)
    };
    (value(c & 31, ex), value((c >> 8) & 31, ey), pos.wrapping_add(c >> 16))
}

/// Bounding box of a glyph's points, in font units, with the same meaning as fontdue's
/// `OutlineBounds`: what the encoder takes. The store keeps it in grid units, and every decoded
/// point lies inside that.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub xmin: f32,
    pub ymin: f32,
    pub width: f32,
    pub height: f32,
}

/// A parsed store. Everything is read in place from the blob; parsing only checks it and records
/// where each part starts.
pub struct Store<'a> {
    grid: f32,
    id_bits: u32,
    xy_bits: u32,
    glyph_count: u16,
    pool_count: u16,
    steps: StepTable<'a>,
    pool_offsets: &'a [u32],
    glyphs: &'a [u32],
    words: &'a [u32],
}

/// A store's tables, as [`Store::parts`] returns them and [`Store::from_parts`] takes them. The
/// macro checks a store with [`Store::new`] at compile time and emits these as statics, so the
/// device neither parses nor walks it.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct Parts<'a> {
    pub grid: f32,
    pub id_bits: u32,
    pub xy_bits: u32,
    pub glyph_count: u16,
    pub pool_count: u16,
    pub step_counts: [u16; MAX_CODE_LEN + 1],
    pub step_bits: &'a [u16],
    pub step_fast: &'a [u32],
    pub pool_offsets: &'a [u32],
    pub glyphs: &'a [u32],
    pub words: &'a [u32],
}

fn take<'a>(data: &mut &'a [u32], n: usize) -> Result<&'a [u32], Error> {
    if data.len() < n {
        return Err(Error::Truncated);
    }
    let (head, rest) = data.split_at(n);
    *data = rest;
    Ok(head)
}

impl<'a> Store<'a> {
    /// Parses a store and decodes every glyph once with checked reads. Rejects a store any of
    /// whose reads would leave the stream, or any of whose points would leave its glyph's bounds.
    /// That is what makes the unchecked reads in [`lines`] and drawing the store sound. The walk
    /// costs one decode of the whole font.
    ///
    /// [`lines`]: Store::lines
    pub fn new(data: &'a [u8]) -> Result<Self, Error> {
        let store = Self::parse(data)?;
        let mode = Checked(core::cell::Cell::new(false));
        for g in 0..store.glyph_count {
            for _ in store.lines_in(&mode, g) {}
            if mode.failed() {
                return Err(Error::Corrupt);
            }
        }
        Ok(store)
    }

    /// Parses and checks a store's tables without the decoding walk in [`new`](Store::new).
    ///
    /// # Safety
    ///
    /// `data` must be a store written by this crate's encoder and not modified since. The decoder
    /// reads the stream without bounds checks, and the raster writes each decoded point without
    /// bounds checks, so a corrupt stream is undefined behaviour.
    pub unsafe fn new_unchecked(data: &'a [u8]) -> Result<Self, Error> {
        Self::parse(data)
    }

    #[doc(hidden)]
    pub fn parts(&self) -> Parts<'a> {
        Parts {
            grid: self.grid,
            id_bits: self.id_bits,
            xy_bits: self.xy_bits,
            glyph_count: self.glyph_count,
            pool_count: self.pool_count,
            step_counts: self.steps.counts,
            step_bits: self.steps.bits,
            step_fast: self.steps.fast,
            pool_offsets: self.pool_offsets,
            glyphs: self.glyphs,
            words: self.words,
        }
    }

    /// A store from its parts, with no checks at all.
    ///
    /// # Safety
    ///
    /// `parts` must equal what [`parts`](Store::parts) returned for a store that
    /// [`new`](Store::new) accepted, slice contents included. That is what the macro emits.
    #[doc(hidden)]
    pub const unsafe fn from_parts(parts: Parts<'a>) -> Self {
        Store {
            grid: parts.grid,
            id_bits: parts.id_bits,
            xy_bits: parts.xy_bits,
            glyph_count: parts.glyph_count,
            pool_count: parts.pool_count,
            steps: StepTable {
                counts: parts.step_counts,
                bits: parts.step_bits,
                fast: parts.step_fast,
            },
            pool_offsets: parts.pool_offsets,
            glyphs: parts.glyphs,
            words: parts.words,
        }
    }

    fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        // SAFETY: every bit pattern is a valid u32. The checks below reject a blob that is not
        // 4-byte aligned or not a whole number of words, so the middle slice is all of it.
        let (prefix, mut data, suffix) = unsafe { bytes.align_to::<u32>() };
        if !prefix.is_empty() || !suffix.is_empty() {
            return Err(Error::Misaligned);
        }
        let head = take(&mut data, 2)?;
        let [version, grid_shift, id_bits, xy_bits] = head[0].to_le_bytes();
        if version != VERSION {
            return Err(Error::Version);
        }
        let (id_bits, xy_bits) = (id_bits as u32, xy_bits as u32);
        if !(1..=16).contains(&id_bits) || !(1..=16).contains(&xy_bits) || grid_shift > 16 {
            return Err(Error::BadModel);
        }
        let glyph_count = head[1] as u16;
        let pool_count = (head[1] >> 16) as u16;

        let mut steps = StepTable {
            counts: [0; MAX_CODE_LEN + 1],
            bits: &[],
            fast: &[],
        };
        let n = take(&mut data, 1)?[0] as usize;
        if n == 0 || n > MAX_SYMBOLS * MAX_SYMBOLS {
            return Err(Error::BadModel);
        }
        let counts = take(&mut data, 8)?;
        for (i, c) in steps.counts.iter_mut().enumerate() {
            *c = (counts[i / 2] >> (16 * (i % 2))) as u16;
        }
        if steps.counts[0] != 0 || steps.counts.iter().map(|&c| c as usize).sum::<usize>() != n {
            return Err(Error::BadModel);
        }
        let bits = take(&mut data, n.div_ceil(2))?;
        // SAFETY: u16 has no invalid bit patterns and u32 alignment covers u16's.
        let (_, bits, _) = unsafe { bits.align_to::<u16>() };
        steps.bits = &bits[..n];
        if steps.bits.iter().any(|&b| {
            b != STEP_ESCAPE
                && (b & 0xc0c0 != 0 || (b & 31) as u32 > MAX_BITS || ((b >> 8) & 31) as u32 > MAX_BITS)
        }) {
            return Err(Error::BadModel);
        }
        let fast = take(&mut data, STEP_ENTRY_WORDS * FAST_SIZE)?;
        let entry_ok = |e: &[u32]| {
            let c = e[2];
            if c == STEP_SLOW {
                return true;
            }
            let (len, oy, total) = (c & 0xff, (c >> 8) & 0xff, c >> 16);
            let (mx, my) = (e[0] & 31, e[1] & 31);
            mx < MAX_BITS
                && my < MAX_BITS
                && (1..=FAST_BITS).contains(&len)
                && oy == len + mx
                && oy <= 31
                && total == oy + my
                && total <= 32
        };
        if !fast.chunks(STEP_ENTRY_WORDS).all(entry_ok) {
            return Err(Error::BadModel);
        }
        steps.fast = fast;
        let pool_offsets = take(&mut data, pool_count as usize)?;
        let glyphs = take(&mut data, GLYPH_WORDS * glyph_count as usize)?;
        let stream_words = take(&mut data, 1)?[0] as usize;
        let words = take(&mut data, stream_words)?;
        Ok(Store {
            grid: 1.0 / (1u32 << grid_shift) as f32,
            id_bits,
            xy_bits,
            glyph_count,
            pool_count,
            steps,
            pool_offsets,
            glyphs,
            words,
        })
    }

    /// The size of one decoded coordinate step in font units, a power of two.
    pub fn unit(&self) -> f32 {
        self.grid
    }

    pub fn glyph_count(&self) -> u16 {
        self.glyph_count
    }

    pub fn pool_count(&self) -> u16 {
        self.pool_count
    }

    /// The glyph's record words after the stream offset, or zeros for an index past the end.
    #[inline(always)]
    fn record(&self, glyph: u16) -> [u32; GLYPH_WORDS - 1] {
        let at = GLYPH_WORDS * glyph as usize;
        let w = |i: usize| self.glyphs.get(at + 1 + i).copied().unwrap_or(0);
        [w(0), w(1), w(2), w(3)]
    }

    /// The glyph's bounds in grid units, which is what its points are in.
    pub fn grid_bounds(&self, glyph: u16) -> crate::OutlineBounds {
        let [xmin, ymin, size, _] = self.record(glyph);
        crate::OutlineBounds {
            xmin: f32::from_bits(xmin),
            ymin: f32::from_bits(ymin),
            width: (size & 0xffff) as f32,
            height: (size >> 16) as f32,
        }
    }

    /// The glyph's bounds in font units.
    pub fn bounds(&self, glyph: u16) -> Bounds {
        let b = self.grid_bounds(glyph);
        Bounds {
            xmin: b.xmin * self.grid,
            ymin: b.ymin * self.grid,
            width: b.width * self.grid,
            height: b.height * self.grid,
        }
    }

    pub fn advance_width(&self, glyph: u16) -> f32 {
        f32::from_bits(self.record(glyph)[3])
    }

    /// The glyph's segments, in contour order. Segments with no vertical extent are skipped, as
    /// fontdue's `Geometry::push` skips them. A glyph index past the end yields nothing.
    #[inline]
    pub fn lines(&self, glyph: u16) -> Lines<'_, 'a, Trusted> {
        self.lines_in(Trusted, glyph)
    }

    #[inline(always)]
    fn lines_in<M: Mode>(&self, mode: M, glyph: u16) -> Lines<'_, 'a, M> {
        let (placements, record) = match self.glyphs.get(GLYPH_WORDS * glyph as usize) {
            Some(&w) if glyph < self.glyph_count => (w >> GLYPH_SHIFT, w & ((1 << GLYPH_SHIFT) - 1)),
            _ => (0, 0),
        };
        // Read only by the checked walk.
        let size = if M::TRUSTED {
            0
        } else {
            self.record(glyph)[2]
        };
        Lines {
            store: self,
            mode,
            width: size & 0xffff,
            height: size >> 16,
            record,
            placements,
            shape: 0,
            steps: 0,
            x: 0,
            y: 0,
            fx: 0.0,
            fy: 0.0,
        }
    }
}

// SAFETY: a `Store` built by `new` has decoded every glyph under `Checked`, which requires every
// point to lie inside the glyph's size in grid units, which is what `grid_bounds` returns. The
// trusted decode yields exactly those points. `new_unchecked` moves that obligation to its caller.
unsafe impl crate::OutlineSource for Store<'_> {
    #[inline]
    fn info(&self, glyph: u16) -> crate::OutlineInfo {
        crate::OutlineInfo {
            bounds: self.grid_bounds(glyph),
            unit: self.grid,
            advance_width: self.advance_width(glyph),
            advance_height: 0.0,
        }
    }

    #[inline]
    fn draw(&self, glyph: u16, sink: &mut crate::raster::Sink<'_, '_>) {
        for segment in self.lines(glyph) {
            sink.segment(segment);
        }
    }
}

// SAFETY: `segments` yields exactly what `draw` passes to the sink.
unsafe impl crate::SegmentSource for Store<'_> {
    type Segments<'s>
        = Lines<'s, 's, Trusted>
    where
        Self: 's;

    #[inline]
    fn segments(&self, glyph: u16) -> Self::Segments<'_> {
        self.lines(glyph)
    }
}

/// Streaming iterator over one glyph's segments. Its state is two bit positions and the current
/// point, kept both as grid integers and as the floats the last segment ended on.
pub struct Lines<'s, 'a, M> {
    store: &'s Store<'a>,
    mode: M,
    /// The glyph's size in grid units, which the checked walk holds every point to.
    width: u32,
    height: u32,
    record: u32,
    placements: u32,
    shape: u32,
    steps: u32,
    x: i32,
    y: i32,
    fx: f32,
    fy: f32,
}

impl<M: Mode> Iterator for Lines<'_, '_, M> {
    type Item = [f32; 4];

    #[inline(always)]
    fn next(&mut self) -> Option<[f32; 4]> {
        let s = self.store;
        let words = s.words;
        let steps_t = &s.steps;
        let (mut steps, mut shape, mut x, mut y) = (self.steps, self.shape, self.x, self.y);
        loop {
            while steps > 0 {
                steps -= 1;
                let (dx, dy, p) = step(&self.mode, words, shape, steps_t);
                shape = p;
                x = x.wrapping_add(dx);
                y = y.wrapping_add(dy);
                if !M::TRUSTED {
                    self.mode.require(x as u32 <= self.width && y as u32 <= self.height);
                }
                if dy != 0 {
                    let (fx, fy) = (x as f32, y as f32);
                    let seg = [self.fx, self.fy, fx, fy];
                    (self.fx, self.fy) = (fx, fy);
                    (self.steps, self.shape, self.x, self.y) = (steps, shape, x, y);
                    return Some(seg);
                }
                // A step with no vertical extent still moves the point; the next segment starts there.
                (self.fx, self.fy) = (x as f32, y as f32);
            }
            if self.placements == 0 || self.mode.failed() {
                (self.steps, self.shape, self.x, self.y) = (steps, shape, x, y);
                return None;
            }
            self.placements -= 1;
            let w = peek(&self.mode, words, self.record);
            let wy = peek(&self.mode, words, self.record.wrapping_add(s.id_bits + s.xy_bits));
            // SAFETY: `parse` checked `1 <= id_bits <= 16` and `1 <= xy_bits <= 16`, so every
            // shift is below 32.
            let (id, px, py) = unsafe {
                (
                    w.unchecked_shr(32 - s.id_bits) as usize,
                    w.unchecked_shl(s.id_bits).unchecked_shr(32 - s.xy_bits),
                    wy.unchecked_shr(32 - s.xy_bits),
                )
            };
            self.mode.require(id < s.pool_count as usize);
            self.record = self.record.wrapping_add(s.id_bits + 2 * s.xy_bits);
            (x, y) = (px as i32, py as i32);
            if !M::TRUSTED {
                self.mode.require(px <= self.width && py <= self.height);
            }
            (self.fx, self.fy) = (x as f32, y as f32);
            let pool = self.mode.word(s.pool_offsets, id);
            shape = pool & ((1 << POOL_SHIFT) - 1);
            steps = pool >> POOL_SHIFT;
        }
    }
}
