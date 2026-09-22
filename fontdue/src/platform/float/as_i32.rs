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

/// Truncates toward zero, on every target. `as_i32` does not: its x86 SIMD form rounds to
/// nearest.
///
/// # Safety
///
/// `value` must be finite and truncate into `i32`. Xtensa has no saturating float-to-int
/// instruction, so `as i32` there costs 21 instructions against 4 for the raw convert. The raster
/// loop uses it, and so does `metrics_raw` after its range check; both can state the bound.
#[inline(always)]
pub unsafe fn as_i32_unchecked(value: f32) -> i32 {
    unsafe { value.to_int_unchecked() }
}
