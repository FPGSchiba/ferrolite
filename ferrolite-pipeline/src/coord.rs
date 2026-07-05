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
}
