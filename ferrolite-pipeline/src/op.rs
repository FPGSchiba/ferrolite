//! The edit document model (v2): a struct `EditDoc` with a global `AdjustmentSet`,
//! a stack of `MaskLayer`s (each pairing a mask with local adjustments), and
//! global-only fields `lens` + `geometry`. Pure data — no GPU. This is the unit
//! of undo/redo (later plan) and the payload persisted to the `.xmp` sidecar
//! (Plan 4). `Op`/`OpKind` survive as the edit-message vocabulary for rebuild
//! decisions and the `set_op`/`reset` interface (retired in Phase 2).

use serde::{Deserialize, Serialize};

use crate::local::{AdjustmentSet, LocalAdjustments, MaskLayer};

/// Current on-stack schema version. Bumped if `Op`'s shape changes incompatibly.
pub const STACK_VERSION: u32 = 2;

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

/// Dehaze via the Dark Channel Prior (He et al.). Bipolar: `amount > 0` removes
/// haze; `amount < 0` re-adds haze (symmetric synthesis). 0 = identity. The
/// atmospheric light `A` is a whole-image estimate supplied to the GPU pass as a
/// uniform (never stored here — it is derived from the image, not a user param).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Dehaze {
    /// Dehaze strength in [-1, 1]. 0 = identity; >0 removes haze, <0 adds haze.
    pub amount: f32,
    /// Dark-channel min-filter patch radius in pixels (drives the halo, plumbed
    /// like `Sharpen::radius`). Larger = coarser/softer transmission estimate.
    pub radius: u32,
}

impl Dehaze {
    /// True when the op has no effect (can be dropped from the stack). Keyed on
    /// `amount` only — a radius alone shapes nothing when `amount == 0`.
    pub fn is_identity(&self) -> bool {
        self.amount == 0.0
    }
}

impl Default for Dehaze {
    /// Identity amount but the CANONICAL default radius, so a set that only
    /// ever touches `amount` still shapes the transmission the way the UI's
    /// radius slider default does.
    fn default() -> Self {
        Self {
            amount: 0.0,
            radius: crate::DEHAZE_DEFAULT_RADIUS,
        }
    }
}

/// Interpolation between tone-curve control points.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub enum CurveMode {
    /// Piecewise linear (sharp corners at control points).
    /// Legacy back-compat: sidecars without `mode` load as Linear.
    #[default]
    Linear,
    /// Monotone cubic Hermite (smooth, monotonic, no overshoot).
    Smooth,
}

/// A single point-curve channel (control points + interpolation mode).
/// Identity = empty `points` (or `[(0,0),(1,1)]`). Reuses the shared
/// `curve_lut` bake; `Default` is identity so it is a valid `#[serde(default)]`.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct PointCurve {
    pub points: Vec<(f32, f32)>,
    #[serde(default)]
    pub mode: CurveMode,
}

impl PointCurve {
    /// True when this channel is the identity ramp (no effect).
    pub fn is_identity(&self) -> bool {
        points_are_identity(&self.points)
    }
}

/// Lightroom-style parametric region curve applied to all channels via the
/// composited LUT. Region values in `[-1,1]` (0 = identity); split points in
/// `[0,1]` partition the tonal range into Shadows|Darks|Lights|Highlights.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct ParametricCurve {
    pub highlights: f32,
    pub lights: f32,
    pub darks: f32,
    pub shadows: f32,
    pub shadow_split: f32,
    pub midtone_split: f32,
    pub highlight_split: f32,
}

impl Default for ParametricCurve {
    fn default() -> Self {
        // All region shifts 0, splits at the LR defaults → identity.
        Self {
            highlights: 0.0,
            lights: 0.0,
            darks: 0.0,
            shadows: 0.0,
            shadow_split: 0.25,
            midtone_split: 0.50,
            highlight_split: 0.75,
        }
    }
}

impl ParametricCurve {
    /// True when the curve is at its DEFAULT configuration — zero region shifts
    /// AND splits at their defaults. A splits-only edit has no render effect on
    /// its own (splits only shape non-zero regions), but it is a real user
    /// configuration to preserve, so it is NOT elided: dropping the op on a
    /// splits-only edit would leave the split sliders nothing to persist on and
    /// they would snap back to their defaults.
    pub fn is_identity(&self) -> bool {
        *self == ParametricCurve::default()
    }
}

/// Control points form the identity ramp when empty or exactly the two corners.
fn points_are_identity(points: &[(f32, f32)]) -> bool {
    points.is_empty()
        || (points.len() == 2
            && (points[0].0).abs() < 1e-6
            && (points[0].1).abs() < 1e-6
            && (points[1].0 - 1.0).abs() < 1e-6
            && (points[1].1 - 1.0).abs() < 1e-6)
}

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct ToneCurve {
    /// Master (RGB/luminance) curve — legacy field names, unchanged for
    /// back-compat. Baked to a 256-entry monotone LUT by `uniforms::curve_lut`.
    pub points: Vec<(f32, f32)>,
    /// Interpolation mode. Absent in pre-feature sidecars → Linear (serde default).
    #[serde(default)]
    pub mode: CurveMode,
    // New in P3 — all `#[serde(default)]` = identity, so pre-P3 sidecars load unchanged.
    #[serde(default)]
    pub red: PointCurve,
    #[serde(default)]
    pub green: PointCurve,
    #[serde(default)]
    pub blue: PointCurve,
    #[serde(default)]
    pub parametric: ParametricCurve,
}

