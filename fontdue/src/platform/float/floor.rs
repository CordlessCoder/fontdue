#[cfg(target_arch = "xtensa")]
#[inline(always)]
pub fn floor(x: f32) -> f32 {
    let truncated = super::trunc(x);
    if truncated > x {
        truncated - 1.0
    } else {
        truncated
    }
}

// [See license/rust-lang/libm] Copyright (c) 2018 Jorge Aparicio
#[cfg(not(target_arch = "xtensa"))]
pub fn floor(x: f32) -> f32 {
    let mut ui = x.to_bits();
    let e = (((ui >> 23) as i32) & 0xff) - 0x7f;

    if e >= 23 {
        return x;
    }
    if e >= 0 {
        let m: u32 = 0x007fffff >> e;
        if (ui & m) == 0 {
            return x;
        }
        if ui >> 31 != 0 {
            ui += m;
        }
        ui &= !m;
    } else {
        if ui >> 31 == 0 {
            ui = 0;
        } else if ui << 1 != 0 {
            return -1.0;
        }
    }
    f32::from_bits(ui)
}
