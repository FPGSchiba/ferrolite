//! EditDoc (v2) <-> string codec. JSON payload (embedded in the `frl:ops` XMP
//! attribute in Plan 4). Version-checked: v1 payloads and unknown versions
//! deserialize to `None` so the caller can fall back to `OpStack::default()`
//! (unedited); v2 payloads with missing fields load with serde defaults.

use crate::op::OpStack;
use crate::op::STACK_VERSION;

pub fn serialize(stack: &OpStack) -> String {
    serde_json::to_string(stack).expect("OpStack is always serializable")
}

pub fn deserialize(s: &str) -> Option<OpStack> {
    let stack: OpStack = serde_json::from_str(s).ok()?;
    if stack.version != STACK_VERSION {
        return None;
    }
    Some(stack)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::{AdjustmentSet, MaskLayer};
    use crate::op::{Aspect, CropRect, CurveMode, Exposure, Geometry, Op, ToneCurve};

    #[test]
    fn round_trips_a_full_document() {
        let d = OpStack::default()
            .set_op(Op::Exposure(Exposure { ev: 0.75 }))
            .set_op(Op::ToneCurve(ToneCurve {
                points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
                mode: CurveMode::Linear,
                ..Default::default()
            }))
            .set_op(Op::LocalAdjustments(crate::local::LocalAdjustments {
                layers: vec![MaskLayer {
                    name: "Sky".into(),
                    visible: true,
                    mask: Default::default(),
                    adjustments: AdjustmentSet {
                        exposure: -0.5,
                        ..Default::default()
                    },
                }],
            }))
            .set_op(Op::Geometry(Geometry {
                crop: CropRect {
                    x: 0.05,
                    y: 0.05,
                    w: 0.9,
                    h: 0.9,
                },
                angle_deg: 2.5,
                aspect: Aspect::SixteenNine,
                keystone_v: 0.15,
                keystone_h: -0.2,
            }));
        let text = serialize(&d);
        assert_eq!(deserialize(&text), Some(d));
    }

    #[test]
    fn round_trips_the_empty_document() {
        let d = OpStack::default();
        assert_eq!(deserialize(&serialize(&d)), Some(d));
    }

    #[test]
    fn v1_payload_is_none_bytes_untouched_semantics() {
        // A real pre-EditDoc payload (version 1, Vec<Op> shape): must load as None
        // so callers fall back to "no edits" — never a parse panic, never a
        // half-migrated doc.
        let v1 = r#"{"version":1,"ops":[{"Exposure":{"ev":0.5}}]}"#;
        assert_eq!(deserialize(v1), None);
    }

    #[test]
    fn future_version_is_none() {
        let json = r#"{"version":999,"global":{},"layers":[]}"#;
        assert_eq!(deserialize(json), None);
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(deserialize("not json {{"), None);
    }

    #[test]
    fn missing_new_fields_load_as_identity() {
        // Forward tolerance within v2: a minimal v2 payload (older v2 build,
        // fewer fields) loads with serde defaults.
        let json = r#"{"version":2}"#;
        let d = deserialize(json).unwrap();
        assert!(d.is_identity());
    }
}
