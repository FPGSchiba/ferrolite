//! ferrolite-pipeline — the photo edit DAG. An ordered `OpStack` document model
//! and a retained GPU pipeline built on `ferrolite-gpu`'s generic executor; WGSL
//! compute passes implement the edits. Photo tier (GPL-OK).
mod coord;
mod dehaze;
mod dehaze_node;
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
mod rcd_gpu;
mod serialize;
mod sharpen_node;
mod tile_edit;
mod uniforms;

pub use coord::{display_to_source, source_to_display};
pub use dehaze::{
    dehaze_halo, dehaze_recover, estimate_atmospheric_light, guided_radius, transmission_map,
    transmission_mip_level_count, transmission_sample_lod, transmission_working_dims,
    DEHAZE_ATMOS_NEUTRAL, DEHAZE_DEFAULT_RADIUS, DEHAZE_GUIDED_EPS, DEHAZE_MAX_TRANSMISSION_DIM,
    MAX_DEHAZE_RADIUS,
};
pub use gpu_pyramid::{live_gpu_pyramid_bytes, live_gpu_pyramids, GpuPyramidSource};
pub use image::PipelineImage;
pub use lens_bake::bake_products;
pub use lens_gpu::{VignetteTexture, WarpGridTexture};
pub use local::{
    AdjustmentSet, ColorControl, ColorSwatch, LightControl, LocalAdjustments, MaskLayer,
    NoiseReduction,
};
// `EngineStage` is the fused layer-engine node's stage discriminant (Task 3
// wires two `LocalAdjustmentsNode` instances, one per stage, into the
// pipelines) — re-exported from the otherwise crate-private `local_node`
// module.
pub use local_node::EngineStage;
pub use mask_overlay::{overlay_tint, MaskOverlayCompositor, OverlayTexture};
pub use nodes::{color_convert, upload_source};
pub use op::{
    Aspect, ColorGrade, Contrast, Correction, CropRect, CurveMode, Dehaze, EditDoc, Exposure,
    Geometry, GradeWheel, Hsl, HslBand, LensCorrection, Op, OpKind, OpStack, ParametricCurve,
    PointCurve, Sharpen, ToneCurve, WhiteBalance, STACK_VERSION,
};
pub use pipeline::{blit_to_rgba8, blit_to_rgba8_with_matrix, EditPipeline};
pub use rcd_gpu::{demosaic_rcd_gpu, CfaInput};
pub use serialize::{deserialize, serialize};
pub use tile_edit::TileEditPipeline;
// The uniform structs are exported as the documented GPU memory layout the
// edit passes consume. Most param→uniform helpers are crate-internal; the pure
// reusable transforms (`color_grade_px`, `curve_lut`, `parametric_curve_lut`, `tone_curve_luts`)
// are public per design §2.5 so the future per-mask path reuses them with no rework.
// `sharpen_halo`/`lens_halo_px` are public for Plan 3's tile producer.
// `ExposureUniform`/`WbUniform`/`ContrastUniform` retired with Task 3 (Phase 3
// fused layer engine): the standalone exposure/white-balance/contrast passes
// they backed are gone — `local_adjust_uniform`/`LocalAdjustUniform` cover the
// same math for both the Light-stage engine node and per-mask layers now.
pub use uniforms::{
    color_grade_px, curve_lut, geometry_tile_uniform, lens_halo_px, lens_uniform,
    parametric_curve_lut, sharpen_halo, tone_curve_luts, vignette_amount, ColorGradeUniform,
    GeometryUniform, HslUniform, LensUniform, LocalAdjustUniform, SharpenUniform, VignetteUniform,
    MAX_SHARPEN_RADIUS,
};

