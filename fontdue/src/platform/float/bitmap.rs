/// Turns four accumulated area deltas into four coverage bytes, continuing the running total from
/// `offset`. Returns the bytes and the total after the fourth delta, which the caller feeds back in
/// as the next block's `offset`.
///
/// The two implementations do not agree bit for bit. The SIMD one sums the block as a tree, the
/// scalar one left to right, and the two round differently at the truncation to u8. Baselines
/// recorded under one will show single-unit differences under the other.
#[cfg(not(all(any(target_arch = "x86", target_arch = "x86_64"), feature = "simd")))]
#[inline(always)]
pub fn bitmap_block(chunk: [f32; 4], offset: f32) -> ([u8; 4], f32) {
    use crate::platform::abs;

    let mut height = offset;
    let mut out = [0u8; 4];
    for i in 0..4 {
        height += chunk[i];
        // The cast saturates, so coverage over 1.0 pins to 255 rather than wrapping.
        out[i] = (abs(height) * 255.9) as u8;
    }
    (out, height)
}

#[cfg(all(any(target_arch = "x86", target_arch = "x86_64"), feature = "simd"))]
#[inline(always)]
pub fn bitmap_block(chunk: [f32; 4], offset: f32) -> ([u8; 4], f32) {
    #[cfg(target_arch = "x86")]
    use core::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::*;

    unsafe {
        // Negative zero: the sign bit alone, used to mask it off for abs.
        let nzero = _mm_castps_si128(_mm_set1_ps(-0.0));

        let mut x = _mm_loadu_ps(chunk.as_ptr());
        // Prefix sum across the four lanes: x += (0, x0, x1, x2), then x += (0, 0, x0, x1).
        x = _mm_add_ps(x, _mm_castsi128_ps(_mm_slli_si128(_mm_castps_si128(x), 4)));
        x = _mm_add_ps(x, _mm_castsi128_ps(_mm_slli_si128(_mm_castps_si128(x), 8)));
        x = _mm_add_ps(x, _mm_set1_ps(offset));

        let y = _mm_mul_ps(x, _mm_set1_ps(255.9));
        let y = _mm_andnot_ps(_mm_castsi128_ps(nzero), y);
        let y = _mm_cvttps_epi32(y);
        // Both packs saturate, which is what keeps coverage over 1.0 at 255.
        let y = _mm_packus_epi16(_mm_packs_epi32(y, nzero), nzero);

        let packed = core::mem::transmute::<__m128i, [i32; 4]>(y)[0];
        let carry = core::mem::transmute::<__m128, [f32; 4]>(x)[3];
        (packed.to_ne_bytes(), carry)
    }
}
