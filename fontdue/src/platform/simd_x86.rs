#![allow(non_camel_case_types)]

#[cfg(target_arch = "x86")]
use core::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

#[derive(Copy, Clone)]
#[repr(transparent)]
pub struct f32x4(__m128);

impl f32x4 {
    #[inline(always)]
    pub const fn new(x0: f32, x1: f32, x2: f32, x3: f32) -> Self {
        // SAFETY: `__m128` is 16 bytes, every bit pattern valid, and its lanes lie in memory order,
        // as an array's elements do.
        Self(unsafe { core::mem::transmute::<[f32; 4], __m128>([x0, x1, x2, x3]) })
    }

    #[inline(always)]
    pub fn copied(self) -> (f32, f32, f32, f32) {
        // SAFETY: as in `new`.
        let [x0, x1, x2, x3] = unsafe { core::mem::transmute::<__m128, [f32; 4]>(self.0) };
        (x0, x1, x2, x3)
    }

    #[inline(always)]
    pub fn sqrt(self) -> Self {
        // SAFETY: this module is compiled only where SSE2 is enabled.
        unsafe { f32x4(_mm_sqrt_ps(self.0)) }
    }
}
