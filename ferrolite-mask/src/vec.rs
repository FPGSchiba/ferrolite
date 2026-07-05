//! Minimal scalar value types for parametric mask shapes. Kept crate-local
//! (no glam dependency) so the engine-transferable dependency graph stays lean.

use serde::{Deserialize, Serialize};

/// A 2D point/vector in normalized source coordinates ([0,1]² over the image).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A linear-RGB color triple used by color-range selection samples.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Rgb {
    pub fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }
}
