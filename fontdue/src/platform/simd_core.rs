#![allow(non_camel_case_types)]

#[repr(C)]
#[derive(Copy, Clone)]
pub struct f32x4 {
    x0: f32,
    x1: f32,
    x2: f32,
    x3: f32,
}

impl f32x4 {
    #[inline(always)]
    pub const fn new(x0: f32, x1: f32, x2: f32, x3: f32) -> Self {
        f32x4 {
            x0,
            x1,
            x2,
            x3,
        }
    }

    #[inline(always)]
    pub const fn copied(self) -> (f32, f32, f32, f32) {
        (self.x0, self.x1, self.x2, self.x3)
    }

    #[inline(always)]
    pub fn sqrt(self) -> Self {
        use super::sqrt;
        Self::new(sqrt(self.x0), sqrt(self.x1), sqrt(self.x2), sqrt(self.x3))
    }
}
