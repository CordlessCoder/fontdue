#[cfg(not(all(any(target_arch = "x86", target_arch = "x86_64"), feature = "simd")))]
#[inline(always)]
pub fn as_i32(value: f32) -> i32 {
    value as i32
}

#[cfg(all(any(target_arch = "x86", target_arch = "x86_64"), feature = "simd"))]
#[inline(always)]
pub fn as_i32(value: f32) -> i32 {
    #[cfg(target_arch = "x86")]
    use core::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::*;

    unsafe { _mm_cvtss_si32(_mm_set_ss(value)) }
}

/// # Safety
///
/// `value` must be finite and truncate into `i32`. Xtensa has no saturating float-to-int
/// instruction, so `as i32` there costs 21 instructions against 4 for the raw convert; this is
/// the variant the raster loop uses, and it is the only caller that can state the bound.
#[cfg(target_arch = "xtensa")]
#[inline(always)]
pub unsafe fn as_i32_unchecked(value: f32) -> i32 {
    unsafe { value.to_int_unchecked() }
}

/// # Safety
///
/// `value` must be finite and truncate into `i32`.
#[cfg(not(target_arch = "xtensa"))]
#[inline(always)]
pub unsafe fn as_i32_unchecked(value: f32) -> i32 {
    as_i32(value)
}