impl ToneCurve {
    /// True when Master + R/G/B + parametric are all identity (op can be dropped).
    pub fn is_identity(&self) -> bool {
        points_are_identity(&self.points)
            && self.red.is_identity()
            && self.green.is_identity()
            && self.blue.is_identity()
            && self.parametric.is_identity()
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct HslBand {
    /// Hue shift, normalized [-1, 1]. 0 = identity.
    pub hue: f32,
    /// Saturation delta, normalized [-1, 1]. 0 = identity.
    pub sat: f32,
    /// Lightness delta, normalized [-1, 1]. 0 = identity.
    pub lum: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Hsl {
    /// Per-band deltas; bands = red, orange, yellow, green, aqua, blue,
    /// purple, magenta (the canonical 8-band order). All-zero = identity.
    pub bands: [HslBand; 8],
}

impl Hsl {
    /// True when every band is zero-identity (op can be dropped). Single source
    /// of truth for the all-zero check — used by both the `EditDoc::hsl` getter
    /// and `AdjustmentSet::is_identity` so the two predicates cannot drift.
    pub fn is_identity(&self) -> bool {
        self.bands
            .iter()
            .all(|b| b.hue == 0.0 && b.sat == 0.0 && b.lum == 0.0)
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Sharpen {
    /// Unsharp-mask amount (>= 0). 0 = identity.
    pub amount: f32,
    /// Box-blur radius in pixels (drives the halo size in Plan 3). 0 = identity.
    pub radius: u32,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Correction {
    /// Whether this correction is applied at all.
    pub enabled: bool,
    /// Strength multiplier [0..], 1.0 = full DB correction. Applied as a shader lerp.
    pub amount: f32,
}

impl Default for Correction {
    fn default() -> Self {
        Self {
            enabled: false,
            amount: 1.0,
        }
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct LensCorrection {
    /// Resolved Lensfun lens key; None = unmatched (identity). Re-resolved on open.
    pub lens_id: Option<String>,
    /// Capture context used for the bake (EXIF; user-overridable).
    pub focal_len: f32,
    pub aperture: f32,
    pub crop_factor: f32,
    pub distortion: Correction,
    pub tca: Correction,
    pub vignetting: Correction,
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

/// One color-grading wheel: a hue-sat tint direction plus a luminance offset.
/// `hue` in [0,360) degrees (wheel angle), `sat` in [0,1] (distance from center,
/// 0 = no tint), `lum` in [-1,1] (region luminance offset). Default = neutral.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct GradeWheel {
    pub hue: f32,
    pub sat: f32,
    pub lum: f32,
}

impl GradeWheel {
    /// True when this wheel applies no tint and no luminance shift.
    pub fn is_neutral(&self) -> bool {
        self.sat == 0.0 && self.lum == 0.0
    }
}

/// Lightroom-style color grading: four wheels (Shadows/Midtones/Highlights/
/// Global) plus region `blending` (overlap width, [0,1]) and `balance` (shifts
/// the shadow↔highlight midpoint, [-1,1]). Default = identity.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct ColorGrade {
    pub shadows: GradeWheel,
    pub midtones: GradeWheel,
    pub highlights: GradeWheel,
    pub global: GradeWheel,
    pub blending: f32,
    pub balance: f32,
}

impl Default for ColorGrade {
    fn default() -> Self {
        // Neutral wheels, mid blending, centered balance → identity.
        Self {
            shadows: GradeWheel::default(),
            midtones: GradeWheel::default(),
            highlights: GradeWheel::default(),
            global: GradeWheel::default(),
            blending: 0.5,
            balance: 0.0,
        }
    }
}

impl ColorGrade {
    /// True when the grade is at its DEFAULT configuration — every wheel neutral
    /// AND blending/balance at their defaults. A blending/balance-only edit has
    /// no render effect on its own (they only shape non-neutral wheels), but it
    /// is a real user configuration to preserve, so it is NOT elided: dropping
    /// the op on a blending/balance-only edit would leave those sliders nothing
    /// to persist on and they would snap back to their defaults.
    pub fn is_identity(&self) -> bool {
        *self == ColorGrade::default()
    }
}

/// One adjustment in the stack. `Op` is `Clone` (not `Copy`) because `ToneCurve`
/// carries a `Vec` of control points.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Op {
    Exposure(Exposure),
    WhiteBalance(WhiteBalance),
    Contrast(Contrast),
    Dehaze(Dehaze),
    ToneCurve(ToneCurve),
    Hsl(Hsl),
    ColorGrade(ColorGrade),
    LocalAdjustments(LocalAdjustments),
    Sharpen(Sharpen),
    LensCorrection(LensCorrection),
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
    Dehaze = 3,
    ToneCurve = 4,
    Hsl = 5,
    ColorGrade = 6,
    LocalAdjustments = 7,
    Sharpen = 8,
    LensCorrection = 9,
    Geometry = 10,
}

impl Op {
    pub fn kind(&self) -> OpKind {
        match self {
            Op::Exposure(_) => OpKind::Exposure,
            Op::WhiteBalance(_) => OpKind::WhiteBalance,
            Op::Contrast(_) => OpKind::Contrast,
            Op::Dehaze(_) => OpKind::Dehaze,
            Op::ToneCurve(_) => OpKind::ToneCurve,
            Op::Hsl(_) => OpKind::Hsl,
            Op::ColorGrade(_) => OpKind::ColorGrade,
            Op::LocalAdjustments(_) => OpKind::LocalAdjustments,
            Op::Sharpen(_) => OpKind::Sharpen,
            Op::LensCorrection(_) => OpKind::LensCorrection,
            Op::Geometry(_) => OpKind::Geometry,
        }
    }
}

/// The edit document (design 2026-07-28 §2): geometry ops global-only, one
/// global `AdjustmentSet` ("the layer with no mask"), and mask layers stacked
/// on top. Immutable editing: `set_op`/`reset` return new docs. The old
/// `Vec<Op>` stack is gone; `Op`/`OpKind` survive as the edit-message
/// vocabulary (`EditOutcome.kind`, rebuild decisions) until Phase 2.
///
/// **Invariant:** an identity-valued `set_op` is byte-equal to a reset — that
/// is, `is_identity()`, `PartialEq` against `EditDoc::default()`, and the
/// serde hash (`hash_serde`, keyed for the warm/preview caches) all agree, for
/// EVERY `Op` kind (including `Hsl`, whose bands can be `-0.0`-valued and thus
/// `is_identity() == true` without being the literal `Default` bit pattern).
/// `set_op` enforces this via `AdjustmentSet::normalized()`: the match writes
/// each op's raw params into `global` (or a layer's `adjustments`), then a
/// tail call snaps every identity-valued structured field to its exact
/// `Default`, rather than checking identity per-arm.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct EditDoc {
    pub version: u32,
    #[serde(default)]
    pub global: AdjustmentSet,
    #[serde(default)]
    pub layers: Vec<MaskLayer>,
    #[serde(default)]
    pub lens: Option<LensCorrection>,
    #[serde(default)]
    pub geometry: Option<Geometry>,
}

/// Compatibility alias: the rest of the workspace still says `OpStack`.
/// Retired in Phase 2 alongside `Op`/`OpKind`.
pub type OpStack = EditDoc;

impl Default for EditDoc {
    fn default() -> Self {
        Self {
            version: STACK_VERSION,
            global: AdjustmentSet::default(),
            layers: Vec::new(),
            lens: None,
            geometry: None,
        }
    }
}

impl EditDoc {
    /// Unedited: identity global set, no mask layers, no geometry/lens.
    pub fn is_identity(&self) -> bool {
        self.global.is_identity()
            && self.layers.is_empty()
            && self.lens.is_none()
            && self.geometry.is_none()
    }

    /// Return a new doc with `op`'s parameters written into their unified home
    /// (global set field, the layer list, or a geometry/lens global).
    ///
    /// Identity-valued params are normalized to the kind's exact `Default`
    /// (see the invariant documented on `EditDoc`) so an identity `set_op`
    /// never leaves the doc `is_identity() == true` yet `!= EditDoc::default()`
    /// — which would otherwise desync `should_write_back`'s `== OpStack::default()`
    /// check and the `hash_serde` cache key from `is_identity()`. The match writes
    /// each op's raw params; a single tail `AdjustmentSet::normalized()` call
    /// (mirrored for each layer's set in the `LocalAdjustments` arm) then snaps
    /// every identity-valued structured field to its exact `Default` in one
    /// place, for ALL kinds — including `Hsl`, whose `-0.0`-valued bands are
    /// `is_identity() == true` but not byte-equal to `Hsl::default()` without it.
    pub fn set_op(&self, op: Op) -> EditDoc {
        let mut d = self.clone();
        match op {
            Op::Exposure(e) => d.global.exposure = e.ev,
            Op::WhiteBalance(w) => {
                d.global.temp = w.temp;
                d.global.tint = w.tint;
            }
            Op::Contrast(c) => d.global.contrast = c.amount,
            Op::Dehaze(x) => d.global.dehaze = x,
            Op::ToneCurve(t) => d.global.tone_curve = t,
            Op::Hsl(h) => d.global.hsl = h,
            Op::ColorGrade(g) => d.global.color_grade = g,
            Op::LocalAdjustments(la) => {
                d.layers = la
                    .layers
                    .into_iter()
                    .map(|mut layer| {
                        layer.adjustments = layer.adjustments.normalized();
                        layer
                    })
                    .collect();
            }
            Op::Sharpen(s) => d.global.sharpen = s,
            Op::LensCorrection(l) => d.lens = Some(l),
            Op::Geometry(g) => d.geometry = Some(g),
        }
        d.global = d.global.normalized();
        d
    }

    /// Return a new doc with `kind`'s parameters reset to identity.
    pub fn reset(&self, kind: OpKind) -> EditDoc {
        let mut d = self.clone();
        match kind {
            OpKind::Exposure => d.global.exposure = 0.0,
            OpKind::WhiteBalance => {
                d.global.temp = 0.0;
                d.global.tint = 0.0;
            }
            OpKind::Contrast => d.global.contrast = 0.0,
            OpKind::Dehaze => d.global.dehaze = Dehaze::default(),
            OpKind::ToneCurve => d.global.tone_curve = ToneCurve::default(),
            OpKind::Hsl => d.global.hsl = Hsl::default(),
            OpKind::ColorGrade => d.global.color_grade = ColorGrade::default(),
            OpKind::LocalAdjustments => d.layers = Vec::new(),
            OpKind::Sharpen => d.global.sharpen = Sharpen::default(),
            OpKind::LensCorrection => d.lens = None,
            OpKind::Geometry => d.geometry = None,
        }
        d
    }

    /// New doc with the GLOBAL adjustment set replaced (normalized — see
    /// `AdjustmentSet::normalized`). The scoped-edit write path for
    /// `EditScope::Global`.
    pub fn with_global(&self, set: AdjustmentSet) -> EditDoc {
        let mut d = self.clone();
        d.global = set.normalized();
        d
    }

    /// New doc with layer `idx`'s adjustment set replaced (normalized). The
    /// scoped-edit write path for `EditScope::Mask(idx)`. An out-of-range
    /// `idx` (stale selection racing a delete) returns the doc unchanged.
    pub fn with_layer_adjustments(&self, idx: usize, set: AdjustmentSet) -> EditDoc {
        let mut d = self.clone();
        if let Some(layer) = d.layers.get_mut(idx) {
            layer.adjustments = set.normalized();
        }
        d
    }

    pub fn exposure(&self) -> Option<Exposure> {
        (self.global.exposure != 0.0).then_some(Exposure {
            ev: self.global.exposure,
        })
    }
    pub fn white_balance(&self) -> Option<WhiteBalance> {
        (self.global.temp != 0.0 || self.global.tint != 0.0).then_some(WhiteBalance {
            temp: self.global.temp,
            tint: self.global.tint,
        })
    }
    pub fn contrast(&self) -> Option<Contrast> {
        (self.global.contrast != 0.0).then_some(Contrast {
            amount: self.global.contrast,
        })
    }
    pub fn dehaze(&self) -> Option<Dehaze> {
        (!self.global.dehaze.is_identity()).then_some(self.global.dehaze)
    }
    /// True when dehaze recovery is active ANYWHERE in the document: the
    /// global `Dehaze` op, or any VISIBLE mask layer's `dehaze.amount`
    /// (Phase 4 Task 3 — per-mask dehaze reuses the shared whole-image
    /// transmission map). Callers that used to gate a dehaze-dependent action
    /// on `self.dehaze().is_some()` alone (transmission-map computation,
    /// export's transmission-source selection) must widen to this instead —
    /// otherwise a mask-only dehaze layer (global amount 0, so `dehaze()`
    /// returns `None`) silently gets no transmission to recover from. A
    /// hidden layer never counts, mirroring `LocalAdjustments::is_identity`'s
    /// `visible_layers()` filter.
    pub fn dehaze_active_anywhere(&self) -> bool {
        self.dehaze().is_some()
            || self
                .layers
                .iter()
                .any(|l| l.visible && l.adjustments.dehaze.amount != 0.0)
    }
    pub fn tone_curve(&self) -> Option<ToneCurve> {
        (!self.global.tone_curve.is_identity()).then(|| self.global.tone_curve.clone())
    }
    pub fn hsl(&self) -> Option<Hsl> {
        (!self.global.hsl.is_identity()).then_some(self.global.hsl)
    }
    pub fn color_grade(&self) -> Option<ColorGrade> {
        (!self.global.color_grade.is_identity()).then_some(self.global.color_grade)
    }
    pub fn local_adjustments(&self) -> Option<LocalAdjustments> {
        (!self.layers.is_empty()).then(|| LocalAdjustments {
            layers: self.layers.clone(),
        })
    }
    pub fn sharpen(&self) -> Option<Sharpen> {
        (self.global.sharpen.amount != 0.0).then_some(self.global.sharpen)
    }
    pub fn geometry(&self) -> Option<Geometry> {
        self.geometry
    }
    pub fn lens_correction(&self) -> Option<LensCorrection> {
        self.lens.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_doc_is_identity_at_version_2() {
        let d = EditDoc::default();
        assert_eq!(d.version, STACK_VERSION);
        assert_eq!(STACK_VERSION, 2);
        assert!(d.is_identity());
        assert!(d.exposure().is_none());
        assert!(d.local_adjustments().is_none());
    }

    #[test]
    fn set_op_and_getters_round_trip_every_op_kind() {
        let d = EditDoc::default()
            .set_op(Op::Exposure(Exposure { ev: 0.75 }))
            .set_op(Op::WhiteBalance(WhiteBalance {
                temp: 0.2,
                tint: -0.1,
            }))
            .set_op(Op::Contrast(Contrast { amount: 0.3 }))
            .set_op(Op::Dehaze(Dehaze {
                amount: 0.4,
                radius: 9,
            }))
            .set_op(Op::Sharpen(Sharpen {
                amount: 0.6,
                radius: 3,
            }));
        assert_eq!(d.exposure(), Some(Exposure { ev: 0.75 }));
        assert_eq!(
            d.white_balance(),
            Some(WhiteBalance {
                temp: 0.2,
                tint: -0.1
            })
        );
        assert_eq!(d.contrast(), Some(Contrast { amount: 0.3 }));
        assert_eq!(
            d.dehaze(),
            Some(Dehaze {
                amount: 0.4,
                radius: 9
            })
        );
        assert_eq!(
            d.sharpen(),
            Some(Sharpen {
                amount: 0.6,
                radius: 3
            })
        );
        assert!(!d.is_identity());
    }

    #[test]
    fn getters_are_none_at_identity_values() {
        // Setting an identity-valued op is equivalent to reset (mirrors the old
        // "op absent" semantics the whole app keys has_edits on). This is the
        // stronger EditDoc invariant, not just "getter returns None": the
        // resulting doc is byte-equal to EditDoc::default() (see
        // identity_valued_set_op_is_byte_equal_to_default below), so
        // is_identity(), PartialEq-vs-default, and the serde hash all agree.
        let d = EditDoc::default()
            .set_op(Op::Exposure(Exposure { ev: 0.5 }))
            .set_op(Op::Exposure(Exposure { ev: 0.0 }));
        assert!(d.exposure().is_none());
        assert!(d.is_identity());
        assert_eq!(d, EditDoc::default());
    }

    #[test]
    fn identity_valued_set_op_is_byte_equal_to_default() {
        // The EditDoc invariant (see the doc comment on EditDoc and on set_op):
        // an identity-valued set_op must be == EditDoc::default(), not merely
        // is_identity() == true, so has_edits (== OpStack::default()) and
        // hash_serde (the preview/warm cache key) stay in sync with is_identity().
        let default = EditDoc::default();

        let dehazed = default.set_op(Op::Dehaze(Dehaze {
            amount: 0.0,
            radius: 9, // non-canonical radius, but amount 0 => identity
        }));
        assert!(dehazed.is_identity());
        assert_eq!(
            dehazed, default,
            "identity dehaze must normalize to default"
        );

        let sharpened = default.set_op(Op::Sharpen(Sharpen {
            amount: 0.0,
            radius: 3, // non-canonical radius, but amount 0 => identity
        }));
        assert!(sharpened.is_identity());
        assert_eq!(
            sharpened, default,
            "identity sharpen must normalize to default"
        );

        let curved = default.set_op(Op::ToneCurve(ToneCurve {
            points: vec![(0.0, 0.0), (1.0, 1.0)], // identity corner-ramp
            ..Default::default()
        }));
        assert!(curved.is_identity());
        assert_eq!(
            curved, default,
            "identity tone curve must normalize to default"
        );

        // ColorGrade::is_identity is full struct equality against
        // ColorGrade::default() (`*self == ColorGrade::default()`), so the
        // only way for is_identity() to be true on a value whose fields are
        // not literally the default constants is IEEE-754 negative zero:
        // -0.0 == 0.0 numerically (so is_identity() is true and `assert_eq!`,
        // which uses that same PartialEq, can't tell them apart) but they
        // serialize to different JSON ("-0.0" vs "0.0"), which is exactly the
        // "different serde hash" failure mode this invariant guards against.
        let graded = default.set_op(Op::ColorGrade(ColorGrade {
            balance: -0.0,
            ..ColorGrade::default()
        }));
        assert!(graded.is_identity());
        assert_eq!(graded, default);
        assert_eq!(
            serde_json::to_string(&graded).unwrap(),
            serde_json::to_string(&default).unwrap(),
            "identity color grade must serialize byte-identically to default, \
             not just PartialEq-equal (negative zero is == but not byte-equal)"
        );

        // Same -0.0 hole for Hsl: is_identity() treats -0.0 == 0.0, but a band
        // literal with a -0.0 field is not byte-equal to Hsl::default() unless
        // set_op's tail normalization snaps it back to the canonical default.
        let mut bands = [HslBand::default(); 8];
        bands[0].hue = -0.0;
        let hsled = default.set_op(Op::Hsl(Hsl { bands }));
        assert!(hsled.is_identity());
        assert_eq!(hsled, default, "identity hsl must normalize to default");
        assert_eq!(
            serde_json::to_string(&hsled).unwrap(),
            serde_json::to_string(&default).unwrap(),
            "identity hsl must serialize byte-identically to default, \
             not just PartialEq-equal (negative zero is == but not byte-equal)"
        );
    }

    #[test]
    fn reset_clears_exactly_one_kind() {
        let d = EditDoc::default()
            .set_op(Op::Exposure(Exposure { ev: 0.5 }))
            .set_op(Op::Contrast(Contrast { amount: 0.3 }));
        let d = d.reset(OpKind::Exposure);
        assert!(d.exposure().is_none());
        assert_eq!(d.contrast(), Some(Contrast { amount: 0.3 }));
    }

    #[test]
    fn local_adjustments_map_to_layers() {
        let la = LocalAdjustments {
            layers: vec![crate::local::MaskLayer {
                name: "Mask 1".into(),
                visible: true,
                mask: Default::default(),
                adjustments: Default::default(),
            }],
        };
        let d = EditDoc::default().set_op(Op::LocalAdjustments(la.clone()));
        assert_eq!(d.layers.len(), 1);
        assert_eq!(d.local_adjustments(), Some(la));
        // A created (even identity-valued) mask counts as an edit, as today.
        assert!(!d.is_identity());
        let d = d.reset(OpKind::LocalAdjustments);
        assert!(d.local_adjustments().is_none());
    }

    #[test]
    fn geometry_and_lens_are_globals_not_layers() {
        let g = Geometry {
            crop: CropRect::full(),
            angle_deg: 2.0,
            aspect: Aspect::Original,
        };
        let d = EditDoc::default().set_op(Op::Geometry(g));
        assert_eq!(d.geometry(), Some(g));
        assert!(d.layers.is_empty());
        assert!(!d.is_identity());
    }

    #[test]
    fn new_ops_round_through_set_and_accessors() {
        let d = EditDoc::default()
            .set_op(Op::ToneCurve(ToneCurve {
                // A non-identity ramp so the getter (None at identity values)
                // returns Some, per the "op absent" semantics.
                points: vec![(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)],
                mode: CurveMode::Linear,
                ..Default::default()
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
        assert_eq!(d.tone_curve().unwrap().points.len(), 3);
        assert_eq!(d.hsl().unwrap().bands[0].hue, 0.1);
        assert_eq!(
            d.sharpen(),
            Some(Sharpen {
                amount: 0.5,
                radius: 2
            })
        );
        assert_eq!(d.geometry().unwrap().angle_deg, 5.0);
    }

    #[test]
    fn tonecurve_without_mode_field_deserializes_as_linear() {
        // A sidecar written before this feature has no `mode` key.
        let json = r#"{ "points": [[0.0,0.0],[1.0,1.0]] }"#;
        let tc: ToneCurve = serde_json::from_str(json).unwrap();
        assert_eq!(tc.mode, CurveMode::Linear);
    }

    #[test]
    fn tonecurve_mode_roundtrips() {
        let tc = ToneCurve {
            points: vec![(0.0, 0.0), (1.0, 1.0)],
            mode: CurveMode::Smooth,
            ..Default::default()
        };
        let s = serde_json::to_string(&tc).unwrap();
        assert_eq!(serde_json::from_str::<ToneCurve>(&s).unwrap(), tc);
    }

    #[test]
    fn lens_correction_and_geometry_are_independent_globals() {
        let lc = LensCorrection {
            lens_id: Some("Canon EF 24-70mm f/2.8L II USM".into()),
            focal_len: 50.0,
            aperture: 8.0,
            crop_factor: 1.0,
            distortion: Correction {
                enabled: true,
                amount: 1.0,
            },
            tca: Correction::default(),
            vignetting: Correction::default(),
        };
        let g = Geometry {
            crop: CropRect::full(),
            angle_deg: 3.0,
            aspect: Aspect::Original,
        };
        let d = EditDoc::default()
            .set_op(Op::Geometry(g))
            .set_op(Op::LensCorrection(lc.clone()));
        assert_eq!(d.lens_correction(), Some(lc));
        assert_eq!(d.geometry(), Some(g));
    }

    #[test]
    fn correction_default_is_off_at_full_amount() {
        assert_eq!(
            Correction::default(),
            Correction {
                enabled: false,
                amount: 1.0
            }
        );
    }

    #[test]
    fn local_adjustments_sharpen_and_hsl_coexist() {
        use crate::local::{AdjustmentSet, LocalAdjustments, MaskLayer};
        use ferrolite_mask::MaskDefinition;
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
        let d = EditDoc::default()
            .set_op(Op::Sharpen(Sharpen {
                amount: 0.3,
                radius: 1,
            }))
            .set_op(Op::LocalAdjustments(la.clone()))
            .set_op(Op::Hsl(Hsl {
                bands: [HslBand {
                    hue: 0.1,
                    sat: 0.0,
                    lum: 0.0,
                }; 8],
            }));
        assert_eq!(
            d.sharpen(),
            Some(Sharpen {
                amount: 0.3,
                radius: 1
            })
        );
        assert_eq!(d.local_adjustments(), Some(la));
        assert_eq!(d.hsl().unwrap().bands[0].hue, 0.1);
    }

    #[test]
    fn grade_wheel_default_is_neutral() {
        let w = GradeWheel::default();
        assert_eq!((w.hue, w.sat, w.lum), (0.0, 0.0, 0.0));
        assert!(w.is_neutral());
    }

    #[test]
    fn color_grade_default_is_identity_with_half_blending() {
        let cg = ColorGrade::default();
        assert_eq!(cg.blending, 0.5);
        assert_eq!(cg.balance, 0.0);
        assert!(cg.shadows.is_neutral() && cg.global.is_neutral());
        assert!(
            cg.is_identity(),
            "the default grade (neutral wheels, default blending/balance) is identity"
        );
    }

    #[test]
    fn color_grade_tinted_wheel_is_non_identity() {
        let cg = ColorGrade {
            shadows: GradeWheel {
                hue: 210.0,
                sat: 0.4,
                lum: 0.0,
            },
            ..Default::default()
        };
        assert!(!cg.is_identity());
        // A lum-only wheel is also non-identity.
        let cg2 = ColorGrade {
            highlights: GradeWheel {
                hue: 0.0,
                sat: 0.0,
                lum: 0.3,
            },
            ..Default::default()
        };
        assert!(!cg2.is_identity());
        // Blending/balance moved off their defaults is a real, persist-worthy
        // configuration even with neutral wheels (no render effect on its own),
        // so it must NOT be elided — otherwise those sliders snap back.
        let cg3 = ColorGrade {
            blending: 0.9,
            balance: -0.5,
            ..Default::default()
        };
        assert!(!cg3.is_identity());
        // Blending/balance still AT their defaults with neutral wheels = identity.
        let cg4 = ColorGrade {
            blending: 0.5,
            balance: 0.0,
            ..Default::default()
        };
        assert!(cg4.is_identity());
    }

    #[test]
    fn color_grade_sharpen_and_hsl_coexist() {
        let cg = Op::ColorGrade(ColorGrade {
            midtones: GradeWheel {
                hue: 120.0,
                sat: 0.2,
                lum: 0.0,
            },
            ..Default::default()
        });
        let d = EditDoc::default()
            .set_op(Op::Sharpen(Sharpen {
                amount: 0.3,
                radius: 1,
            }))
            .set_op(cg.clone())
            .set_op(Op::Hsl(Hsl {
                bands: [HslBand {
                    hue: 0.1,
                    sat: 0.0,
                    lum: 0.0,
                }; 8],
            }));
        assert_eq!(d.color_grade().unwrap().midtones.hue, 120.0);
        assert_eq!(
            d.sharpen(),
            Some(Sharpen {
                amount: 0.3,
                radius: 1
            })
        );
        assert_eq!(d.hsl().unwrap().bands[0].hue, 0.1);
    }

    #[test]
    fn color_grade_roundtrips() {
        let cg = ColorGrade {
            shadows: GradeWheel {
                hue: 210.0,
                sat: 0.5,
                lum: -0.2,
            },
            midtones: GradeWheel {
                hue: 90.0,
                sat: 0.1,
                lum: 0.0,
            },
            highlights: GradeWheel {
                hue: 40.0,
                sat: 0.3,
                lum: 0.15,
            },
            global: GradeWheel {
                hue: 0.0,
                sat: 0.0,
                lum: 0.05,
            },
            blending: 0.7,
            balance: -0.3,
        };
        let s = serde_json::to_string(&cg).unwrap();
        assert_eq!(serde_json::from_str::<ColorGrade>(&s).unwrap(), cg);
    }

    #[test]
    fn point_curve_default_is_identity() {
        let p = PointCurve::default();
        assert!(p.points.is_empty());
        assert_eq!(p.mode, CurveMode::Linear);
        assert!(p.is_identity());
    }

    #[test]
    fn parametric_default_splits_are_quarter_half_threequarter() {
        let p = ParametricCurve::default();
        assert_eq!(p.shadow_split, 0.25);
        assert_eq!(p.midtone_split, 0.50);
        assert_eq!(p.highlight_split, 0.75);
        assert_eq!(
            (p.highlights, p.lights, p.darks, p.shadows),
            (0.0, 0.0, 0.0, 0.0)
        );
        assert!(
            p.is_identity(),
            "the default parametric curve (zero regions, default splits) is identity"
        );
    }

    #[test]
    fn parametric_moved_split_alone_is_not_identity() {
        // A split moved off its default (with zero regions) has no render effect
        // on its own, but is a real user configuration to persist — it must NOT
        // be elided, or the split sliders would snap back to their defaults.
        let p = ParametricCurve {
            midtone_split: 0.65,
            ..Default::default()
        };
        assert!(!p.is_identity());
    }

    #[test]
    fn color_grade_moved_blending_alone_is_not_identity() {
        let cg = ColorGrade {
            blending: 0.8,
            ..Default::default()
        };
        assert!(!cg.is_identity());
    }

    #[test]
    fn tone_curve_default_is_fully_identity() {
        let tc = ToneCurve::default();
        assert!(tc.is_identity());
        assert!(tc.red.is_identity() && tc.green.is_identity() && tc.blue.is_identity());
        assert!(tc.parametric.is_identity());
    }

    #[test]
    fn tone_curve_red_channel_makes_it_non_identity() {
        let tc = ToneCurve {
            red: PointCurve {
                points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
                mode: CurveMode::Smooth,
            },
            ..Default::default()
        };
        assert!(
            !tc.is_identity(),
            "a non-identity red curve makes the op non-identity"
        );
    }

    #[test]
    fn tone_curve_parametric_makes_it_non_identity() {
        let tc = ToneCurve {
            parametric: ParametricCurve {
                shadows: 0.5,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(!tc.is_identity());
    }

    #[test]
    fn pre_p3_tonecurve_loads_with_identity_new_fields() {
        // A sidecar written before P3 has only points + mode.
        let json = r#"{ "points": [[0.0,0.0],[1.0,1.0]], "mode": "Linear" }"#;
        let tc: ToneCurve = serde_json::from_str(json).unwrap();
        assert_eq!(tc.points, vec![(0.0, 0.0), (1.0, 1.0)]);
        assert!(tc.red.is_identity() && tc.green.is_identity() && tc.blue.is_identity());
        assert!(tc.parametric.is_identity());
    }

    #[test]
    fn tonecurve_with_new_fields_roundtrips() {
        let tc = ToneCurve {
            points: vec![(0.0, 0.0), (1.0, 1.0)],
            mode: CurveMode::Smooth,
            red: PointCurve {
                points: vec![(0.0, 0.0), (0.4, 0.6), (1.0, 1.0)],
                mode: CurveMode::Smooth,
            },
            green: PointCurve::default(),
            blue: PointCurve::default(),
            parametric: ParametricCurve {
                shadows: 0.3,
                highlight_split: 0.8,
                ..Default::default()
            },
        };
        let s = serde_json::to_string(&tc).unwrap();
        assert_eq!(serde_json::from_str::<ToneCurve>(&s).unwrap(), tc);
    }

    #[test]
    fn dehaze_default_and_identity() {
        // A radius alone (amount 0) has no render effect → identity.
        assert!(Dehaze {
            amount: 0.0,
            radius: 8
        }
        .is_identity());
        assert!(!Dehaze {
            amount: 0.5,
            radius: 8
        }
        .is_identity());
        assert!(!Dehaze {
            amount: -0.5,
            radius: 8
        }
        .is_identity());
    }

    #[test]
    fn dehaze_contrast_and_tone_curve_coexist() {
        let d = EditDoc::default()
            .set_op(Op::ToneCurve(ToneCurve {
                points: vec![(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)],
                ..Default::default()
            }))
            .set_op(Op::Dehaze(Dehaze {
                amount: 0.4,
                radius: 8,
            }))
            .set_op(Op::Contrast(Contrast { amount: 0.1 }));
        assert_eq!(
            d.dehaze(),
            Some(Dehaze {
                amount: 0.4,
                radius: 8
            })
        );
        assert_eq!(d.contrast(), Some(Contrast { amount: 0.1 }));
        assert!(d.tone_curve().is_some());
    }

    /// Phase 4 Task 3 (TDD Step 1): `dehaze_active_anywhere` must widen past
    /// the global-only `dehaze()` gate — a mask-only dehaze layer (global
    /// amount 0) still needs the shared transmission map computed.
    #[test]
    // default-then-assign mirrors the plan's literal test spec; clearer than
    // struct-update for single fields.
    #[allow(clippy::field_reassign_with_default)]
    fn dehaze_active_anywhere_covers_global_and_mask_layers() {
        use ferrolite_mask::MaskDefinition;

        // Nothing active anywhere.
        assert!(!EditDoc::default().dehaze_active_anywhere());

        // Global dehaze active, no layers.
        let global_active = EditDoc::default().set_op(Op::Dehaze(Dehaze {
            amount: 0.5,
            radius: 8,
        }));
        assert!(global_active.dehaze_active_anywhere());

        // A VISIBLE mask layer with a non-zero dehaze amount, global amount 0.
        let mut layer_adjustments = AdjustmentSet::default();
        layer_adjustments.dehaze.amount = 0.3;
        let mask_active = EditDoc::default().set_op(Op::LocalAdjustments(LocalAdjustments {
            layers: vec![MaskLayer {
                name: "m".into(),
                visible: true,
                mask: MaskDefinition::default(),
                adjustments: layer_adjustments.clone(),
            }],
        }));
        assert!(
            mask_active.dehaze_active_anywhere(),
            "a visible mask layer's non-zero dehaze amount alone must activate the gate"
        );

        // Same layer, but HIDDEN: must not count.
        let mask_hidden = EditDoc::default().set_op(Op::LocalAdjustments(LocalAdjustments {
            layers: vec![MaskLayer {
                name: "m".into(),
                visible: false,
                mask: MaskDefinition::default(),
                adjustments: layer_adjustments,
            }],
        }));
        assert!(
            !mask_hidden.dehaze_active_anywhere(),
            "a hidden layer's dehaze amount must not activate the gate"
        );

        // A layer with a zero dehaze amount (but other adjustments) does not
        // activate the gate on its own.
        let mask_inert = EditDoc::default().set_op(Op::LocalAdjustments(LocalAdjustments {
            layers: vec![MaskLayer {
                name: "m".into(),
                visible: true,
                mask: MaskDefinition::default(),
                adjustments: AdjustmentSet {
                    exposure: 0.4,
                    ..Default::default()
                },
            }],
        }));
        assert!(!mask_inert.dehaze_active_anywhere());
    }

    #[test]
    fn opkind_discriminants_after_dehaze_insert() {
        assert_eq!(OpKind::Contrast as u8, 2);
        assert_eq!(OpKind::Dehaze as u8, 3);
        assert_eq!(OpKind::ToneCurve as u8, 4);
        assert_eq!(OpKind::Hsl as u8, 5);
        assert_eq!(OpKind::ColorGrade as u8, 6);
        assert_eq!(OpKind::LocalAdjustments as u8, 7);
        assert_eq!(OpKind::Sharpen as u8, 8);
        assert_eq!(OpKind::LensCorrection as u8, 9);
        assert_eq!(OpKind::Geometry as u8, 10);
    }

    #[test]
    fn doc_roundtrips_through_json() {
        // `EditDoc`'s JSON shape (global/layers) replaces the old ordered
        // `Vec<Op>` wire format; Task 3 owns migrating the on-disk fixtures.
        // Here we only assert the new shape roundtrips.
        let d = EditDoc::default()
            .set_op(Op::Exposure(Exposure { ev: 0.5 }))
            .set_op(Op::Dehaze(Dehaze {
                amount: -0.25,
                radius: 8,
            }))
            .set_op(Op::Sharpen(Sharpen {
                amount: 0.6,
                radius: 3,
            }));
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<EditDoc>(&json).unwrap(), d);
    }

    #[test]
    // default-then-assign mirrors the plan's literal test spec; clearer than
    // struct-update for single fields.
    #[allow(clippy::field_reassign_with_default)]
    fn with_global_normalizes_identity_structures() {
        let mut set = AdjustmentSet::default();
        set.dehaze = Dehaze {
            amount: 0.0,
            radius: 9,
        }; // identity, non-canonical radius
        set.exposure = 0.5;
        let d = EditDoc::default().with_global(set);
        assert_eq!(
            d.global.dehaze,
            Dehaze::default(),
            "identity dehaze snapped"
        );
        assert_eq!(d.global.exposure, 0.5, "live value preserved");
    }

    #[test]
    // default-then-assign mirrors the plan's literal test spec; clearer than
    // struct-update for single fields.
    #[allow(clippy::field_reassign_with_default)]
    fn with_layer_adjustments_writes_only_that_layer_and_normalizes() {
        let la = LocalAdjustments {
            layers: vec![
                crate::local::MaskLayer {
                    name: "A".into(),
                    visible: true,
                    mask: Default::default(),
                    adjustments: Default::default(),
                },
                crate::local::MaskLayer {
                    name: "B".into(),
                    visible: true,
                    mask: Default::default(),
                    adjustments: Default::default(),
                },
            ],
        };
        let d = EditDoc::default().set_op(Op::LocalAdjustments(la));
        let mut set = AdjustmentSet::default();
        set.exposure = -1.0;
        set.sharpen = Sharpen {
            amount: 0.0,
            radius: 5,
        }; // identity, non-canonical
        let d2 = d.with_layer_adjustments(1, set);
        assert_eq!(
            d2.layers[0].adjustments,
            AdjustmentSet::default(),
            "layer 0 untouched"
        );
        assert_eq!(d2.layers[1].adjustments.exposure, -1.0);
        assert_eq!(
            d2.layers[1].adjustments.sharpen,
            Sharpen::default(),
            "identity sharpen snapped"
        );
    }

    #[test]
    // default-then-assign mirrors the plan's literal test spec; clearer than
    // struct-update for single fields.
    #[allow(clippy::field_reassign_with_default)]
    fn with_layer_adjustments_out_of_range_is_a_noop() {
        let d = EditDoc::default();
        let mut set = AdjustmentSet::default();
        set.exposure = 1.0;
        assert_eq!(
            d.with_layer_adjustments(3, set),
            d,
            "no panic, unchanged doc"
        );
    }
}
