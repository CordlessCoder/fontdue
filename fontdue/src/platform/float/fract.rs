#[inline(always)]
pub fn fract(value: f32) -> f32 {
    value - super::trunc(value)
}
