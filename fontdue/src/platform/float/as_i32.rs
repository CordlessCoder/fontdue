/// Truncates toward zero.
///
/// # Safety
///
/// `value` must be finite and truncate into `i32`. Xtensa has no saturating float-to-int
/// instruction, so `as i32` there costs 21 instructions against 4 for the raw convert. The raster
/// loop uses it, and so does `metrics_raw` after its range check; both can state the bound.
#[inline(always)]
pub unsafe fn as_i32_unchecked(value: f32) -> i32 {
    // SAFETY: the caller's obligation above is `to_int_unchecked`'s.
    unsafe { value.to_int_unchecked() }
}