/// Pre-compile every edit-pass shader on `ctx` so the first image open reuses
/// cached modules instead of compiling on the UI thread. Call once at startup,
/// alongside the display-pipeline pre-warm. Covers `color-matrix`/
/// `geometry`/`vignette` (the surviving standalone point/geometry passes),
/// `sharpen-box-h`/`-box-v`/`-apply` (the Phase 4 separable `SharpenNode`
/// three-pass replacement for the old fused `sharpen.wgsl`, which stays
/// in-tree as reference math but is no longer compiled here),
/// `local-adjust` (the fused Light+Color engine — one shader now covers what
/// used to be six standalone passes: exposure/white-balance/contrast/
/// tone-curve/hsl/color-grade, retired as graph nodes by the Phase 3 fused
/// layer engine; their `.wgsl` files stay in-tree as reference math for
/// `local_adjust.wgsl`'s per-op ports, just no longer compiled here), and the
/// dehaze passes: `dehaze-dark-channel`/`-min-h`/`-min-v`/`-products`/
/// `-box-h`/`-box-v`/`-guided-ab`/`-guided-q` (the multi-pass guided-filter
/// transmission map, `DehazeTransmissionNode`) — plus `dehaze-transmission-mip`
/// (its mip-chain downsample for LOD-aware sampling) — plus `dehaze-recovery` (the
/// amount/atmos blend, `DehazeRecoveryNode`) — both nodes shared by
/// `EditPipeline` and `TileEditPipeline` (QS-Task 4/5).
pub fn prewarm_shaders(ctx: &ferrolite_gpu::GpuContext) {
    for (label, src) in [
        ("color-matrix", include_str!("shaders/color_matrix.wgsl")),
        (
            "dehaze-dark-channel",
            include_str!("shaders/dehaze_dark_channel.wgsl"),
        ),
        ("dehaze-min-h", include_str!("shaders/dehaze_min_h.wgsl")),
        ("dehaze-min-v", include_str!("shaders/dehaze_min_v.wgsl")),
        (
            "dehaze-products",
            include_str!("shaders/dehaze_products.wgsl"),
        ),
        ("dehaze-box-h", include_str!("shaders/dehaze_box_h.wgsl")),
        ("dehaze-box-v", include_str!("shaders/dehaze_box_v.wgsl")),
        (
            "dehaze-guided-ab",
            include_str!("shaders/dehaze_guided_ab.wgsl"),
        ),
        (
            "dehaze-guided-q",
            include_str!("shaders/dehaze_guided_q.wgsl"),
        ),
        (
            "dehaze-transmission-mip",
            include_str!("shaders/dehaze_transmission_mip.wgsl"),
        ),
        (
            "dehaze-recovery",
            include_str!("shaders/dehaze_recovery.wgsl"),
        ),
        // `sharpen.wgsl` (the retired fused 2D pass) stays in-tree as
        // reference math (see `sharpen_node.rs`'s doc) but is no longer
        // compiled here — `SharpenNode` (both pipelines) now dispatches the
        // three passes below instead.
        ("sharpen-box-h", include_str!("shaders/sharpen_box_h.wgsl")),
        ("sharpen-box-v", include_str!("shaders/sharpen_box_v.wgsl")),
        ("sharpen-apply", include_str!("shaders/sharpen_apply.wgsl")),
        ("geometry", include_str!("shaders/geometry.wgsl")),
        ("vignette", include_str!("shaders/vignette.wgsl")),
        ("local-adjust", include_str!("shaders/local_adjust.wgsl")),
    ] {
        let _ = ctx.shader_module(label, src);
    }
}

/// Force first-use driver compilation of every edit pipeline at startup by
/// building + evaluating tiny dummy `EditPipeline`/`TileEditPipeline`s. Companion
/// to `prewarm_shaders` (which only compiles shader MODULES): the driver compiles
/// a pipeline on its first DISPATCH, so we must evaluate once here, not merely
/// construct. Startup-only; the dummies are dropped, only the driver's cache
/// persists. Call once, after `prewarm_shaders`, on the render thread.
pub fn prewarm_pipelines(ctx: std::sync::Arc<ferrolite_gpu::GpuContext>) {
    const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    // 64×64 opaque grey dummy — size-independent for compilation.
    let px = vec![0.5f32; 64 * 64 * 4];
    let img = ferrolite_image::LinearRgbaF32::new(64, 64, px).expect("dummy image");

    // Whole-image edit chain (reveal + preview path).
    let mut ep = EditPipeline::new(ctx.clone(), &img, OpStack::default(), IDENTITY);
    let _ = ep.evaluate();

    // Tiled edit chain (full-res producer path: geometry-head + tiled passes).
    let pyramid = std::sync::Arc::new(GpuPyramidSource::new(&ctx, &img));
    let mut tep = TileEditPipeline::new(ctx, pyramid, OpStack::default(), IDENTITY, None, None);
    let _ = tep.produce_tile(ferrolite_image::TileCoord { lod: 0, x: 0, y: 0 });
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
