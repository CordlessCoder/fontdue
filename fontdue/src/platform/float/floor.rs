// On Xtensa, `floor.s` converts to an integer rounding toward minus infinity in one instruction.
// The libm form below is about 36 instructions, too long to inline, so every caller would pay a
// windowed call. Below 2^23 in magnitude a float can have a fraction and fits `i32`, so the
// convert is exact. At or above it, and for NaN and infinity, `x` is already its own floor.
// Copying `x`'s sign keeps -0.0 where libm keeps it.
//
// `floor_by_convert` is the same guard and sign copy with a portable truncating convert. The host
// test checks it against libm; the asm itself is only checked on the target.
#[cfg(test)]
#[inline(always)]
pub fn floor_by_convert(x: f32) -> f32 {
    if super::abs(x) < 8388608.0 {
        // SAFETY: |x| < 2^23, so `x` is finite and truncates into `i32`.
        let t = unsafe { x.to_int_unchecked::<i32>() } as f32;
        super::copysign(
            if t > x {
                t - 1.0
            } else {
                t
            },
            x,
        )
    } else {
        x
    }
}

#[cfg(target_arch = "xtensa")]
#[inline(always)]
pub fn floor(x: f32) -> f32 {
    if super::abs(x) < 8388608.0 {
        let i: i32;
        // SAFETY: reads one float register and writes one address register, nothing else.
        unsafe {
            core::arch::asm!("floor.s {0}, {1}, 0", out(reg) i, in(freg) x, options(pure, nomem, nostack))
        };
        super::copysign(i as f32, x)
    } else {
        x
    }
}

// [See license/rust-lang/libm] Copyright (c) 2018 Jorge Aparicio
#[cfg(not(target_arch = "xtensa"))]
pub fn floor(x: f32) -> f32 {
    let mut ui = x.to_bits();
    let e = (((ui >> 23) as i32) & 0xff) - 0x7f;

    if e >= 23 {
        return x;
    }
    if e >= 0 {
        let m: u32 = 0x007fffff >> e;
        if (ui & m) == 0 {
            return x;
        }
        if ui >> 31 != 0 {
            ui += m;
        }
        ui &= !m;
    } else {
        if ui >> 31 == 0 {
            ui = 0;
        } else if ui << 1 != 0 {
            return -1.0;
        }
    }
    f32::from_bits(ui)
}

#[cfg(test)]
mod tests {
    #[test]
    fn floor_by_convert_matches_libm_bit_for_bit() {
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
            let (fast, libm) = (super::floor_by_convert(x), super::floor(x));
            assert!(
                fast.to_bits() == libm.to_bits() || (fast.is_nan() && libm.is_nan()),
                "floor({x:e}): {fast:e} against {libm:e}"
            );
        }
    }
}
