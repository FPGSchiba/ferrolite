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
mod nr;
mod nr_node;
mod op;
mod patch;
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
pub use nr::{
    atrous_shrink_reference, b3_spline_2d, b3_spline_h, b3_spline_v, nr_halo_px, rgb_to_ycbcr,
    shrink, threshold_at, ycbcr_to_rgb, NR_LEVELS, NR_NOISE_SCALE,
};
pub use op::{
    Aspect, ColorGrade, Contrast, Correction, CropRect, CurveMode, Dehaze, EditDoc, Exposure,
    Geometry, GradeWheel, Hsl, HslBand, LensCorrection, Op, OpKind, OpStack, ParametricCurve,
    PointCurve, Sharpen, ToneCurve, WhiteBalance, STACK_VERSION,
};
pub use patch::{EditPatch, GroupSet, PATCH_VERSION};
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
// `geometry_uniform`/`geometry_src_px` are public as the CPU reference for the
// geometry pass's projective (keystone) mapping — GPU parity tests and any
// future keystone-aware coordinate mapping consume them; `KEYSTONE_STRENGTH`
// is the single named tuning constant for keystone responsiveness (spec C4).
// `nr_uniform`/`NrUniform` stay `pub(crate)` (final-review FIX 9): built and
// consumed entirely inside `nr_node.rs`, with no external consumer — only
// `nr_halo`/`nr_halo_doc` (which `ferrolite-app` actually uses) are exported.
pub use uniforms::{
    clamp_uv_to_crop_bounds, color_grade_px, curve_lut, geometry_src_px, geometry_tile_uniform,
    geometry_uniform, lens_halo_px, lens_uniform, nr_halo, nr_halo_doc, parametric_curve_lut,
    sharpen_halo, sharpen_halo_doc, tone_curve_luts, vignette_amount, ColorGradeUniform,
    GeometryUniform, HslUniform, LensUniform, LocalAdjustUniform, SharpenUniform, VignetteUniform,
    KEYSTONE_STRENGTH, MAX_SHARPEN_RADIUS, SHARPEN_MASK_GRADIENT_NORM,
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
/// (its mip-chain downsample for LOD-aware sampling) — shared by `EditPipeline`
/// and `TileEditPipeline` (QS-Task 4/5). The amount/atmos recovery+blend step
/// (formerly a separate `dehaze-recovery`/`DehazeRecoveryNode` pass) is fused
/// into `local-adjust` below (Phase 4 Task 2) — no longer a standalone shader.
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
        // `dehaze_recovery.wgsl` (the retired standalone recovery pass) stays
        // in-tree as reference math (its per-pixel kernel was ported verbatim
        // into `local_adjust.wgsl`'s `dehaze_recover_step`, Phase 4 Task 2) but
        // is no longer compiled here.
        // `sharpen.wgsl` (the retired fused 2D pass) stays in-tree as
        // reference math (see `sharpen_node.rs`'s doc) but is no longer
        // compiled here — `SharpenNode` (both pipelines) now dispatches the
        // passes below instead. `sharpen-apply-masked` (Phase 4 Task 4) is
        // the per-mask-layer masked apply, only ever dispatched when a
        // visible layer has its own active sharpen. `sharpen-apply-detail`
        // (Phase 4 Task 5) is the Detail/Masking-aware GLOBAL apply, only
        // dispatched when the global op's `detail`/`masking` isn't both zero
        // (the zero case keeps dispatching `sharpen-apply` unchanged, so that
        // path stays byte-exact — see `sharpen_node.rs`'s doc).
        ("sharpen-box-h", include_str!("shaders/sharpen_box_h.wgsl")),
        ("sharpen-box-v", include_str!("shaders/sharpen_box_v.wgsl")),
        ("sharpen-apply", include_str!("shaders/sharpen_apply.wgsl")),
        (
            "sharpen-apply-masked",
            include_str!("shaders/sharpen_apply_masked.wgsl"),
        ),
        (
            "sharpen-apply-detail",
            include_str!("shaders/sharpen_apply_detail.wgsl"),
        ),
        // `nr-clear` (a zero-fill pass that re-zeroed the à trous accumulator
        // every evaluate) is RETIRED and its shader deleted: `nr_atrous.wgsl`'s
        // level 0 now seeds the accumulator directly instead of adding to it,
        // so there is nothing to zero. See `nr_node.rs`'s module doc.
        ("nr-atrous", include_str!("shaders/nr_atrous.wgsl")),
        ("nr-combine", include_str!("shaders/nr_combine.wgsl")),
        ("geometry", include_str!("shaders/geometry.wgsl")),
        ("vignette", include_str!("shaders/vignette.wgsl")),
        ("local-adjust", include_str!("shaders/local_adjust.wgsl")),
    ] {
        let _ = ctx.shader_module(label, src);
    }
}

