//! Op-stack <-> string codec. JSON payload (embedded in the `frl:ops` XMP
//! attribute in Plan 4). Version-checked: an unknown version deserializes to
//! `None` so the caller can fall back to `OpStack::default()` (unedited).

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
    use crate::op::{
        Aspect, Contrast, CropRect, Exposure, Geometry, Hsl, HslBand, Op, Sharpen, ToneCurve,
        WhiteBalance,
    };

    #[test]
    fn round_trips_a_full_stack() {
        let s = OpStack::default()
            .set_op(Op::Exposure(Exposure { ev: 0.75 }))
            .set_op(Op::WhiteBalance(WhiteBalance {
                temp: 0.2,
                tint: -0.1,
            }))
            .set_op(Op::Contrast(Contrast { amount: 0.3 }));
        let text = serialize(&s);
        assert_eq!(deserialize(&text), Some(s));
    }

    #[test]
    fn round_trips_the_empty_stack() {
        let s = OpStack::default();
        assert_eq!(deserialize(&serialize(&s)), Some(s));
    }

    #[test]
    fn unknown_version_is_none() {
        // A well-formed stack but with a future version.
        let json = r#"{"version":999,"ops":[]}"#;
        assert_eq!(deserialize(json), None);
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(deserialize("not json {{"), None);
    }

    #[test]
    fn round_trips_all_seven_ops() {
        let s = OpStack::default()
            .set_op(Op::Exposure(Exposure { ev: 0.5 }))
            .set_op(Op::WhiteBalance(WhiteBalance {
                temp: 0.2,
                tint: -0.1,
            }))
            .set_op(Op::Contrast(Contrast { amount: 0.3 }))
            .set_op(Op::ToneCurve(ToneCurve {
                points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
            }))
            .set_op(Op::Hsl(Hsl {
                bands: [HslBand {
                    hue: 0.1,
                    sat: -0.2,
                    lum: 0.05,
                }; 8],
            }))
            .set_op(Op::Sharpen(Sharpen {
                amount: 0.6,
                radius: 3,
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
            }));
        let text = serialize(&s);
        assert_eq!(deserialize(&text), Some(s));
    }
}
