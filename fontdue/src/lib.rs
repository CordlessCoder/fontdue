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

#[cfg(feature = "cache")]
mod cache;
#[doc(hidden)]
pub mod font;
mod fontrepr;
mod hash;
/// Tools for laying out strings of text.
pub mod layout;
mod lazy;
#[doc(hidden)]
pub mod math;
pub mod outline;
mod path;
mod platform;
pub mod raster;
pub mod store;
mod table;
mod transform;
mod unicode;

#[cfg(feature = "cache")]
pub use crate::cache::Cached;
pub use crate::font::*;
pub use crate::lazy::LazyFont;
pub use crate::outline::{
    GlyphRef, OutlineInfo, OutlineSource, PathEvent, PathGlyph, PathSource, SMALLEST_COORDINATE,
};
pub use crate::path::{
    Flatten, MAX_CURVE_SEGMENTS, PathCommand, Transform, TransformedMetrics, flatten, rasterize_path,
    rasterize_path_clipped,
};
pub use crate::transform::{
    PenOffsets, rasterize_source_transformed, rasterize_source_transformed_indexed, rasterize_transformed,
    transformed_raster_capacity,
};

#[cfg(feature = "hashbrown")]
pub(crate) use hashbrown::{HashMap, HashSet};
#[cfg(not(feature = "hashbrown"))]
pub(crate) use std::collections::{HashMap, HashSet};

/// Alias for Result<T, &'static str>.
pub type FontResult<T> = Result<T, &'static str>;
