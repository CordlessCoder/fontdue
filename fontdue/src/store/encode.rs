//! Encoder for the format described in the parent module. The macro runs it at compile time.

use super::{
    Bounds, FAST_BITS, GLYPH_SHIFT, MAX_BITS, MAX_CODE_LEN, PADDING_WORDS, POOL_SHIFT, STEP_ESCAPE,
    STEP_SLOW, VERSION,
};
use crate::{Glyph, HashMap, HashSet};
use alloc::collections::BinaryHeap;
use alloc::vec;
use alloc::vec::Vec;

/// One glyph to encode: its contours as chains of integer points in units of `2^-grid_shift`,
/// its bounds in font units, whose size must be a whole number of grid units and contain every
/// point, and its advance width in font units.
pub struct GlyphInput {
    pub chains: Vec<Vec<(i32, i32)>>,
    pub bounds: Bounds,
    pub advance_width: f32,
}

use super::{ESCAPE_BITS, step_base};

/// A step value's half of its symbol, `b | negative << 5`, and its raw bits and their count.
fn step_value(v: i32) -> (u32, u32, u32) {
    let (b, _, _) = split(v.unsigned_abs());
    let raw = v.wrapping_sub(step_base(b, v < 0)) as u32;
    (b | ((v < 0) as u32) << 5, raw, b.saturating_sub(1))
}

/// `u`'s bit length, which is what the step model codes, and the raw bits below its leading one.
fn split(u: u32) -> (u32, u32, u32) {
    let b = 32 - u.leading_zeros();
    assert!(b <= MAX_BITS, "{u} is too large for the format");
    let m = b.saturating_sub(1);
    (b, m, u & ((1u32 << m) - 1))
}

struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    have: u32,
    len: u64,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter {
            bytes: Vec::new(),
            acc: 0,
            have: 0,
            len: 0,
        }
    }

    fn put(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1) as u64;
            self.have += 1;
            self.len += 1;
            if self.have == 8 {
                self.bytes.push(self.acc as u8);
                self.acc = 0;
                self.have = 0;
            }
        }
    }

    fn position(&self) -> u32 {
        self.len as u32
    }

    /// The stream as 32-bit words, most significant bit first, plus the zero padding words.
    fn finish(mut self) -> Vec<u32> {
        if self.have > 0 {
            self.bytes.push((self.acc << (8 - self.have)) as u8);
        }
        self.bytes.resize(self.bytes.len().next_multiple_of(4), 0);
        let mut words: Vec<u32> =
            self.bytes.chunks(4).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect();
        words.extend_from_slice(&[0; PADDING_WORDS]);
        words
    }
}

/// Code lengths for each symbol, limited to `limit` by halving frequencies until they fit.
fn code_lengths(freqs: &HashMap<u32, u64>, limit: u32) -> Vec<(u32, u32)> {
    let mut f: Vec<(u32, u64)> = freqs.iter().map(|(&s, &c)| (s, c)).collect();
    f.sort();
    if f.len() == 1 {
        return vec![(f[0].0, 1)];
    }
    loop {
        #[derive(PartialEq, Eq, PartialOrd, Ord)]
        struct Node(core::cmp::Reverse<u64>, usize);
        let mut heap: BinaryHeap<Node> = BinaryHeap::new();
        let mut parent: Vec<usize> = vec![usize::MAX; f.len()];
        for (i, &(_, c)) in f.iter().enumerate() {
            heap.push(Node(core::cmp::Reverse(c), i));
        }
        while heap.len() > 1 {
            let a = heap.pop().unwrap();
            let b = heap.pop().unwrap();
            let id = parent.len();
            parent.push(usize::MAX);
            parent[a.1] = id;
            parent[b.1] = id;
            heap.push(Node(core::cmp::Reverse(a.0.0 + b.0.0), id));
        }
        let lens: Vec<u32> = (0..f.len())
            .map(|mut i| {
                let mut d = 0;
                while parent[i] != usize::MAX {
                    i = parent[i];
                    d += 1;
                }
                d
            })
            .collect();
        if lens.iter().all(|&l| l <= limit) {
            return f.iter().zip(lens).map(|(&(s, _), l)| (s, l)).collect();
        }
        for e in &mut f {
            e.1 = (e.1 + 1) / 2;
        }
    }
}

