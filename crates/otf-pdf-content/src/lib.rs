//! Content layer: parses content streams and interprets them, tracking the
//! graphics and text state, resources and Form XObjects, and reports what is
//! drawn (paths, glyphs, images, marked content) to a [`Device`]. Text
//! extraction, rendering and conformance checks are all devices.
//!
//! Fonts come through the [`FontLoader`] trait, implemented by
//! `otf-pdf-font`, so this crate stays below it in the dependency order.
#![forbid(unsafe_code)]

mod device;
mod interp;
mod matrix;
mod ops;

pub use device::{
    Device, FillRule, Font, FontLoader, Glyph, GraphicsState, Image, PathSegment, TextState,
};
pub use interp::{page_content, run_page};
pub use matrix::Matrix;
pub use ops::{Operation, parse_content};
