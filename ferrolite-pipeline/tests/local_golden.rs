mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{CompositeMode, MaskComponent, MaskDefinition, Vec2 as MVec2};
use ferrolite_pipeline::{AdjustmentSet, EditPipeline, LocalAdjustments, MaskLayer, Op, OpStack};
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
