//! The edit document model: an ordered `OpStack` of point/parametric ops. Pure
//! data — no GPU. This is the unit of undo/redo (later plan) and the payload
//! persisted to the `.xmp` sidecar (Plan 4). Apply order is the fixed canonical
//! op order (the `OpKind` discriminant order); the `Vec` is kept sorted by it.

use serde::{Deserialize, Serialize};

/// Current on-stack schema version. Bumped if `Op`'s shape changes incompatibly.
pub const STACK_VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Exposure {
    /// Exposure adjustment in stops (EV). 0 = identity.
    pub ev: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct WhiteBalance {
    /// Normalized temperature in [-1, 1] (warm positive). 0 = identity.
    pub temp: f32,
    /// Normalized tint in [-1, 1] (magenta positive). 0 = identity.
    pub tint: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Contrast {
    /// Bipolar contrast amount in [-1, 1]. 0 = identity.
    pub amount: f32,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ToneCurve {
    /// Control points in [0,1]×[0,1] (x ascending). Identity = `[(0,0),(1,1)]`
    /// or empty. Baked to a 256-entry monotone LUT by `uniforms::curve_lut`.
    pub points: Vec<(f32, f32)>,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct HslBand {
    /// Hue shift, normalized [-1, 1]. 0 = identity.
    pub hue: f32,
    /// Saturation delta, normalized [-1, 1]. 0 = identity.
    pub sat: f32,
    /// Lightness delta, normalized [-1, 1]. 0 = identity.
    pub lum: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Hsl {
    /// Per-band deltas; bands = red, orange, yellow, green, aqua, blue,
    /// purple, magenta (the canonical 8-band order). All-zero = identity.
    pub bands: [HslBand; 8],
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Sharpen {
    /// Unsharp-mask amount (>= 0). 0 = identity.
    pub amount: f32,
    /// Box-blur radius in pixels (drives the halo size in Plan 3). 0 = identity.
    pub radius: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Aspect {
    Original,
    Free,
    Square,
    ThreeTwo,
    FourThree,
    SixteenNine,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct CropRect {
    /// Normalized crop in source space: (x, y) top-left, (w, h) extent, all [0,1].
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl CropRect {
    /// The whole image (no crop).
    pub fn full() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Geometry {
    pub crop: CropRect,
    /// Rotation in degrees about the crop center. 0 = identity.
    pub angle_deg: f32,
    pub aspect: Aspect,
}

/// One adjustment in the stack. `Op` is `Clone` (not `Copy`) because `ToneCurve`
/// carries a `Vec` of control points.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Op {
    Exposure(Exposure),
    WhiteBalance(WhiteBalance),
    Contrast(Contrast),
    ToneCurve(ToneCurve),
    Hsl(Hsl),
    Sharpen(Sharpen),
    Geometry(Geometry),
}

/// Canonical op identity + apply order (the discriminant order is the order ops
/// are applied in the pipeline chain).
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    Exposure = 0,
    WhiteBalance = 1,
    Contrast = 2,
    ToneCurve = 3,
    Hsl = 4,
    Sharpen = 5,
    Geometry = 6,
}

impl Op {
    pub fn kind(&self) -> OpKind {
        match self {
            Op::Exposure(_) => OpKind::Exposure,
            Op::WhiteBalance(_) => OpKind::WhiteBalance,
            Op::Contrast(_) => OpKind::Contrast,
            Op::ToneCurve(_) => OpKind::ToneCurve,
            Op::Hsl(_) => OpKind::Hsl,
            Op::Sharpen(_) => OpKind::Sharpen,
            Op::Geometry(_) => OpKind::Geometry,
        }
    }
}

/// An ordered, immutable stack of edits. `set_op`/`reset` return new stacks.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct OpStack {
    pub version: u32,
    pub ops: Vec<Op>,
}

impl Default for OpStack {
    fn default() -> Self {
        Self {
            version: STACK_VERSION,
            ops: Vec::new(),
        }
    }
}

impl OpStack {
    /// No ops = unedited (renders identically to the source).
    pub fn is_identity(&self) -> bool {
        self.ops.is_empty()
    }

    /// Return a new stack with `op` set: replaces any existing op of the same
    /// kind, keeps the `Vec` sorted in canonical (`OpKind`) order.
    pub fn set_op(&self, op: Op) -> OpStack {
        let k = op.kind();
        let mut ops: Vec<Op> = self.ops.iter().filter(|o| o.kind() != k).cloned().collect();
        ops.push(op);
        ops.sort_by_key(|o| o.kind() as u8);
        OpStack {
            version: self.version,
            ops,
        }
    }

    /// Return a new stack with any op of `kind` removed (per-op reset).
    pub fn reset(&self, kind: OpKind) -> OpStack {
        OpStack {
            version: self.version,
            ops: self
                .ops
                .iter()
                .filter(|o| o.kind() != kind)
                .cloned()
                .collect(),
        }
    }

    pub fn exposure(&self) -> Option<Exposure> {
        self.ops.iter().find_map(|o| match o {
            Op::Exposure(e) => Some(*e),
            _ => None,
        })
    }

    pub fn white_balance(&self) -> Option<WhiteBalance> {
        self.ops.iter().find_map(|o| match o {
            Op::WhiteBalance(w) => Some(*w),
            _ => None,
        })
    }

    pub fn contrast(&self) -> Option<Contrast> {
        self.ops.iter().find_map(|o| match o {
            Op::Contrast(c) => Some(*c),
            _ => None,
        })
    }

    pub fn tone_curve(&self) -> Option<ToneCurve> {
        self.ops.iter().find_map(|o| match o {
            Op::ToneCurve(t) => Some(t.clone()),
            _ => None,
        })
    }

    pub fn hsl(&self) -> Option<Hsl> {
        self.ops.iter().find_map(|o| match o {
            Op::Hsl(h) => Some(*h),
            _ => None,
        })
    }

    pub fn sharpen(&self) -> Option<Sharpen> {
        self.ops.iter().find_map(|o| match o {
            Op::Sharpen(s) => Some(*s),
            _ => None,
        })
    }

    pub fn geometry(&self) -> Option<Geometry> {
        self.ops.iter().find_map(|o| match o {
            Op::Geometry(g) => Some(*g),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_identity_and_empty() {
        let s = OpStack::default();
        assert_eq!(s.version, STACK_VERSION);
        assert!(s.is_identity());
        assert!(s.ops.is_empty());
    }

    #[test]
    fn set_op_is_immutable_and_adds() {
        let base = OpStack::default();
        let next = base.set_op(Op::Exposure(Exposure { ev: 0.5 }));
        assert!(base.is_identity(), "original stack unchanged (immutable)");
        assert_eq!(next.exposure(), Some(Exposure { ev: 0.5 }));
        assert_eq!(next.ops.len(), 1);
    }

    #[test]
    fn set_op_same_kind_replaces() {
        let s = OpStack::default()
            .set_op(Op::Exposure(Exposure { ev: 0.5 }))
            .set_op(Op::Exposure(Exposure { ev: -1.0 }));
        assert_eq!(s.ops.len(), 1, "same kind replaced, not appended");
        assert_eq!(s.exposure(), Some(Exposure { ev: -1.0 }));
    }

    #[test]
    fn ops_stay_in_canonical_order() {
        let s = OpStack::default()
            .set_op(Op::Contrast(Contrast { amount: 0.2 }))
            .set_op(Op::Exposure(Exposure { ev: 0.1 }))
            .set_op(Op::WhiteBalance(WhiteBalance {
                temp: 0.0,
                tint: 0.0,
            }));
        let kinds: Vec<OpKind> = s.ops.iter().map(|o| o.kind()).collect();
        assert_eq!(
            kinds,
            vec![OpKind::Exposure, OpKind::WhiteBalance, OpKind::Contrast]
        );
    }

    #[test]
    fn reset_removes_one_kind() {
        let s = OpStack::default()
            .set_op(Op::Exposure(Exposure { ev: 0.5 }))
            .set_op(Op::Contrast(Contrast { amount: 0.2 }))
            .reset(OpKind::Exposure);
        assert_eq!(s.exposure(), None);
        assert_eq!(s.contrast(), Some(Contrast { amount: 0.2 }));
    }

    #[test]
    fn new_ops_round_through_set_and_accessors() {
        let s = OpStack::default()
            .set_op(Op::ToneCurve(ToneCurve {
                points: vec![(0.0, 0.0), (1.0, 1.0)],
            }))
            .set_op(Op::Hsl(Hsl {
                bands: [HslBand {
                    hue: 0.1,
                    sat: 0.0,
                    lum: 0.0,
                }; 8],
            }))
            .set_op(Op::Sharpen(Sharpen {
                amount: 0.5,
                radius: 2,
            }))
            .set_op(Op::Geometry(Geometry {
                crop: CropRect {
                    x: 0.1,
                    y: 0.1,
                    w: 0.8,
                    h: 0.8,
                },
                angle_deg: 5.0,
                aspect: Aspect::Free,
            }));
        assert_eq!(s.tone_curve().unwrap().points.len(), 2);
        assert_eq!(s.hsl().unwrap().bands[0].hue, 0.1);
        assert_eq!(
            s.sharpen(),
            Some(Sharpen {
                amount: 0.5,
                radius: 2
            })
        );
        assert_eq!(s.geometry().unwrap().angle_deg, 5.0);
    }

    #[test]
    fn full_seven_op_stack_is_in_canonical_order() {
        let s = OpStack::default()
            .set_op(Op::Geometry(Geometry {
                crop: CropRect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                },
                angle_deg: 0.0,
                aspect: Aspect::Original,
            }))
            .set_op(Op::Sharpen(Sharpen {
                amount: 0.3,
                radius: 1,
            }))
            .set_op(Op::Hsl(Hsl {
                bands: [HslBand {
                    hue: 0.0,
                    sat: 0.0,
                    lum: 0.0,
                }; 8],
            }))
            .set_op(Op::ToneCurve(ToneCurve { points: vec![] }))
            .set_op(Op::Contrast(Contrast { amount: 0.1 }))
            .set_op(Op::WhiteBalance(WhiteBalance {
                temp: 0.0,
                tint: 0.0,
            }))
            .set_op(Op::Exposure(Exposure { ev: 0.1 }));
        let kinds: Vec<OpKind> = s.ops.iter().map(|o| o.kind()).collect();
        assert_eq!(
            kinds,
            vec![
                OpKind::Exposure,
                OpKind::WhiteBalance,
                OpKind::Contrast,
                OpKind::ToneCurve,
                OpKind::Hsl,
                OpKind::Sharpen,
                OpKind::Geometry,
            ]
        );
    }
}
