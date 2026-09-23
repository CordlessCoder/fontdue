/// `(1.0 / a, 1.0 / b)`. On Xtensa each is the `recip0.s` estimate refined by two fused Newton
/// steps, `e = 1 - x·r` then `r + r·e`, measured within 1 ulp of the division for every normal
/// `x`. There the division is a ROM call that spills every live float register around it. For
/// zero, subnormal and infinite `x` the estimate's result does not match the division's class, so
/// callers keep both normal. The two chains are interleaved in one block: every step waits four
/// cycles on the one before it, and the other chain's step fills the wait. Everywhere else it
/// divides.
#[inline(always)]
pub fn recip2(a: f32, b: f32) -> (f32, f32) {
    #[cfg(target_arch = "xtensa")]
    {
        let (ra, rb): (f32, f32);
        // SAFETY: register-only float arithmetic.
        unsafe {
            core::arch::asm!(
                "recip0.s {ra}, {a}",
                "recip0.s {rb}, {b}",
                "const.s {ea}, 1",
                "const.s {eb}, 1",
                "msub.s {ea}, {a}, {ra}",
                "msub.s {eb}, {b}, {rb}",
                "madd.s {ra}, {ra}, {ea}",
                "madd.s {rb}, {rb}, {eb}",
                "const.s {ea}, 1",
                "const.s {eb}, 1",
                "msub.s {ea}, {a}, {ra}",
                "msub.s {eb}, {b}, {rb}",
                "madd.s {ra}, {ra}, {ea}",
                "madd.s {rb}, {rb}, {eb}",
                ra = out(freg) ra,
                rb = out(freg) rb,
                ea = out(freg) _,
                eb = out(freg) _,
                a = in(freg) a,
                b = in(freg) b,
                options(pure, nomem, nostack)
            );
        }
        (ra, rb)
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        (1.0 / a, 1.0 / b)
    }
}
