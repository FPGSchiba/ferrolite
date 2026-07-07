mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_image::{LinearRgbaF32, TileCoord, TILE_SIZE};
use ferrolite_mask::{CompositeMode, MaskComponent, MaskDefinition, Vec2 as MVec2};
use ferrolite_pipeline::{
    sharpen_halo, AdjustmentSet, EditPipeline, GpuPyramidSource, LocalAdjustments, MaskLayer, Op,
    OpStack, Sharpen, TileEditPipeline,
};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 48;
const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

#[test]
fn radial_exposure_layer_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let la = LocalAdjustments {
        layers: vec![MaskLayer {
            name: "spot".into(),
            visible: true,
            mask: MaskDefinition {
                components: vec![(
                    MaskComponent::RadialGradient {
                        center: MVec2::new(0.5, 0.5),
                        radius: MVec2::new(0.3, 0.3),
                        rotation: 0.0,
                        feather: 0.4,
                        invert: false,
                    },
                    CompositeMode::Add,
                )],
                invert: false,
            },
            adjustments: AdjustmentSet {
                exposure: 1.0,
                ..Default::default()
            },
        }],
    };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "local_radial_exposure.png");
}

#[test]
fn hidden_and_empty_layers_render_identical_to_source() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let src = common::gradient(W, H);
    let base = {
        let mut p = EditPipeline::new(ctx.clone(), &src, OpStack::default(), IDENTITY);
        p.render_to_image()
    };
    // A hidden layer must not change the render.
    let la = LocalAdjustments {
        layers: vec![MaskLayer {
            name: "off".into(),
            visible: false,
            mask: MaskDefinition::default(),
            adjustments: AdjustmentSet {
                exposure: 2.0,
                ..Default::default()
            },
        }],
    };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));
    let mut p = EditPipeline::new(ctx, &src, stack, IDENTITY);
    let got = p.render_to_image();
    assert_eq!(
        common::max_abs_diff(&got, &base),
        0,
        "hidden layer changed the image"
    );
}

#[test]
fn empty_mask_layer_applies_globally() {
    // An empty MaskDefinition = full mask -> exposure applies everywhere; the
    // whole render should differ from the identity render.
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let src = common::gradient(W, H);
    let base = {
        let mut p = EditPipeline::new(ctx.clone(), &src, OpStack::default(), IDENTITY);
        p.render_to_image()
    };
    let la = LocalAdjustments {
        layers: vec![MaskLayer {
            name: "all".into(),
            visible: true,
            mask: MaskDefinition::default(),
            adjustments: AdjustmentSet {
                exposure: 1.0,
                ..Default::default()
            },
        }],
    };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));
    let mut p = EditPipeline::new(ctx, &src, stack, IDENTITY);
    let got = p.render_to_image();
    assert!(
        common::max_abs_diff(&got, &base) > 8,
        "empty mask should apply the adjustment globally"
    );
}

#[test]
fn two_layer_masked_adjustment_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // Layer 1: radial mask, +1 EV exposure. Layer 2: luma-range mask, warm temp.
    let radial = MaskLayer {
        name: "spot".into(),
        visible: true,
        mask: MaskDefinition {
            components: vec![(
                MaskComponent::RadialGradient {
                    center: MVec2::new(0.35, 0.5),
                    radius: MVec2::new(0.25, 0.25),
                    rotation: 0.0,
                    feather: 0.5,
                    invert: false,
                },
                CompositeMode::Add,
            )],
            invert: false,
        },
        adjustments: AdjustmentSet {
            exposure: 1.0,
            ..Default::default()
        },
    };
    let luma = MaskLayer {
        name: "brights".into(),
        visible: true,
        mask: MaskDefinition {
            components: vec![(
                MaskComponent::LumaRange {
                    lo: 0.4,
                    hi: 1.0,
                    softness: 0.1,
                },
                CompositeMode::Add,
            )],
            invert: false,
        },
        adjustments: AdjustmentSet {
            temp: 0.6,
            ..Default::default()
        },
    };
    let la = LocalAdjustments {
        layers: vec![radial, luma],
    };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "two_layer_masked.png");
}

#[test]
fn local_adjust_edit_only_reevaluates_node_and_downstream() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let mut pipe = EditPipeline::new(
        Arc::new(ctx),
        &common::gradient(W, H),
        OpStack::default(),
        IDENTITY,
    );
    let _ = pipe.evaluate();
    let before = pipe.eval_count();
    let la = LocalAdjustments {
        layers: vec![MaskLayer {
            name: "m".into(),
            visible: true,
            mask: MaskDefinition::default(),
            adjustments: AdjustmentSet {
                exposure: 0.5,
                ..Default::default()
            },
        }],
    };
    pipe.set_stack(OpStack::default().set_op(Op::LocalAdjustments(la)));
    let _ = pipe.evaluate();
    let delta = pipe.eval_count() - before;
    // Only LocalAdjustments + Sharpen + Geometry re-run (upstream cached).
    assert_eq!(delta, 3, "expected 3 downstream re-evals, got {delta}");
}

