/// `1.0 / x`. On Xtensa, for magnitudes in `[2^-64, 2^64]`, it is the `recip0.s` estimate refined
/// by two fused Newton steps, within 1 ulp of the division, which there is a ROM call that spills
/// every live float register around it. Outside that range, and on every other target, it divides.
#[inline(always)]
pub fn recip(x: f32) -> f32 {
    #[cfg(target_arch = "xtensa")]
    {
        // The estimate's behaviour on zero, subnormals and infinity is not checked on the core,
        // and the line walk's crossing order depends on these values, so those divide.
        let magnitude = x.to_bits() & 0x7fff_ffff;
        if (0x1f80_0000..=0x5f80_0000).contains(&magnitude) {
            let mut r: f32;
            // SAFETY: register-only float arithmetic.
            unsafe {
                core::arch::asm!("recip0.s {r}, {x}", r = out(freg) r, x = in(freg) x, options(pure, nomem, nostack));
            }
            // Written out rather than looped: at `opt-level = "s"` a two-trip loop is not
            // unrolled, and its counter lands in the dependent chain.
            r = step(x, r);
            r = step(x, r);
            return r;
        }
    }
    1.0 / x
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