/// A model as written: symbols in canonical order, and each symbol's (code, length, index).
struct Coded {
    ordered: Vec<(u32, u32)>,
    codes: HashMap<u32, (u32, u32, usize)>,
}

/// Canonical code assignment: symbols ordered by (length, symbol), codes increasing.
fn canonical(mut lens: Vec<(u32, u32)>) -> Coded {
    lens.sort_by_key(|&(s, l)| (l, s));
    let mut codes = HashMap::new();
    let mut code = 0u32;
    let mut prev = lens[0].1;
    for (i, &(s, l)) in lens.iter().enumerate() {
        code <<= l - prev;
        codes.insert(s, (code, l, i));
        code += 1;
        prev = l;
    }
    Coded {
        ordered: lens,
        codes,
    }
}

/// The step model's words: symbol count, code counts per length, each symbol, and the lookahead
/// table in `StepTable`'s layout.
fn step_words(c: &Coded) -> Vec<u32> {
    let mut out = vec![c.ordered.len() as u32];
    let mut counts = [0u16; MAX_CODE_LEN + 1];
    for &(_, l) in &c.ordered {
        counts[l as usize] += 1;
    }
    for pair in counts.chunks(2) {
        out.push(pair[0] as u32 | (pair[1] as u32) << 16);
    }
    let mut bits: Vec<u16> = c.ordered.iter().map(|&(s, _)| s as u16).collect();
    bits.resize(bits.len().next_multiple_of(2), 0);
    out.extend(bits.chunks(2).map(|q| q[0] as u32 | (q[1] as u32) << 16));
    let size = 1usize << FAST_BITS;
    let mut fast = vec![[0, 0, STEP_SLOW]; size];
    let half = |h: u32| {
        let (b, m) = (h & 31, (h & 31).saturating_sub(1));
        ((step_base(b, h & 32 != 0) << 5) as u32 | m, m)
    };
    for (&sym, &(code, len, _)) in &c.codes {
        if sym == STEP_ESCAPE as u32 {
            continue;
        }
        let ((ex, mx), (ey, my)) = (half(sym & 0xff), half(sym >> 8));
        if len <= FAST_BITS && len + mx <= 31 && len + mx + my <= 32 {
            let start = (code << (FAST_BITS - len)) as usize;
            let span = 1usize << (FAST_BITS - len);
            let c = len | (len + mx) << 8 | (len + mx + my) << 16;
            for e in &mut fast[start..start + span] {
                *e = [ex, ey, c];
            }
        }
    }
    out.extend(fast.iter().flatten());
    out
}

/// What a glyph's placements and the pool's shapes turn into, before coding.
enum Op {
    Step(i32, i32),
    Raw(u32, u32),
    /// Marks the start of pool entry `n`.
    PoolStart(usize),
    /// Marks the start of glyph `n`'s record.
    GlyphStart(usize),
}