/// Parity test for Task 9: `TileEditPipeline` composites the local-adjustments
/// mask once at full output resolution and each tile samples its sub-region via
/// `set_mask_origin`. For identity geometry the tile (0,0) interior must match
/// the corresponding top-left region of a whole-image `EditPipeline` render
/// (both in scene-linear space, before the display/tone-map + sRGB encode that
/// `render_to_image`/`blit_to_rgba8` would apply).
#[test]
fn tile_masked_adjustment_matches_preview_region_identity_geometry() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // Source larger than one tile so the tile is a genuine sub-region.
    let sw = TILE_SIZE + 40;
    let sh = TILE_SIZE + 24;
    let src = common::gradient(sw, sh);
    let la = LocalAdjustments {
        layers: vec![MaskLayer {
            name: "lin".into(),
            visible: true,
            mask: MaskDefinition {
                components: vec![(
                    MaskComponent::LinearGradient {
                        start: MVec2::new(0.0, 0.0),
                        end: MVec2::new(1.0, 0.0),
                    },
                    CompositeMode::Add,
                )],
                invert: false,
            },
            adjustments: AdjustmentSet {
                exposure: 0.8,
                ..Default::default()
            },
        }],
    };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));

    // Whole-image reference.
    let mut preview = EditPipeline::new(ctx.clone(), &src, stack.clone(), IDENTITY);
    let whole = common::read_image_linear(&ctx, &preview.evaluate());

    // Tile (0,0), identity geometry -> interior TILE_SIZE^2 must match the
    // whole-image top-left TILE_SIZE^2 region within tolerance.
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    let mut tiles = TileEditPipeline::new(ctx.clone(), pyramid, stack, IDENTITY, None, None);
    let tex = tiles.produce_tile(TileCoord { lod: 0, x: 0, y: 0 });
    let tile = common::read_tile_linear(&ctx, &tex);

    let mut max_d = 0.0f32;
    for ty in 0..TILE_SIZE.min(sh) {
        for tx in 0..TILE_SIZE.min(sw) {
            for ch in 0..3 {
                let ti = ((ty * TILE_SIZE + tx) * 4 + ch) as usize;
                let wi = ((ty * sw + tx) * 4 + ch) as usize;
                max_d = max_d.max((tile[ti] - whole[wi]).abs());
            }
        }
    }
    assert!(max_d < 0.02, "tile vs preview region drift {max_d}");
}

/// Regression guard for Task 9's `mask_origin = [coord*TILE_SIZE - halo, ...]`
/// (set in `produce_tile`, `ferrolite-pipeline/src/tile_edit.rs`). The parity
/// test above always runs with `halo == 0` (no Sharpen/lens), so it can never
/// catch a regression that drops the `- self.halo as i32` term. This test adds
/// a non-zero `Sharpen` (amount != 0 && radius != 0 => `sharpen_halo(..) > 0`,
/// see `ferrolite-pipeline/src/uniforms.rs`) alongside a non-identity
/// `LocalAdjustments` layer, so the tile is haloed AND the mask sample must be
/// offset by that halo to land on the correct sub-region. If `- halo` were
/// dropped, the mask would be sampled from the wrong global position and the
/// tile interior would drift from the whole-image reference well beyond the
/// float/driver tolerance used below.
#[test]
fn tile_masked_adjustment_with_sharpen_halo_matches_preview_region() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // Source larger than one tile so the tile is a genuine sub-region.
    let sw = TILE_SIZE + 40;
    let sh = TILE_SIZE + 24;
    let src = common::gradient(sw, sh);
    let la = LocalAdjustments {
        layers: vec![MaskLayer {
            name: "lin".into(),
            visible: true,
            mask: MaskDefinition {
                components: vec![(
                    MaskComponent::LinearGradient {
                        start: MVec2::new(0.0, 0.0),
                        end: MVec2::new(1.0, 0.0),
                    },
                    CompositeMode::Add,
                )],
                invert: false,
            },
            adjustments: AdjustmentSet {
                exposure: 0.8,
                ..Default::default()
            },
        }],
    };
    let sharpen = Sharpen {
        amount: 0.5,
        radius: 3,
    };
    assert!(
        sharpen_halo(Some(sharpen)) > 0,
        "test setup must exercise halo > 0"
    );
    let stack = OpStack::default()
        .set_op(Op::LocalAdjustments(la))
        .set_op(Op::Sharpen(sharpen));

    // Whole-image reference.
    let mut preview = EditPipeline::new(ctx.clone(), &src, stack.clone(), IDENTITY);
    let whole = common::read_image_linear(&ctx, &preview.evaluate());

    // Tile (0,0), identity geometry -> interior TILE_SIZE^2 must match the
    // whole-image top-left TILE_SIZE^2 region within tolerance, even though
    // the sharpen halo makes this a genuinely haloed tile (halo > 0), so the
    // mask must be sampled at `coord*TILE_SIZE - halo` to line up correctly.
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    let mut tiles = TileEditPipeline::new(ctx.clone(), pyramid, stack, IDENTITY, None, None);
    let tex = tiles.produce_tile(TileCoord { lod: 0, x: 0, y: 0 });
    let tile = common::read_tile_linear(&ctx, &tex);

    let mut max_d = 0.0f32;
    for ty in 0..TILE_SIZE.min(sh) {
        for tx in 0..TILE_SIZE.min(sw) {
            for ch in 0..3 {
                let ti = ((ty * TILE_SIZE + tx) * 4 + ch) as usize;
                let wi = ((ty * sw + tx) * 4 + ch) as usize;
                max_d = max_d.max((tile[ti] - whole[wi]).abs());
            }
        }
    }
    assert!(max_d < 0.02, "tile vs preview region drift {max_d}");
}

