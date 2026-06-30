//! GPU tile-producer seam (cross-cutting contract §5). The VT can fill a pool
//! slot by asking a producer to RENDER the tile on the GPU — no CPU readback —
//! instead of uploading CPU pixels. The trait is photo-agnostic: it knows only a
//! `GpuContext` and a `TileCoord`, and returns a `TILE_SIZE`² `Rgba16Float`
//! texture (`COPY_SRC`) that the VT copies into the slot. The photo edit producer
//! lives in `ferrolite-app`/`ferrolite-pipeline`, never here.
//!
//! The trait is intentionally NOT `Send`/`Sync`: the edit producer wraps a
//! `TileEditPipeline` that holds `Rc`/`RefCell` (it reuses the Plan 1/2 nodes).
//! It lives in `ViewerState` (single-threaded app state) and is handed to the VT
//! as `&mut dyn TileProducer` per `produce_view` call — never stored in the VT,
//! which must stay `Sync` to live in eframe's `callback_resources`.

use ferrolite_gpu::GpuContext;
use ferrolite_image::TileCoord;

pub trait TileProducer {
    /// Render the `TILE_SIZE`² interior for `coord` into a fresh `Rgba16Float`
    /// texture with `COPY_SRC` usage. Runs on the render thread (GPU work).
    fn produce(&mut self, ctx: &GpuContext, coord: TileCoord) -> wgpu::Texture;
}
