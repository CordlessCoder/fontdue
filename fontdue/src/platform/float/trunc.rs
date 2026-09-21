// Xtensa has no float round-to-integer instruction and no saturating convert, so the portable
// bit-twiddling form below costs 20 instructions where a raw convert-and-back costs 6. Only the
// raster calls this, through `f32x4::trunc`, and its coordinates are bounded by the glyph
// dimensions `metrics_raw` produced. Nothing else in the crate may call it on this target.
//
// The convert is unchecked: a non-finite input, or one that does not fit `i32`, is undefined
// behaviour here where every other target saturates.
#[cfg(all(
    target_arch = "xtensa",
    not(all(any(target_arch = "x86", target_arch = "x86_64"), feature = "simd"))
))]
#[inline(always)]
pub fn trunc(value: f32) -> f32 {
    unsafe { value.to_int_unchecked::<i32>() as f32 }
}

// [See license/rust-lang/libm] Copyright (c) 2018 Jorge Aparicio

#[cfg(all(
    not(target_arch = "xtensa"),
    not(all(any(target_arch = "x86", target_arch = "x86_64"), feature = "simd"))
))]
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

#[cfg(all(any(target_arch = "x86", target_arch = "x86_64"), feature = "simd"))]
#[inline(always)]
pub fn trunc(value: f32) -> f32 {
    #[cfg(target_arch = "x86")]
    use core::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::*;

    unsafe { _mm_cvtss_f32(_mm_cvtepi32_ps(_mm_cvttps_epi32(_mm_set_ss(value)))) }
}
