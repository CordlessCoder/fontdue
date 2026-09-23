/// `1.0 / x`. On Xtensa it is the `recip0.s` estimate refined by two fused Newton steps, measured
/// within 1 ulp of the division for every normal `x`. There the division is a ROM call that spills
/// every live float register around it, and a guarded fallback to it costs the loop around it the
/// same, taken or not. For zero, subnormal and infinite `x` the estimate's result does not match
/// the division's class, so callers keep `x` normal. Everywhere else it divides.
#[inline(always)]
pub fn recip(x: f32) -> f32 {
    #[cfg(target_arch = "xtensa")]
    {
        let mut r: f32;
        // SAFETY: register-only float arithmetic.
        unsafe {
            core::arch::asm!("recip0.s {r}, {x}", r = out(freg) r, x = in(freg) x, options(pure, nomem, nostack));
        }
        // Written out rather than looped: at `opt-level = "s"` a two-trip loop is not unrolled,
        // and its counter lands in the dependent chain.
        r = step(x, r);
        r = step(x, r);
        r
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        1.0 / x
    }
}

/// One Newton step for `1 / x`: `e = 1 - x·r`, then `r + r·e`, both fused.
#[cfg(target_arch = "xtensa")]
#[inline(always)]
fn step(x: f32, r: f32) -> f32 {
    let mut e: f32 = 1.0;
    let mut r = r;
    // SAFETY: register-only float arithmetic.
    unsafe {
        core::arch::asm!("msub.s {e}, {x}, {r}", e = inout(freg) e, x = in(freg) x, r = in(freg) r, options(pure, nomem, nostack));
        core::arch::asm!("madd.s {r}, {r}, {e}", r = inout(freg) r, e = in(freg) e, options(pure, nomem, nostack));
    }
    r
}
