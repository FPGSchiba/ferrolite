//! Shared constants for the Develop mask overlay's GPU-native red tint
//! (`ferrolite_pipeline::MaskOverlayCompositor::overlay_texture` +
//! `overlay_tint`). No egui/GPU here — just the bound + strength the app and
//! pipeline agree on.

/// Bounded overlay resolution (longest edge) — keeps the GPU composite + tint
/// pass small enough to rebuild every frame during a stroke (CLAUDE.md §1).
pub const OVERLAY_MAX_EDGE: u32 = 512;

/// Red-overlay tint strength (alpha multiplier). Matches the former 50% tint.
pub const OVERLAY_STRENGTH: f32 = 0.5;