/// A uniform mid-gray source (display-linear), large enough that `lod = 1`
/// (half-resolution) is still bigger than one `TILE_SIZE`.
fn uniform_gray(w: u32, h: u32, v: f32) -> LinearRgbaF32 {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        px.extend_from_slice(&[v, v, v, 1.0]);
    }
    LinearRgbaF32::new(w, h, px).expect("uniform gray length")
}

/// Regression test for the tiled-mask-LOD bug: `produce_tile` at `lod > 0` used
/// to sample the LOD-0 composited mask using LOD-`lod` (i.e. unscaled) tile
/// coordinates, so a tile at `lod >= 1` only ever saw the top-left
/// `1 / 2^lod` sub-region of the mask. A near-step horizontal-split mask (left
/// half ~0, right half ~1) over a 512x512 source therefore rendered adjusted
/// only in the LEFT half of a `lod = 1` tile (which covers the FULL width at
/// half-res) instead of the RIGHT half — masked edits silently vanished
/// whenever the fit-view idled at `lod >= 1`. After the fix, the tile's mask
/// coordinate is scaled by `2^lod` before sampling the LOD-0 mask, so the
/// right half of the tile (mask ~1) is adjusted and the left half (mask ~0)
/// is not, matching a whole-image render.
#[test]
fn tile_lod1_masked_adjustment_samples_correct_mask_half() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);

    // 512x512 -> level_size(1) = 256x256 = exactly one TILE_SIZE tile.
    let sw = 512u32;
    let sh = 512u32;
    let v = 0.5f32;
    let src = uniform_gray(sw, sh, v);

    // Near-step horizontal split: mask ~0 for x < 0.5 of the LOD-0 width,
    // ~1 for x > 0.5. exposure +2.0 EV -> gain 4.0 (adjust() at identity is
    // just `rgb * exposure_gain`).
    let exposure_ev = 2.0f32;
    let gain = 2.0f32.powf(exposure_ev);
    let la = LocalAdjustments {
        layers: vec![MaskLayer {
            name: "split".into(),
            visible: true,
            mask: MaskDefinition {
                components: vec![(
                    MaskComponent::LinearGradient {
                        start: MVec2::new(0.499, 0.5),
                        end: MVec2::new(0.501, 0.5),
                    },
                    CompositeMode::Add,
                )],
                invert: false,
            },
            adjustments: AdjustmentSet {
                exposure: exposure_ev,
                ..Default::default()
            },
        }],
    };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));

    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    assert_eq!(
        pyramid.level_size(1),
        (256, 256),
        "test assumes lod=1 halves a 512x512 source to exactly one tile"
    );
    let mut tiles = TileEditPipeline::new(ctx.clone(), pyramid, stack, IDENTITY, None, None);
    let tex = tiles.produce_tile(TileCoord { lod: 1, x: 0, y: 0 });
    let tile = common::read_tile_linear(&ctx, &tex);

    let px = |tx: u32, ty: u32, ch: u32| -> f32 { tile[((ty * TILE_SIZE + tx) * 4 + ch) as usize] };

    // Well into the RIGHT half of the lod=1 tile (tile x ~ 230 -> LOD-0 x ~
    // 460): mask ~1, so this pixel must be adjusted (out ~ v * gain).
    let right = px(230, 128, 0);
    assert!(
        (right - v * gain).abs() < 0.1,
        "right-half pixel should be adjusted: got {right}, want ~{}",
        v * gain
    );

    // Well into the LEFT half (tile x ~ 25 -> LOD-0 x ~ 50): mask ~0, so this
    // pixel must stay unadjusted (out ~ v), both before and after the fix.
    let left = px(25, 128, 0);
    assert!(
        (left - v).abs() < 0.1,
        "left-half pixel should be unadjusted: got {left}, want ~{v}"
    );
}
