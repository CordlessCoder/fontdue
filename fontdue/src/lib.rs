//! Fontdue is a font parser, rasterizer, and layout tool.
//!
//! This is a no_std crate, but still requires the alloc crate.

#![cfg_attr(all(not(test), not(feature = "std"), feature = "hashbrown"), no_std)]
#![cfg_attr(target_arch = "xtensa", feature(asm_experimental_arch, core_float_math))]
#![allow(dead_code)]
#![allow(clippy::style)]
#![allow(clippy::complexity)]
#![allow(clippy::misnamed_getters)]

extern crate alloc;

#[doc(hidden)]
pub mod font;
mod fontrepr;
mod hash;
/// Tools for laying out strings of text.
pub mod layout;
#[doc(hidden)]
pub mod math;
pub mod outline;
mod platform;
pub mod raster;
pub mod store;
mod table;
mod transform;
mod unicode;

pub use crate::font::*;
pub use crate::outline::{GlyphRef, LineGlyph, OutlineInfo, OutlineSource, SegmentSource};
pub use crate::transform::{
    PenOffsets, Transform, TransformedMetrics, rasterize_source_transformed,
    rasterize_source_transformed_indexed, rasterize_transformed, transformed_raster_capacity,
};

#[cfg(feature = "hashbrown")]
pub(crate) use hashbrown::{HashMap, HashSet};
#[cfg(not(feature = "hashbrown"))]
pub(crate) use std::collections::{HashMap, HashSet};

/// Alias for Result<T, &'static str>.
pub type FontResult<T> = Result<T, &'static str>;