/// The dummy op stacks [`prewarm_pipelines`] evaluates, one per set of passes
/// that needs its own dispatch to get compiled.
///
/// A pass is only compiled by the driver when it is actually DISPATCHED, and a
/// node that early-returns at identity never dispatches. `OpStack::default()`
/// alone therefore leaves every conditionally-dispatched pass cold, to be
/// compiled on the render thread the first time the user touches its slider —
/// exactly the stall CLAUDE.md's build-once/pre-warm rule exists to prevent.
/// Each entry below exists to force one such group:
///
/// 1. **Default** — the always-dispatched chain (reveal + preview path).
/// 2. **NR + detail/masking sharpen** — `NoiseReductionNode` early-returns
///    unless `is_active()` (needs a nonzero `luminance`/`color` *strength*, not
///    merely `detail`), and `SharpenNode` only dispatches `sharpen-apply-detail`
///    when `amount > 0` AND `detail`/`masking` aren't both zero (the all-zero
///    case dispatches `sharpen-apply` instead, which entry 1 already warms).
///
/// Kept as data, not inlined, so `prewarm_covers_the_conditionally_dispatched_passes`
/// can assert the coverage without needing a GPU adapter.
pub(crate) fn prewarm_stacks() -> Vec<OpStack> {
    let mut nr_and_detail_sharpen = OpStack::default();
    nr_and_detail_sharpen.global.noise_reduction = crate::local::NoiseReduction {
        luminance: 0.5,
        detail: 0.5,
        color: 0.5,
        color_detail: 0.5,
    };
    nr_and_detail_sharpen.global.sharpen = crate::op::Sharpen {
        amount: 0.5,
        radius: 2,
        detail: 0.5,
        masking: 0.5,
    };
    vec![OpStack::default(), nr_and_detail_sharpen]
}

/// Force first-use driver compilation of every edit pipeline at startup by
/// building + evaluating tiny dummy `EditPipeline`/`TileEditPipeline`s. Companion
/// to `prewarm_shaders` (which only compiles shader MODULES): the driver compiles
/// a pipeline on its first DISPATCH, so we must evaluate once here, not merely
/// construct. Startup-only; the dummies are dropped, only the driver's cache
/// persists. Call once, after `prewarm_shaders`, on the render thread.
///
/// Evaluates once per [`prewarm_stacks`] entry — see there for why more than
/// the identity stack is needed.
pub fn prewarm_pipelines(ctx: std::sync::Arc<ferrolite_gpu::GpuContext>) {
    const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    // 64×64 opaque grey dummy — size-independent for compilation.
    let px = vec![0.5f32; 64 * 64 * 4];
    let img = ferrolite_image::LinearRgbaF32::new(64, 64, px).expect("dummy image");
    // One pyramid, reused across stacks — building it is unrelated to which
    // passes each stack warms.
    let pyramid = std::sync::Arc::new(GpuPyramidSource::new(&ctx, &img));

    for stack in prewarm_stacks() {
        // Whole-image edit chain (reveal + preview path).
        let mut ep = EditPipeline::new(ctx.clone(), &img, stack.clone(), IDENTITY);
        let _ = ep.evaluate();

        // Tiled edit chain (full-res producer path: geometry-head + tiled passes).
        let mut tep =
            TileEditPipeline::new(ctx.clone(), pyramid.clone(), stack, IDENTITY, None, None);
        let _ = tep.produce_tile(ferrolite_image::TileCoord { lod: 0, x: 0, y: 0 });
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

    /// Pre-warm must dispatch the passes that a DEFAULT op stack leaves cold,
    /// or the driver compiles them on the render thread on first slider touch
    /// (CLAUDE.md's pre-warm rule). `NoiseReductionNode` early-returns unless
    /// `is_active()`, and `SharpenNode` only reaches `sharpen-apply-detail`
    /// when `amount > 0` and `detail`/`masking` aren't both zero — so the
    /// identity stack alone warms neither.
    ///
    /// No GPU needed: this asserts the warmed op stacks cover those gates,
    /// which is the part that regresses (someone adding a conditionally
    /// dispatched pass and forgetting to warm it). That a stack passing these
    /// gates really does dispatch is proven separately, on a GPU, by
    /// `nr_node::tests::active_nr_dispatches_and_allocates_four_plus_out` and
    /// `sharpen_node`'s detail-apply tests.
    #[test]
    fn prewarm_covers_the_conditionally_dispatched_passes() {
        let stacks = crate::prewarm_stacks();

        assert!(
            stacks.iter().any(|s| s.global.noise_reduction.is_active()),
            "no pre-warm stack activates NR, so nr-atrous/nr-combine are never \
             dispatched at startup and compile on first use"
        );
        assert!(
            stacks.iter().any(|s| {
                let sh = &s.global.sharpen;
                sh.amount > 0.0 && (sh.detail > 0.0 || sh.masking > 0.0)
            }),
            "no pre-warm stack has sharpen amount + detail/masking, so \
             sharpen-apply-detail is never dispatched at startup"
        );
    }

    #[test]
    fn edited_output_dims_identity_equals_source() {
        let stack = OpStack::default();
        assert_eq!(edited_output_dims(&stack, 6000, 4000), (6000, 4000));
    }
}
