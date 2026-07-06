//! `MaskOverlayCompositor` — composites a `MaskDefinition` against a (bounded,
//! downscaled) input image and returns a CPU coverage buffer for the Develop
//! canvas overlay. Reuses `ferrolite_mask::MaskCompositor` (the same passes the
//! edit DAG uses), so the overlay is faithful to the actual mask. The app caches
//! one instance (built once) and a bounded input; it calls `coverage` only when
//! the mask/preview/toggle change (never unconditionally per frame).

use std::sync::Arc;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{read_mask_r32f, MaskCompositor, MaskDefinition};

use crate::image::PipelineImage;

pub struct MaskOverlayCompositor {
    compositor: MaskCompositor,
}

impl MaskOverlayCompositor {
    pub fn new(ctx: Arc<GpuContext>) -> Self {
        Self {
            compositor: MaskCompositor::new(ctx),
        }
    }

    /// Composite `def` against `input` at `input`'s dimensions; return
    /// `(w, h, coverage)` with `coverage[i] ∈ [0,1]`, row-major, length w*h.
    /// `input` must already be bounded (≤ OVERLAY_MAX_EDGE) by the caller so the
    /// readback stays cheap. Range shapes sample `input`; analytic/brush shapes
    /// ignore it.
    pub fn coverage(
        &self,
        ctx: &GpuContext,
        def: &MaskDefinition,
        input: &PipelineImage,
    ) -> (u32, u32, Vec<f32>) {
        let (w, h) = (input.width, input.height);
        let iv = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let buf = self.compositor.composite(def, &iv, w, h);
        (w, h, read_mask_r32f(ctx, &buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::upload_source;
    use ferrolite_image::LinearRgbaF32;
    use ferrolite_mask::{CompositeMode, MaskComponent, Vec2 as MVec2};

    #[test]
    fn linear_gradient_coverage_ramps_left_to_right() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let oc = MaskOverlayCompositor::new(ctx.clone());
        // 8x1 mid-grey input (unused by the linear shape).
        let src = LinearRgbaF32::new(8, 1, vec![0.5; 8 * 4]).unwrap();
        let img = upload_source(&ctx, &src);
        let def = MaskDefinition {
            components: vec![(
                MaskComponent::LinearGradient {
                    start: MVec2::new(0.0, 0.5),
                    end: MVec2::new(1.0, 0.5),
                },
                CompositeMode::Add,
            )],
            invert: false,
        };
        let (w, h, cov) = oc.coverage(&ctx, &def, &img);
        assert_eq!((w, h), (8, 1));
        assert!(
            cov[0] < cov[7],
            "coverage increases left->right: {} !< {}",
            cov[0],
            cov[7]
        );
    }
}
