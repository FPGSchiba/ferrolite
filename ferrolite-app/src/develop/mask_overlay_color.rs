//! Pure conversion of a mask coverage buffer to a red RGBA overlay image. Alpha
//! = coverage · strength; RGB is the overlay color (default red). No egui/GPU.
//!
//! NOTE: consumed by the canvas overlay task later in Plan 4; the module-level
//! allow is REMOVED at the Plan-4 gate (Task 13).
#![allow(dead_code)]

/// Bounded overlay resolution (longest edge) — keeps the GPU composite + readback
/// small enough to rebuild every frame during a stroke (CLAUDE.md §1).
pub const OVERLAY_MAX_EDGE: u32 = 512;

/// Red overlay: each texel becomes (255, 0, 0, coverage·strength·255).
pub fn overlay_rgba(coverage: &[f32], strength: f32) -> Vec<u8> {
    let s = strength.clamp(0.0, 1.0);
    let mut out = Vec::with_capacity(coverage.len() * 4);
    for &c in coverage {
        let a = (c.clamp(0.0, 1.0) * s * 255.0).round() as u8;
        out.extend_from_slice(&[255, 0, 0, a]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_coverage_is_transparent_full_is_opaque_red() {
        let px = overlay_rgba(&[0.0, 1.0], 1.0);
        assert_eq!(&px[0..4], &[255, 0, 0, 0], "zero coverage -> transparent");
        assert_eq!(&px[4..8], &[255, 0, 0, 255], "full coverage -> opaque red");
    }

    #[test]
    fn strength_scales_alpha() {
        let px = overlay_rgba(&[1.0], 0.5);
        assert_eq!(px[3], 128, "half strength -> ~50% alpha");
    }

    #[test]
    fn coverage_is_clamped() {
        let px = overlay_rgba(&[-0.2, 1.5], 1.0);
        assert_eq!(px[3], 0);
        assert_eq!(px[7], 255);
    }
}
