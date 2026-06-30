//! The photo edit tile producer: implements the engine-tier `ferrolite_vt::
//! TileProducer` by rendering each tile through a `TileEditPipeline` over the
//! GPU-resident source pyramid. Lives in the app (not the VT) so the VT stays
//! photo-agnostic (spec §5.2). `!Send`/`!Sync` (holds the pipeline's Rc/RefCell);
//! owned by `ViewerState` and only ever called on the render/update thread.

use ferrolite_gpu::GpuContext;
use ferrolite_image::TileCoord;
use ferrolite_pipeline::TileEditPipeline;
use ferrolite_vt::TileProducer;

pub struct EditTileProducer {
    pipeline: TileEditPipeline,
}

impl EditTileProducer {
    pub fn new(pipeline: TileEditPipeline) -> Self {
        Self { pipeline }
    }

    /// Update the producer's op stack in place (color-only changes). Geometry /
    /// halo-radius changes are baked at construction and require rebuilding the
    /// whole producer, not this passthrough.
    pub fn set_stack(&mut self, stack: ferrolite_pipeline::OpStack) {
        self.pipeline.set_stack(stack);
    }
}

impl TileProducer for EditTileProducer {
    fn produce(&mut self, _ctx: &GpuContext, coord: TileCoord) -> wgpu::Texture {
        // `_ctx` is the same device the pipeline was built against; the pipeline
        // holds its own Arc<GpuContext>, so we render through it directly.
        self.pipeline.produce_tile(coord)
    }
}
