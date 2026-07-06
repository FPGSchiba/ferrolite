//! Forward-compatibility / additivity proof for the AI-mask seam (design §8, §11).
//! Adding `MaskComponent::Imported { handle, provenance }` must NOT break existing
//! variants or the schema, and A2 must be able to extend it additively. Contract 2:
//! only the prompt (provenance) persists — never a raster.

use ferrolite_mask::{
    CompositeMode, MaskComponent, MaskDefinition, MaskProvenance, RasterHandle, Vec2,
};

/// A definition authored BEFORE the `Imported` variant existed (shapes only) must
/// still deserialize on the current build — proving the variant addition is additive
/// and did not change how existing variants encode (externally-tagged by variant name).
#[test]
fn legacy_shapes_only_definition_still_deserializes() {
    let legacy = r#"{
        "components": [
            [{"LinearGradient": {"start": {"x": 0.0, "y": 0.0}, "end": {"x": 0.0, "y": 1.0}}}, "Add"],
            [{"LumaRange": {"lo": 0.2, "hi": 0.7, "softness": 0.1}}, "Subtract"]
        ],
        "invert": false
    }"#;
    let def: MaskDefinition = serde_json::from_str(legacy).expect("legacy shapes-only decodes");
    assert_eq!(def.components.len(), 2);
    assert_eq!(
        def.components[0].0,
        MaskComponent::LinearGradient {
            start: Vec2::new(0.0, 0.0),
            end: Vec2::new(0.0, 1.0),
        }
    );
    assert_eq!(def.components[1].1, CompositeMode::Subtract);
    assert!(!def.invert);
}

/// A future build may add fields to `MaskProvenance`. Serde ignores unknown fields by
/// default (we must never add `deny_unknown_fields`), so an extended payload still
/// loads on today's build with the known fields intact — A2 can grow provenance.
#[test]
fn imported_provenance_tolerates_unknown_future_fields() {
    let future = r#"{
        "Imported": {
            "handle": 42,
            "provenance": {
                "model_id": "segnext",
                "model_version": "2.0",
                "prompt": "box:0.1,0.2,0.8,0.9",
                "confidence": 0.97,
                "future_field": {"nested": true}
            }
        }
    }"#;
    let comp: MaskComponent = serde_json::from_str(future).expect("unknown fields tolerated");
    match comp {
        MaskComponent::Imported { handle, provenance } => {
            assert_eq!(handle, RasterHandle(42));
            assert_eq!(provenance.model_id, "segnext");
            assert_eq!(provenance.model_version, "2.0");
            assert_eq!(provenance.prompt, "box:0.1,0.2,0.8,0.9");
        }
        other => panic!("expected Imported, got {other:?}"),
    }
}

/// The engine stores the prompt verbatim and never interprets it. Any opaque prompt
/// encoding (clicks / box / semantic class) round-trips byte-identically.
#[test]
fn provenance_prompt_is_stored_verbatim() {
    for prompt in [
        "click:0.5,0.5;0.25,0.75",
        "box:0.1,0.2,0.8,0.9",
        "semantic:sky",
        "", // empty prompt is still valid opaque data
    ] {
        let def = MaskDefinition {
            components: vec![(
                MaskComponent::Imported {
                    handle: RasterHandle(1),
                    provenance: MaskProvenance {
                        model_id: "sam2.1".into(),
                        model_version: "1.0".into(),
                        prompt: prompt.into(),
                    },
                },
                CompositeMode::Add,
            )],
            invert: false,
        };
        let back: MaskDefinition =
            serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
        assert_eq!(def, back, "prompt {prompt:?} round-trips verbatim");
    }
}

/// Contract 2: the serialized `Imported` component carries the PROMPT (provenance),
/// not a raster. Only `handle` (a u64 id) + `provenance` are present — no pixel data.
#[test]
fn serialized_imported_carries_prompt_not_raster() {
    let def = MaskDefinition {
        components: vec![(
            MaskComponent::Imported {
                handle: RasterHandle(7),
                provenance: MaskProvenance {
                    model_id: "sam2.1".into(),
                    model_version: "1.0".into(),
                    prompt: "click:0.5,0.5".into(),
                },
            },
            CompositeMode::Add,
        )],
        invert: false,
    };
    let json = serde_json::to_string(&def).unwrap();
    assert!(
        json.contains("\"prompt\":\"click:0.5,0.5\""),
        "prompt persists"
    );
    assert!(json.contains("\"handle\":7"), "handle is a plain id");
    // No raster/pixel payload exists in the model to serialize.
    assert!(!json.contains("raster"), "no raster field");
    assert!(!json.contains("pixels"), "no pixel data");
    assert!(!json.contains("texture"), "no texture data");
}