/// The encoded store, as bytes ready to embed with `include_store!`.
pub fn encode(glyphs: &[GlyphInput], grid_shift: u8) -> Vec<u8> {
    // Pool: unique shapes as steps from their own first point.
    let mut pool: HashMap<Vec<(i32, i32)>, usize> = HashMap::new();
    let mut shapes: Vec<Vec<(i32, i32)>> = Vec::new();
    let mut records: Vec<Vec<(usize, i32, i32)>> = Vec::new();
    for g in glyphs {
        let mut placements = Vec::new();
        for chain in &g.chains {
            assert!(chain.len() >= 2, "a chain needs at least one segment");
            assert!(chain.len() - 1 < 1 << (32 - POOL_SHIFT), "a chain has too many steps for the format");
            let (x0, y0) = chain[0];
            let steps: Vec<(i32, i32)> =
                chain.windows(2).map(|w| (w[1].0 - w[0].0, w[1].1 - w[0].1)).collect();
            let id = *pool.entry(steps.clone()).or_insert_with(|| {
                shapes.push(steps);
                shapes.len() - 1
            });
            placements.push((id, x0, y0));
        }
        assert!(placements.len() < 1 << (32 - GLYPH_SHIFT), "a glyph has too many contours for the format");
        records.push(placements);
    }
    assert!(glyphs.len() <= u16::MAX as usize && shapes.len() <= u16::MAX as usize);
    let id_bits = (usize::BITS - (shapes.len().max(2) - 1).leading_zeros()) as u32;
    let most = records.iter().flatten().map(|&(_, x, y)| x.max(y)).max().unwrap_or(0).max(1);
    assert!(records.iter().flatten().all(|&(_, x, y)| x >= 0 && y >= 0), "start points cannot be negative");
    let xy_bits = 32 - (most as u32).leading_zeros();
    assert!(xy_bits <= 16, "start points are too far from the origin for the format");

    let mut ops = Vec::new();
    for (i, s) in shapes.iter().enumerate() {
        ops.push(Op::PoolStart(i));
        for &(dx, dy) in s {
            ops.push(Op::Step(dx, dy));
        }
    }
    for (i, r) in records.iter().enumerate() {
        ops.push(Op::GlyphStart(i));
        for &(id, x, y) in r {
            ops.push(Op::Raw(id as u32, id_bits));
            ops.push(Op::Raw(x as u32, xy_bits));
            ops.push(Op::Raw(y as u32, xy_bits));
        }
    }

    // A model with no symbols gets a single dummy one.
    let mut freqs: HashMap<u32, u64> = HashMap::new();
    let pair = |dx: i32, dy: i32| step_value(dx).0 | step_value(dy).0 << 8;
    for op in &ops {
        if let Op::Step(dx, dy) = *op {
            *freqs.entry(pair(dx, dy)).or_default() += 1;
        }
    }
    if freqs.is_empty() {
        freqs.insert(0, 1);
    }
    // The model's codes are limited to the lookahead, so only steps that overrun a peek take the
    // slow path, and that needs no more symbols than the lookahead has entries. The rarest share an
    // escape.
    let mut kept: Vec<(u32, u64)> = freqs.iter().map(|(&s, &f)| (s, f)).collect();
    kept.sort_by_key(|&(s, f)| (core::cmp::Reverse(f), s));
    let mut step_freqs: HashMap<u32, u64> = kept.iter().take(1 << FAST_BITS).copied().collect();
    if kept.len() > 1 << FAST_BITS {
        step_freqs = kept.iter().take((1 << FAST_BITS) - 1).copied().collect();
        step_freqs.insert(STEP_ESCAPE as u32, kept[(1 << FAST_BITS) - 1..].iter().map(|&(_, f)| f).sum());
    }
    let step_model = canonical(code_lengths(&step_freqs, FAST_BITS));
    let symbol = |dx: i32, dy: i32| {
        let s = pair(dx, dy);
        if step_model.codes.contains_key(&s) {
            s
        } else {
            STEP_ESCAPE as u32
        }
    };

    let mut w = BitWriter::new();
    let mut pool_offsets = vec![0u32; shapes.len()];
    let mut glyph_offsets = vec![0u32; glyphs.len()];
    for op in &ops {
        match *op {
            Op::PoolStart(i) => pool_offsets[i] = w.position(),
            Op::GlyphStart(i) => glyph_offsets[i] = w.position(),
            Op::Raw(v, n) => w.put(v, n),
            Op::Step(dx, dy) => {
                let s = symbol(dx, dy);
                let (code, len, _) = step_model.codes[&s];
                w.put(code, len);
                for v in [dx, dy] {
                    if s == STEP_ESCAPE as u32 {
                        w.put(v as u32 & ((1 << ESCAPE_BITS) - 1), ESCAPE_BITS);
                    } else {
                        let (_, raw, n) = step_value(v);
                        w.put(raw, n);
                    }
                }
            }
        }
    }
    let stream = w.finish();

    let mut out: Vec<u32> = vec![
        u32::from_le_bytes([VERSION, grid_shift, id_bits as u8, xy_bits as u8]),
        glyphs.len() as u32 | (shapes.len() as u32) << 16,
    ];
    out.extend(step_words(&step_model));
    for (&o, s) in pool_offsets.iter().zip(&shapes) {
        assert!(o < 1 << POOL_SHIFT, "the stream is too long for the format");
        out.push(o | (s.len() as u32) << POOL_SHIFT);
    }
    for ((g, o), r) in glyphs.iter().zip(glyph_offsets).zip(&records) {
        assert!(o < 1 << GLYPH_SHIFT, "the stream is too long for the format");
        out.push(o | (r.len() as u32) << GLYPH_SHIFT);
        let grid = (1u32 << grid_shift) as f32;
        let (w, h) = (g.bounds.width * grid, g.bounds.height * grid);
        assert!(
            w == (w as u32) as f32
                && h == (h as u32) as f32
                && (0.0..65536.0).contains(&w)
                && (0.0..65536.0).contains(&h),
            "a glyph's size is not a whole number of grid units below 2^16"
        );
        out.push((g.bounds.xmin * grid).to_bits());
        out.push((g.bounds.ymin * grid).to_bits());
        out.push(w as u32 | (h as u32) << 16);
        out.push(g.advance_width.to_bits());
    }
    out.push(stream.len() as u32);
    out.extend(&stream);
    out.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// Nearest integer, halves away from zero, as `f64::round` does. That needs `std`.
fn round(v: f64) -> i32 {
    if v >= 0.0 {
        (v + 0.5) as i32
    } else {
        (v - 0.5) as i32
    }
}

/// A fontdue glyph as the encoder takes it: its lines chained and quantized to `2^-grid_shift`
/// font units, and its bounds and advance.
pub fn glyph_input(g: &Glyph, grid_shift: u8) -> GlyphInput {
    let (chains, bounds) = chains(g, grid_shift);
    GlyphInput {
        chains,
        bounds,
        advance_width: g.advance_width(),
    }
}

/// The glyph's lines as chains of points quantized to `2^-grid_shift` font units, and bounds
/// computed from the quantized points. Lines are joined where one ends exactly where another
/// starts, which approximates contour order.
pub fn chains(g: &Glyph, grid_shift: u8) -> (Vec<Vec<(i32, i32)>>, Bounds) {
    let segs: Vec<((f32, f32), (f32, f32))> = g
        .v_lines()
        .iter()
        .chain(g.m_lines().iter())
        .map(|l| {
            let (a, b, c, d) = l.coords().copied();
            ((a, b), (c, d))
        })
        .collect();
    let key = |p: (f32, f32)| ((p.0.to_bits() as u64) << 32) | p.1.to_bits() as u64;
    let mut by_start: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, s) in segs.iter().enumerate() {
        by_start.entry(key(s.0)).or_default().push(i);
    }
    let ends: HashSet<u64> = segs.iter().map(|s| key(s.1)).collect();
    let mut used = vec![false; segs.len()];
    let scale = (1u32 << grid_shift) as f64;
    let q = |p: (f32, f32)| (round(p.0 as f64 * scale), round(p.1 as f64 * scale));
    let mut out = Vec::new();
    let order: Vec<usize> =
        (0..segs.len()).filter(|&i| !ends.contains(&key(segs[i].0))).chain(0..segs.len()).collect();
    for i in order {
        if used[i] {
            continue;
        }
        let mut pts = vec![q(segs[i].0)];
        let mut cur = i;
        loop {
            used[cur] = true;
            pts.push(q(segs[cur].1));
            match by_start.get(&key(segs[cur].1)).and_then(|v| v.iter().copied().find(|&j| !used[j])) {
                Some(j) => cur = j,
                None => break,
            }
        }
        out.push(pts);
    }
    let (mut w, mut h) = (0i32, 0i32);
    for &(x, y) in out.iter().flatten() {
        assert!(x >= 0 && y >= 0, "points are relative to the bounding box and cannot be negative");
        w = w.max(x);
        h = h.max(y);
    }
    let unit = 1.0 / scale as f32;
    let (width, height) = (w as f32 * unit, h as f32 * unit);
    let bounds = Bounds {
        xmin: g.bounds().xmin,
        ymin: g.bounds().ymin + g.bounds().height - height,
        width,
        height,
    };
    (out, bounds)
}
