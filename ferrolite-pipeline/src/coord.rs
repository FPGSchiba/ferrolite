//! Pure display→source inverse coordinate mapping. Mask shapes/strokes are stored
//! in normalized SOURCE coords (§5.2) so they stay anchored to content across
//! crop/rotate/aspect (all applied AFTER LocalAdjustments). The app inverse-maps a
//! display-space pointer to source coords through the active geometry; lens is
//! treated as identity here (the §5.2 fallback). No GPU — fully unit-tested.
//!
//! `geometry_uniform` already builds the output→source transform used by the GPU
//! resample: `src_px = m · out_px + off` (row-major 2×2 `m`). We reuse it: an output
//! point in [0,1] scales to output pixels, maps to source pixels, then normalizes by
//! source dims.

use crate::op::Geometry;
use crate::uniforms::geometry_uniform;

/// Map a normalized output/crop-space point (`out_norm` in [0,1]²) to normalized
/// source-space coords. `geo` is the active geometry op (None = identity).
pub fn display_to_source(
    geo: Option<Geometry>,
    src_w: u32,
    src_h: u32,
    out_norm: (f32, f32),
) -> (f32, f32) {
    let (u, out_w, out_h) = geometry_uniform(geo, src_w, src_h);
    let ox = out_norm.0 * out_w as f32;
    let oy = out_norm.1 * out_h as f32;
    // src_px = m · out_px + off  (m row-major [m00, m01, m10, m11]).
    let sx = u.m[0] * ox + u.m[1] * oy + u.off[0];
    let sy = u.m[2] * ox + u.m[3] * oy + u.off[1];
    (sx / u.src_dims[0], sy / u.src_dims[1])
}

/// Inverse of `display_to_source`: map a normalized SOURCE point to normalized
/// OUTPUT/crop space, for placing mask handles on the displayed (cropped/rotated)
/// image. `src_px = m·out_px + off` ⇒ `out_px = m⁻¹·(src_px − off)`; then normalize
/// by the output dims. Identity geometry → the identity map.
pub fn source_to_display(
    geo: Option<Geometry>,
    src_w: u32,
    src_h: u32,
    src_norm: (f32, f32),
) -> (f32, f32) {
    let (u, out_w, out_h) = geometry_uniform(geo, src_w, src_h);
    let sx = src_norm.0 * u.src_dims[0];
    let sy = src_norm.1 * u.src_dims[1];
    // Invert the row-major 2×2 m = [a b; c d].
    let (a, b, c, d) = (u.m[0], u.m[1], u.m[2], u.m[3]);
    let det = a * d - b * c;
    let inv_det = if det.abs() < 1e-12 { 0.0 } else { 1.0 / det };
    let dx = sx - u.off[0];
    let dy = sy - u.off[1];
    let ox = (d * dx - b * dy) * inv_det;
    let oy = (-c * dx + a * dy) * inv_det;
    (ox / out_w as f32, oy / out_h as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::{Aspect, CropRect, Geometry};

    fn approx(a: (f32, f32), b: (f32, f32)) {
        assert!(
            (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4,
            "{a:?} != {b:?}"
        );
    }

    #[test]
    fn identity_geometry_is_the_identity_map() {
        approx(display_to_source(None, 100, 80, (0.25, 0.75)), (0.25, 0.75));
        approx(display_to_source(None, 100, 80, (0.0, 0.0)), (0.0, 0.0));
    }

    #[test]
    fn crop_maps_output_into_the_crop_window() {
        // Crop the centre half: output (0,0) → source (0.25,0.25); output (1,1) → (0.75,0.75).
        let geo = Geometry {
            crop: CropRect {
                x: 0.25,
                y: 0.25,
                w: 0.5,
                h: 0.5,
            },
            angle_deg: 0.0,
            aspect: Aspect::Free,
        };
        approx(
            display_to_source(Some(geo), 100, 100, (0.0, 0.0)),
            (0.25, 0.25),
        );
        approx(
            display_to_source(Some(geo), 100, 100, (1.0, 1.0)),
            (0.75, 0.75),
        );
        approx(
            display_to_source(Some(geo), 100, 100, (0.5, 0.5)),
            (0.5, 0.5),
        );
    }

    #[test]
    fn rotation_round_trips_through_the_center() {
        // The crop centre is invariant under rotation about it.
        let geo = Geometry {
            crop: CropRect::full(),
            angle_deg: 90.0,
            aspect: Aspect::Original,
        };
        approx(
            display_to_source(Some(geo), 100, 100, (0.5, 0.5)),
            (0.5, 0.5),
        );
    }

    #[test]
    fn source_to_display_is_identity_for_identity_geometry() {
        let p = source_to_display(None, 100, 80, (0.25, 0.75));
        assert!((p.0 - 0.25).abs() < 1e-4 && (p.1 - 0.75).abs() < 1e-4);
    }

    #[test]
    fn source_to_display_round_trips_display_to_source_under_crop() {
        let geo = Geometry {
            crop: CropRect {
                x: 0.25,
                y: 0.25,
                w: 0.5,
                h: 0.5,
            },
            angle_deg: 0.0,
            aspect: Aspect::Free,
        };
        for &(ox, oy) in &[(0.0f32, 0.0f32), (1.0, 1.0), (0.3, 0.6)] {
            let src = display_to_source(Some(geo), 100, 100, (ox, oy));
            let back = source_to_display(Some(geo), 100, 100, src);
            assert!(
                (back.0 - ox).abs() < 1e-3 && (back.1 - oy).abs() < 1e-3,
                "round-trip {ox},{oy} -> {back:?}"
            );
        }
    }

    #[test]
    fn source_to_display_round_trips_under_rotation() {
        let geo = Geometry {
            crop: CropRect::full(),
            angle_deg: 30.0,
            aspect: Aspect::Original,
        };
        let src = display_to_source(Some(geo), 120, 90, (0.4, 0.55));
        let back = source_to_display(Some(geo), 120, 90, src);
        assert!(
            (back.0 - 0.4).abs() < 2e-3 && (back.1 - 0.55).abs() < 2e-3,
            "rot round-trip -> {back:?}"
        );
    }
}
