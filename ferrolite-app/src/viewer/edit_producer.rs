//! The photo edit tile producer: implements the engine-tier `ferrolite_vt::
//! TileProducer` by rendering each tile through a `TileEditPipeline` over the
//! GPU-resident source pyramid. Lives in the app (not the VT) so the VT stays
//! photo-agnostic (spec §5.2). `!Send`/`!Sync` (holds the pipeline's Rc/RefCell);
//! owned by `ViewerState` and only ever called on the render/update thread.

use ferrolite_gpu::GpuContext;
use ferrolite_image::TileCoord;
use ferrolite_pipeline::{LensUniform, TileEditPipeline};
use ferrolite_vt::TileProducer;

pub struct EditTileProducer {
    pipeline: TileEditPipeline,
}

impl EditTileProducer {
    pub fn new(pipeline: TileEditPipeline) -> Self {
        Self { pipeline }
    }

    /// Update the producer's op stack in place (color-only changes). Geometry,
    /// halo-radius, and the baked lens warp grid are fixed at construction and a
    /// change to any of them requires rebuilding the whole producer (via
    /// `needs_full_rebuild`), not this passthrough.
    pub fn set_stack(&mut self, stack: ferrolite_pipeline::OpStack) {
        self.pipeline.set_stack(stack);
    }

    /// Update the producer's camera→working color matrix in place (working-space
    /// change). Geometry / halo-radius changes still require rebuilding the whole
    /// producer, not this passthrough.
    pub fn set_color_matrix(&mut self, m: [[f32; 3]; 3]) {
        self.pipeline.set_color_matrix(m);
    }

    /// The geometry-applied OUTPUT dims this producer renders tiles in (the
    /// rounded crop extent baked at construction). The single source of truth
    /// for the full tier's logical size: whenever a producer is (re)installed,
    /// the sparse VT's logical dims must be re-pointed at THIS value
    /// (`VirtualTexture::set_sparse_image_dims`) so the display/compose
    /// transform, the shader's extent clip, and the convergence needed-set all
    /// agree with the preview tier's cropped output — a mismatch presents as a
    /// wrongly-cropped image at rest and heavy preview↔full flicker on
    /// pan/zoom for cropped images.
    pub fn out_dims(&self) -> (u32, u32) {
        self.pipeline.out_dims()
    }

    // ── Lens amount passthroughs (Spec 4.4, U7) ────────────────────────────
    // Amount-only lens slider changes (distortion/tca/vignetting `amount`,
    // NOT lens id / enabled flags / focal / aperture / crop — those change the
    // baked grid/LUT and require discarding + rebuilding the whole producer
    // via `needs_full_rebuild`, same as a geometry change). These two are
    // plain uniform buffer writes; no pipeline rebuild.

    /// Set the lens-correction amounts + `use_warp` flag (buffer write only).
    pub fn set_lens_uniform(&mut self, lens: LensUniform) {
        self.pipeline.set_lens_uniform(lens);
    }

    /// Set the vignette lerp amount (buffer write only; 0 = identity).
    pub fn set_vig_amount(&mut self, amount: f32) {
        self.pipeline.set_vig_amount(amount);
    }

    /// Set the parametric manual (lens-free) vignette gain (buffer write only;
    /// 0 = identity, negative darkens corners, positive brightens). Independent
    /// of `set_vig_amount` (profile LUT lerp); see `develop::vignette_mode`.
    pub fn set_vig_manual(&mut self, manual: f32) {
        self.pipeline.set_vig_manual(manual);
    }

    /// Set the dehaze atmospheric light on the underlying tiled pipeline (design
    /// §5.3). Called once per image after the producer is built.
    pub fn set_dehaze_atmos(&mut self, atmos: [f32; 3]) {
        self.pipeline.set_dehaze_atmos(atmos);
    }

    /// Hand the tiled pipeline the whole-image dehaze transmission computed by
    /// the preview `EditPipeline` (ST-Task 4 / shared-transmission plan). The
    /// tiled recovery samples this shared, source-space map instead of
    /// recomputing its own per-tile transmission — `None` when dehaze is
    /// inactive, which makes the recovery a passthrough.
    pub fn set_shared_transmission(&mut self, tex: Option<std::sync::Arc<wgpu::Texture>>) {
        self.pipeline.set_shared_transmission(tex);
    }
}

impl TileProducer for EditTileProducer {
    fn produce(&mut self, _ctx: &GpuContext, coord: TileCoord) -> wgpu::Texture {
        // `_ctx` is the same device the pipeline was built against; the pipeline
        // holds its own Arc<GpuContext>, so we render through it directly.
        self.pipeline.produce_tile(coord)
    }
}
