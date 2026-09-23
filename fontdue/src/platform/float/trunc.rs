// On Xtensa the bit-twiddling form below costs 20 instructions where a convert and back costs a
// few. Below 2^23 in magnitude a float can have a fraction and fits `i32`, so the convert is exact.
// At or above it, and for NaN and infinity, `x` is already its own truncation. The guard keeps
// this total: `fract` reaches it from `metrics_raw` with values derived from the caller's `px`.
// The raster, whose values are in range by construction, converts unchecked through its own
// `index_of` instead.
#[cfg(any(test, target_arch = "xtensa"))]
#[inline(always)]
pub fn trunc_by_convert(x: f32) -> f32 {
    if super::abs(x) < 8388608.0 {
        // SAFETY: |x| < 2^23, so `x` is finite and truncates into `i32`.
        super::copysign(unsafe { x.to_int_unchecked::<i32>() } as f32, x)
    } else {
        x
    }
}

#[cfg(target_arch = "xtensa")]
pub use trunc_by_convert as trunc;

// [See license/rust-lang/libm] Copyright (c) 2018 Jorge Aparicio

#[cfg(not(target_arch = "xtensa"))]
pub fn trunc(x: f32) -> f32 {
    let mut i: u32 = x.to_bits();
    let mut e: i32 = (i >> 23 & 0xff) as i32 - 0x7f + 9;
    let m: u32;
    if e >= 23 + 9 {
        return x;
    }
    if e < 9 {
        e = 1;
    }
    m = -1i32 as u32 >> e;
    if (i & m) == 0 {
        return x;
    }
    i &= !m;
    f32::from_bits(i)
}

#[cfg(test)]
mod tests {
    /// The libm form, which is what `trunc` is off Xtensa.
    fn libm(x: f32) -> f32 {
        let mut i: u32 = x.to_bits();
        let mut e: i32 = (i >> 23 & 0xff) as i32 - 0x7f + 9;
        if e >= 23 + 9 {
            return x;
        }
        if e < 9 {
            e = 1;
        }
        let m = -1i32 as u32 >> e;
        if (i & m) == 0 {
            return x;
        }
        i &= !m;
        f32::from_bits(i)
    }

    #[test]
    fn trunc_by_convert_matches_libm_bit_for_bit() {
        let special = [
            0.0,
            -0.0,
            0.5,
            -0.5,
            1.0,
            -1.0,
            8388607.5,
            -8388607.5,
            8388608.0,
            -8388608.0,
            f32::MAX,
            f32::MIN,
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        let sweep = (0..=u32::MAX).step_by(4099).map(f32::from_bits);
        for x in special.into_iter().chain(sweep) {
            let (fast, libm) = (super::trunc_by_convert(x), libm(x));
            assert!(
                fast.to_bits() == libm.to_bits() || (fast.is_nan() && libm.is_nan()),
                "trunc({x:e}): {fast:e} against {libm:e}"
            );
        }
    }
}
