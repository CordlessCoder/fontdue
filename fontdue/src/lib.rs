//! Fontdue is a font parser, rasterizer, and layout tool.
//!
//! This is a no_std crate, but still requires the alloc crate.

#![cfg_attr(all(not(test), not(feature = "std"), feature = "hashbrown"), no_std)]
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
mod platform;
pub mod raster;
mod table;
mod unicode;

pub use crate::font::*;

#[cfg(feature = "hashbrown")]
pub(crate) use hashbrown::{HashMap, HashSet};
#[cfg(not(feature = "hashbrown"))]
pub(crate) use std::collections::{HashMap, HashSet};

/// Alias for Result<T, &'static str>.
pub type FontResult<T> = Result<T, &'static str>;
