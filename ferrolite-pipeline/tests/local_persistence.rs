//! `frl:ops` persistence for LocalAdjustments — pure/IO, runs on every OS in CI.
//!
//! Proves the sidecar path is transparent to `Op::LocalAdjustments`: no
//! `op.rs`/`serialize.rs`/`xmp.rs` code change is required, because the sidecar
//! payload is the whole `OpStack` JSON and `Op` serializes by variant name.

use ferrolite_mask::{
    CompositeMode, MaskComponent, MaskDefinition, MaskProvenance, RasterHandle, Vec2 as MVec2,
};
use ferrolite_pipeline::{
    deserialize, serialize, AdjustmentSet, ColorSwatch, LocalAdjustments, MaskLayer, Op, OpStack,
};

fn sample_stack() -> OpStack {
    let la = LocalAdjustments {
        layers: vec![
            MaskLayer {
                name: "sky".into(),
                visible: true,
                mask: MaskDefinition {
                    components: vec![
                        (
                            MaskComponent::LinearGradient {
                                start: MVec2::new(0.0, 0.0),
                                end: MVec2::new(0.0, 1.0),
                            },
                            CompositeMode::Add,
                        ),
                        (
                            MaskComponent::Imported {
                                handle: RasterHandle(7),
                                provenance: MaskProvenance {
                                    model_id: "sam2.1".into(),
                                    model_version: "1".into(),
                                    prompt: "click:0.5,0.5".into(),
                                },
                            },
                            CompositeMode::Subtract,
                        ),
                    ],
                    invert: false,
                },
                adjustments: AdjustmentSet {
                    exposure: -0.4,
                    temp: 0.5,
                    color: ColorSwatch {
                        r: 0.1,
                        g: 0.2,
                        b: 0.9,
                        amount: 0.3,
                    },
                    ..Default::default()
                },
            },
            MaskLayer {
                name: "brush".into(),
                visible: false,
                mask: MaskDefinition::default(),
                adjustments: AdjustmentSet {
                    contrast: 0.2,
                    ..Default::default()
                },
            },
        ],
    };
    OpStack::default()
        .set_op(Op::Exposure(ferrolite_pipeline::Exposure { ev: 0.25 }))
        .set_op(Op::LocalAdjustments(la))
}

#[test]
fn local_adjustments_round_trips_through_serialize() {
    let s = sample_stack();
    assert_eq!(deserialize(&serialize(&s)), Some(s));
}

#[test]
fn missing_local_adjustments_op_loads_as_none() {
    let json = r#"{"version":2,"global":{"exposure":0.5},"layers":[]}"#;
    let s = deserialize(json).unwrap();
    assert!(s.local_adjustments().is_none());
}

#[test]
fn adjustment_set_missing_fields_load_as_identity() {
    // A future/older payload with only some fields present.
    let json = r#"{"version":2,"layers":[
        {"name":"m","visible":true,"mask":{"components":[],"invert":false},
         "adjustments":{"exposure":1.0}}]}"#;
    let s = deserialize(json).unwrap();
    let la = s.local_adjustments().unwrap();
    let a = &la.layers[0].adjustments;
    assert_eq!(a.exposure, 1.0);
    assert_eq!(a.contrast, 0.0, "absent field → identity via serde default");
    assert_eq!(a.color.amount, 0.0);
}

#[test]
fn xmp_write_read_round_trips_local_adjustments_and_preserves_foreign_nodes() {
    let dir = std::env::temp_dir().join(format!("frl-p3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("img.xmp");
    let payload = serialize(&sample_stack());
    ferrolite_catalog::write_ops(&p, &payload).unwrap();
    // Rating written after must not clobber frl:ops.
    ferrolite_catalog::write_rating(&p, ferrolite_catalog::Rating::new(4)).unwrap();
    let read = ferrolite_catalog::read_ops(&p).unwrap();
    assert_eq!(deserialize(&read), Some(sample_stack()));
    assert_eq!(
        ferrolite_catalog::read_rating(&p),
        Some(ferrolite_catalog::Rating::new(4))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn legacy_shapes_only_local_adjustments_still_loads() {
    // A LocalAdjustments payload authored before the Imported variant existed.
    let json = r#"{"version":2,"layers":[
        {"name":"sky","visible":true,
         "mask":{"components":[[{"LinearGradient":{"start":{"x":0.0,"y":0.0},"end":{"x":0.0,"y":1.0}}},"Add"]],"invert":false},
         "adjustments":{"exposure":-0.4}}]}"#;
    let s = deserialize(json).expect("v2 frl:ops decodes");
    let la = s.local_adjustments().expect("has local adjustments");
    assert_eq!(la.layers.len(), 1);
    assert_eq!(la.layers[0].mask.components.len(), 1);
    assert_eq!(la.layers[0].adjustments.exposure, -0.4);
}

#[test]
fn imported_provenance_unknown_field_tolerated_in_frl_ops() {
    // A future frl:ops with an extra provenance field must load on today's build,
    // proving A2 can extend MaskProvenance without a sidecar schema break.
    let json = r#"{"version":2,"layers":[
        {"name":"subject","visible":true,
         "mask":{"components":[
            [{"Imported":{"handle":7,"provenance":{"model_id":"sam2.1","model_version":"1","prompt":"click:0.5,0.5","future_score":0.9}}},"Add"]
         ],"invert":false},
         "adjustments":{"exposure":0.2}}]}"#;
    let s = deserialize(json).expect("v2 frl:ops with extra provenance field decodes");
    let la = s.local_adjustments().unwrap();
    match &la.layers[0].mask.components[0].0 {
        MaskComponent::Imported { handle, provenance } => {
            assert_eq!(*handle, RasterHandle(7));
            assert_eq!(provenance.prompt, "click:0.5,0.5");
        }
        other => panic!("expected Imported, got {other:?}"),
    }
}
