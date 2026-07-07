//! ferrolite-pipeline — the photo edit DAG. An ordered `OpStack` document model
//! and a retained GPU pipeline built on `ferrolite-gpu`'s generic executor; WGSL
//! compute passes implement the edits. Photo tier (GPL-OK).
mod coord;
mod gpu_pyramid;
mod image;
mod lens_bake;
mod lens_gpu;
mod local;
mod local_node;
mod mask_overlay;
mod nodes;
mod op;
mod pipeline;
mod serialize;
mod tile_edit;
mod uniforms;

pub use coord::{display_to_source, source_to_display};
pub use gpu_pyramid::GpuPyramidSource;
pub use image::PipelineImage;
pub use lens_bake::bake_products;
pub use lens_gpu::{VignetteTexture, WarpGridTexture};
pub use local::{
    AdjustmentSet, ColorControl, ColorSwatch, LightControl, LocalAdjustments, MaskLayer,
};
pub use mask_overlay::{overlay_tint, MaskOverlayCompositor, OverlayTexture};
pub use nodes::{color_convert, upload_source};
pub use op::{
    Aspect, Contrast, Correction, CropRect, CurveMode, Exposure, Geometry, Hsl, HslBand,
    LensCorrection, Op, OpKind, OpStack, Sharpen, ToneCurve, WhiteBalance, STACK_VERSION,
};
pub use pipeline::{blit_to_rgba8, EditPipeline};
pub use serialize::{deserialize, serialize};
pub use tile_edit::TileEditPipeline;
// The uniform structs are exported as the documented GPU memory layout the
// edit passes consume; the param→uniform helper fns + math are crate-internal
// (used by `pipeline`/`uniforms`), so they are not part of the public surface.
// Exception: `sharpen_halo`/`lens_halo_px` are public for Plan 3's tile producer.
pub use uniforms::{
    curve_lut, geometry_tile_uniform, lens_halo_px, lens_uniform, sharpen_halo, vignette_amount,
    ContrastUniform, ExposureUniform, GeometryUniform, HslUniform, LensUniform, LocalAdjustUniform,
    SharpenUniform, VignetteUniform, WbUniform, MAX_SHARPEN_RADIUS,
};

/// Pre-compile every edit-pass shader on `ctx` so the first image open reuses
/// cached modules instead of compiling on the UI thread. Call once at startup,
/// alongside the display-pipeline pre-warm. Ten passes: the seven original
/// color/tone/geometry passes, the two lens passes (geometry now carries the
/// warp; `vignette` is the radial-gain pass), plus `local-adjust` (the masked
/// Light+Color point op).
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
        ("vignette", include_str!("shaders/vignette.wgsl")),
        ("local-adjust", include_str!("shaders/local_adjust.wgsl")),
    ] {
        let _ = ctx.shader_module(label, src);
    }
}

/// Output image dimensions after the stack's geometry (crop/rotate) is applied to
/// a `src_w × src_h` source. For an identity/absent geometry op this is the source
/// size. The tiled full-res export renders `ceil(out_w/TILE_SIZE) × ceil(out_h/
/// TILE_SIZE)` tiles in this output space.
pub fn edited_output_dims(stack: &OpStack, src_w: u32, src_h: u32) -> (u32, u32) {
    let (_, out_w, out_h) = crate::uniforms::geometry_uniform(stack.geometry(), src_w, src_h);
    (out_w, out_h)
}

#[cfg(test)]
mod lib_tests {
    use crate::{edited_output_dims, OpStack};

    #[test]
    fn edited_output_dims_identity_equals_source() {
        let stack = OpStack::default();
        assert_eq!(edited_output_dims(&stack, 6000, 4000), (6000, 4000));
    }
}
