//! ferrolite-pipeline — the photo edit DAG. An ordered `OpStack` document model
//! and a retained GPU pipeline built on `ferrolite-gpu`'s generic executor; WGSL
//! compute passes implement the edits. Photo tier (GPL-OK).
mod gpu_pyramid;
mod image;
mod nodes;
mod op;
mod pipeline;
mod serialize;
mod tile_edit;
mod uniforms;

pub use gpu_pyramid::GpuPyramidSource;
pub use image::PipelineImage;
pub use nodes::{color_convert, upload_source};
pub use op::{
    Aspect, Contrast, CropRect, Exposure, Geometry, Hsl, HslBand, Op, OpKind, OpStack, Sharpen,
    ToneCurve, WhiteBalance, STACK_VERSION,
};
pub use pipeline::{blit_to_rgba8, EditPipeline};
pub use serialize::{deserialize, serialize};
pub use tile_edit::TileEditPipeline;
// The uniform structs are exported as the documented GPU memory layout the
// edit passes consume; the param→uniform helper fns + math are crate-internal
// (used by `pipeline`/`uniforms`), so they are not part of the public surface.
// Exception: `sharpen_halo` is part of the public API for Plan 3's tile producer.
pub use uniforms::{
    geometry_tile_uniform, sharpen_halo, ContrastUniform, ExposureUniform, GeometryUniform,
    HslUniform, SharpenUniform, WbUniform, MAX_SHARPEN_RADIUS,
};

/// Pre-compile every edit-pass shader on `ctx` so the first image open reuses
/// cached modules instead of compiling on the UI thread. Call once at startup,
/// alongside the display-pipeline pre-warm.
pub fn prewarm_shaders(ctx: &ferrolite_gpu::GpuContext) {
    for (label, src) in [
        ("color-matrix", include_str!("shaders/color_matrix.wgsl")),
        ("exposure", include_str!("shaders/exposure.wgsl")),
        ("white-balance", include_str!("shaders/white_balance.wgsl")),
        ("contrast", include_str!("shaders/contrast.wgsl")),
        ("tone-curve", include_str!("shaders/tone_curve.wgsl")),
        ("hsl", include_str!("shaders/hsl.wgsl")),
        ("sharpen", include_str!("shaders/sharpen.wgsl")),
        ("geometry", include_str!("shaders/geometry.wgsl")),
    ] {
        let _ = ctx.shader_module(label, src);
    }
}
