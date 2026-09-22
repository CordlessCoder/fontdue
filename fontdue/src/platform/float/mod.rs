mod as_i32;
mod atan;
mod atan2;
mod ceil;
mod floor;
mod fract;
mod sqrt;
mod trunc;

pub use as_i32::*;
pub use atan::*;
pub use atan2::*;
pub use ceil::*;
pub use floor::*;
pub use fract::*;
pub use sqrt::*;
#[allow(unused_imports)]
pub use trunc::*;

/// Sets the high bit 0x80000000 on a float.
#[inline(always)]
pub const fn abs(value: f32) -> f32 {
    f32::from_bits(value.to_bits() & 0x7fffffff)
}

/// Checks if the high bit 0x80000000 is set on a float.
#[inline(always)]
pub const fn is_negative(value: f32) -> bool {
    value.to_bits() >= 0x80000000
}

/// Checks if the high bit 0x80000000 is not set on a float.
#[inline(always)]
pub const fn is_positive(value: f32) -> bool {
    value.to_bits() < 0x80000000
}

/// Inverts the high bit 0x80000000 on a float.
#[inline(always)]
pub const fn flipsign(value: f32) -> f32 {
    f32::from_bits(value.to_bits() ^ 0x80000000)
}

/// Assigns the high bit 0x80000000 on the sign to the value.
#[inline(always)]
pub const fn copysign(value: f32, sign: f32) -> f32 {
    f32::from_bits((value.to_bits() & 0x7fffffff) | (sign.to_bits() & 0x80000000))
}

/// `a * b + c`. On Xtensa this is one fused `madd.s` with a single rounding, so results there can
/// differ from other targets by one ulp. Everywhere else it rounds twice, since a fused form is not
/// guaranteed to be a single instruction there.
#[cfg(target_arch = "xtensa")]
#[inline(always)]
pub fn mul_add(a: f32, b: f32, c: f32) -> f32 {
    core::f32::math::mul_add(a, b, c)
}

/// `a * b + c`, rounded twice. See the Xtensa form.
#[cfg(not(target_arch = "xtensa"))]
#[inline(always)]
pub fn mul_add(a: f32, b: f32, c: f32) -> f32 {
    a * b + c
}
