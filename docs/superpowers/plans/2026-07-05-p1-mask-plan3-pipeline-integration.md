# P1 Masking — Plan 3: `ferrolite-pipeline` local-adjustments integration

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `Op::LocalAdjustments` (an ordered `Vec<MaskLayer>`) to the edit DAG — each layer composites a `ferrolite_mask::MaskDefinition` and applies a per-mask Light+Color point-op set — inserted after `Hsl`, persisted in `frl:ops`, and recomputed on both the preview and the full-res tiled tiers.

**Architecture:** `ferrolite-pipeline` gains the `LocalAdjustments`/`MaskLayer`/`AdjustmentSet` data model, a Light+Color WGSL point-op that blends adjusted-vs-input by a mask value, and a `LocalAdjustmentsNode` (`Node<PipelineImage>`) that — per visible layer — composites the mask with the existing `ferrolite-mask` passes (shape evaluators, brush rasterizer, `CompositePass`), then applies the point-op through it. The node is inserted between the `Hsl` and `Sharpen` nodes of the unchanged `Graph<PipelineImage>` (contract 4) in **both** `EditPipeline` (preview: geometry runs last → mask is source-anchored, exact) and `TileEditPipeline` (full-res: mask composited once at full output resolution and sampled per tile). Masks are parametric source-of-truth; the sidecar stores only params (serde is transparent — no `xmp.rs` change).

**Tech Stack:** Rust, `wgpu` compute passes, `ferrolite-gpu` retained `Graph<O>` executor, `ferrolite-mask` engine crate (shape/brush/composite passes), `bytemuck` Pod uniforms, `serde`/`serde_json`, golden-image tests that auto-skip when `GpuContext::headless()` is `None`.

## Global Constraints

- **Branch:** `feat/p1-mask-plan3-pipeline`, created off `feat/p1-masking-brush-vt` (Plan 2's brush rasterizer must be present). Do **not** merge/PR/finish — this is 1 of 5 plans; stop and report at the green gate.
- **Never block the UI/update thread** (CLAUDE.md §1). All GPU work here runs inside `Node::evaluate` on the render thread and must be bounded; pipelines/passes are built **once** and reused (never per image/edit/tile). No new per-frame pipeline builds.
- **Executor unchanged** (contract 4): do not modify `ferrolite-gpu`. Mask/adjustment work is supplied as `Node`s.
- **No ferrolite-mask shader changes** in this plan (full-res is the pragmatic tier — see Task 9). Only additive Rust wiring may touch `ferrolite-mask` if a public helper is missing; prefer composing existing public APIs.
- **`OpKind` is a sort key, never serialized; `Op` serializes by serde variant name.** Renumbering `OpKind` discriminants must not change any sidecar JSON — proven by a snapshot test (Task 2).
- **Version-tolerant persistence** (contract 2): unknown/absent `LocalAdjustments` → identity; malformed → identity + `.xmp.bak` (existing `xmp.rs` machinery, unchanged).
- **Per-control reset** (CLAUDE.md): the data model must expose a per-control reset for every Light+Color control (used by Plan 4's UI; tested here at the model level).
- **Reserved neighborhood fields** (`texture`, `clarity`, `dehaze`, `sharpness`, `noise`) are carried in `AdjustmentSet` for serde/schema stability but have **no shader** (P3/P4 own them).
- **NO UI in this plan.** Drive everything via unit tests + golden tests + a minimal in-test harness. The Develop UI is Plan 4.
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` all green. Then STOP and hand the author a visual test plan (or a justified "nothing to visually test") per CLAUDE.md.

**Authoritative spec:** `docs/superpowers/specs/2026-07-05-p1-masking-design.md` (§5, §6, §7, §10, §12 plan 3; honor §13). Context: `2026-06-30-spec2-editing-design.md` (OpStack/DAG/sidecar/two-tier) and `2026-07-01-spec3-color-and-export-design.md` §5 (canonical op order).

---

## File Structure

**Create:**
- `ferrolite-pipeline/src/local.rs` — the `LocalAdjustments` / `MaskLayer` / `AdjustmentSet` data model (pure data; serde; per-control + per-layer reset). ~250 lines.
- `ferrolite-pipeline/src/local_node.rs` — `LocalAdjustmentsNode` (`Node<PipelineImage>`) + its build-once mask-apply pass and mask-composite orchestration. ~350 lines.
- `ferrolite-pipeline/src/coord.rs` — pure display→source inverse coordinate mapping through crop+rotate (lens-identity fallback). ~120 lines.
- `ferrolite-pipeline/src/shaders/local_adjust.wgsl` — the Light+Color masked-apply compute pass.
- `ferrolite-pipeline/tests/local_golden.rs` — the full-stack + node goldens (headless-skip).
- `ferrolite-pipeline/tests/local_persistence.rs` — `frl:ops` round-trip incl. `LocalAdjustments` (pure; runs everywhere).

**Modify:**
- `ferrolite-pipeline/Cargo.toml` — add `ferrolite-mask` dependency.
- `ferrolite-pipeline/src/op.rs` — add `Op::LocalAdjustments`, `OpKind::LocalAdjustments` (renumber), `Op::kind` arm, `local_adjustments()` accessor.
- `ferrolite-pipeline/src/uniforms.rs` — `LocalAdjustUniform` Pod struct + `local_adjust_uniform()` conversion + the pure CPU reference `light_color_apply()`.
- `ferrolite-pipeline/src/lib.rs` — declare the new modules; re-export the new public types; add `local_adjust.wgsl` to `prewarm_shaders`.
- `ferrolite-pipeline/src/pipeline.rs` — insert `LocalAdjustmentsNode` between `hsl` and `sharpen` in `EditPipeline`; `set_stack` diffing; `node_count` 10→11.
- `ferrolite-pipeline/src/tile_edit.rs` — insert `LocalAdjustmentsNode` between `hsl` and `sharpen`; full-output mask cache; per-tile mask origin; rebuild on local-adjust change.

---

## Interfaces (the names later tasks depend on — defined here once)

```rust
// ferrolite-pipeline/src/local.rs
pub struct AdjustmentSet {
    // Light
    pub exposure: f32, pub contrast: f32, pub highlights: f32,
    pub shadows: f32, pub whites: f32, pub blacks: f32,
    // Color
    pub temp: f32, pub tint: f32, pub saturation: f32, pub hue: f32,
    pub color: ColorSwatch,
    // Reserved (no shader in P1)
    pub texture: f32, pub clarity: f32, pub dehaze: f32,
    pub sharpness: f32, pub noise: f32,
}
pub struct ColorSwatch { pub r: f32, pub g: f32, pub b: f32, pub amount: f32 } // amount 0 = identity
pub enum LightControl { Exposure, Contrast, Highlights, Shadows, Whites, Blacks }
pub enum ColorControl { Temp, Tint, Saturation, Hue, Color }
pub struct MaskLayer { pub name: String, pub visible: bool,
                       pub mask: ferrolite_mask::MaskDefinition, pub adjustments: AdjustmentSet }
pub struct LocalAdjustments { pub layers: Vec<MaskLayer> }

impl AdjustmentSet {
    pub fn is_identity(&self) -> bool;                 // all point-op fields zero + swatch.amount 0
    pub fn reset_light(&self, c: LightControl) -> Self; // returns a copy with one field zeroed
    pub fn reset_color(&self, c: ColorControl) -> Self;
}
impl LocalAdjustments {
    pub fn is_identity(&self) -> bool;                 // no visible, non-identity layers
    pub fn visible_layers(&self) -> impl Iterator<Item = &MaskLayer>;
}
// Default for all: identity (Default derive where possible; manual where f32-zero == identity).

// ferrolite-pipeline/src/op.rs
Op::LocalAdjustments(LocalAdjustments)               // new variant
OpKind::LocalAdjustments = 5                         // Sharpen=6, LensCorrection=7, Geometry=8
OpStack::local_adjustments(&self) -> Option<LocalAdjustments>

// ferrolite-pipeline/src/uniforms.rs
#[repr(C)] pub struct LocalAdjustUniform { /* Pod, 16-byte aligned, see Task 3 */ }
pub fn local_adjust_uniform(a: &AdjustmentSet) -> LocalAdjustUniform;
pub fn light_color_apply(rgb: [f32; 3], a: &AdjustmentSet) -> [f32; 3]; // CPU ref, mirrors WGSL

// ferrolite-pipeline/src/local_node.rs
pub(crate) struct LocalAdjustmentsNode { /* see Task 5 */ }
impl LocalAdjustmentsNode {
    pub(crate) fn new(ctx: Arc<GpuContext>, layers: Rc<RefCell<LocalAdjustments>>) -> Self;
    // full-output mask cache control for the tile tier:
    pub(crate) fn set_mask_origin(&self, origin: [i32; 2]);  // tile tier sets per tile; preview leaves [0,0]
    pub(crate) fn set_full_dims(&self, dims: (u32, u32));    // full output dims for the mask composite
}
impl Node<PipelineImage> for LocalAdjustmentsNode { /* per-layer composite→apply→accumulate */ }
impl Node<PipelineImage> for Rc<LocalAdjustmentsNode> { /* delegating, like Rc<GeometryNode> */ }

// ferrolite-pipeline/src/coord.rs
pub fn display_to_source(geo: Option<Geometry>, src_w: u32, src_h: u32,
                         out_norm: (f32, f32)) -> (f32, f32); // → source-normalized [0,1]
```

---

## Task 1: `AdjustmentSet` / `MaskLayer` / `LocalAdjustments` data model

**Files:**
- Create: `ferrolite-pipeline/src/local.rs`
- Modify: `ferrolite-pipeline/Cargo.toml` (add `ferrolite-mask`)
- Modify: `ferrolite-pipeline/src/lib.rs` (declare `mod local;`, re-export)

**Interfaces:**
- Consumes: `ferrolite_mask::MaskDefinition` (already public).
- Produces: `AdjustmentSet`, `ColorSwatch`, `LightControl`, `ColorControl`, `MaskLayer`, `LocalAdjustments` (see Interfaces block).

- [ ] **Step 1: Add the crate dependency.** In `ferrolite-pipeline/Cargo.toml`, under `[dependencies]` after `ferrolite-lens`:

```toml
ferrolite-mask = { workspace = true }
```

Verify the workspace root `Cargo.toml` already lists `ferrolite-mask` under `[workspace.dependencies]` (it does — Plan 1). If not, add `ferrolite-mask = { path = "ferrolite-mask" }` there.

- [ ] **Step 2: Write the failing test.** Create `ferrolite-pipeline/src/local.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_mask::{CompositeMode, MaskComponent, MaskDefinition, Vec2 as MVec2};

    #[test]
    fn adjustment_set_default_is_identity() {
        let a = AdjustmentSet::default();
        assert!(a.is_identity());
        assert_eq!(a.exposure, 0.0);
        assert_eq!(a.color.amount, 0.0);
    }

    #[test]
    fn reset_light_zeroes_one_control_only() {
        let a = AdjustmentSet { exposure: 0.5, contrast: 0.3, ..Default::default() };
        let r = a.reset_light(LightControl::Exposure);
        assert_eq!(r.exposure, 0.0, "exposure reset");
        assert_eq!(r.contrast, 0.3, "contrast untouched");
    }

    #[test]
    fn reset_color_zeroes_one_control_only() {
        let a = AdjustmentSet { temp: 0.4, saturation: -0.2,
            color: ColorSwatch { r: 1.0, g: 0.0, b: 0.0, amount: 0.5 }, ..Default::default() };
        assert_eq!(a.reset_color(ColorControl::Temp).temp, 0.0);
        assert_eq!(a.reset_color(ColorControl::Temp).saturation, -0.2);
        assert_eq!(a.reset_color(ColorControl::Color).color.amount, 0.0);
    }

    #[test]
    fn local_adjustments_default_is_identity() {
        assert!(LocalAdjustments::default().is_identity());
    }

    #[test]
    fn only_visible_layers_are_iterated() {
        let hidden = MaskLayer { name: "a".into(), visible: false,
            mask: MaskDefinition::default(), adjustments: AdjustmentSet { exposure: 1.0, ..Default::default() } };
        let shown = MaskLayer { name: "b".into(), visible: true,
            mask: MaskDefinition::default(), adjustments: AdjustmentSet { exposure: 1.0, ..Default::default() } };
        let la = LocalAdjustments { layers: vec![hidden, shown] };
        let names: Vec<&str> = la.visible_layers().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["b"]);
        assert!(!la.is_identity(), "one visible non-identity layer is not identity");
    }

    #[test]
    fn model_round_trips_through_json() {
        let la = LocalAdjustments { layers: vec![MaskLayer {
            name: "sky".into(), visible: true,
            mask: MaskDefinition { components: vec![(
                MaskComponent::LinearGradient { start: MVec2::new(0.0, 0.0), end: MVec2::new(0.0, 1.0) },
                CompositeMode::Add)], invert: false },
            adjustments: AdjustmentSet { exposure: -0.5, temp: 0.3,
                color: ColorSwatch { r: 0.2, g: 0.4, b: 0.9, amount: 0.25 }, ..Default::default() },
        }] };
        let json = serde_json::to_string(&la).unwrap();
        assert_eq!(serde_json::from_str::<LocalAdjustments>(&json).unwrap(), la);
    }
}
```

- [ ] **Step 3: Run it to confirm it fails to compile.**

Run: `cargo test -p ferrolite-pipeline --lib local::tests`
Expected: FAIL — `AdjustmentSet`/`MaskLayer`/`LocalAdjustments` not defined.

- [ ] **Step 4: Write the model.** Prepend to `ferrolite-pipeline/src/local.rs`:

```rust
//! The local-adjustments document sub-model: an ordered stack of `MaskLayer`s,
//! each pairing a parametric `MaskDefinition` (ferrolite-mask, engine tier) with a
//! per-mask Light+Color `AdjustmentSet` (photo tier). Pure data — `Clone`,
//! `PartialEq`, serde. Applied by `LocalAdjustmentsNode`; persisted in `frl:ops`.
//! Reserved neighborhood fields (texture/clarity/dehaze/sharpness/noise) are
//! carried for schema stability but have no shader in P1 (P3/P4 own them).

use serde::{Deserialize, Serialize};

use ferrolite_mask::MaskDefinition;

/// A color/tint overlay swatch. `amount` 0 = identity (no tint).
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct ColorSwatch {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub amount: f32,
}

/// Per-mask point-op adjustments. All scalars are zero-identity; `Default` is the
/// no-op set. Serde uses `#[serde(default)]` on every field so a payload written
/// by an older/newer build (missing/extra fields) loads as identity for those.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct AdjustmentSet {
    #[serde(default)] pub exposure: f32,
    #[serde(default)] pub contrast: f32,
    #[serde(default)] pub highlights: f32,
    #[serde(default)] pub shadows: f32,
    #[serde(default)] pub whites: f32,
    #[serde(default)] pub blacks: f32,
    #[serde(default)] pub temp: f32,
    #[serde(default)] pub tint: f32,
    #[serde(default)] pub saturation: f32,
    #[serde(default)] pub hue: f32,
    #[serde(default)] pub color: ColorSwatch,
    // Reserved neighborhood locals — no shader in P1 (greyed in Plan 4's UI).
    #[serde(default)] pub texture: f32,
    #[serde(default)] pub clarity: f32,
    #[serde(default)] pub dehaze: f32,
    #[serde(default)] pub sharpness: f32,
    #[serde(default)] pub noise: f32,
}

/// A single Light control (per-control reset target).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LightControl { Exposure, Contrast, Highlights, Shadows, Whites, Blacks }

/// A single Color control (per-control reset target).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorControl { Temp, Tint, Saturation, Hue, Color }

impl AdjustmentSet {
    /// True when every point-op field is zero-identity (reserved fields ignored —
    /// they carry no shader so cannot change output in P1).
    pub fn is_identity(&self) -> bool {
        self.exposure == 0.0 && self.contrast == 0.0 && self.highlights == 0.0
            && self.shadows == 0.0 && self.whites == 0.0 && self.blacks == 0.0
            && self.temp == 0.0 && self.tint == 0.0 && self.saturation == 0.0
            && self.hue == 0.0 && self.color.amount == 0.0
    }

    /// New set with one Light control reset to identity (immutable per-control reset).
    pub fn reset_light(&self, c: LightControl) -> Self {
        let mut s = *self;
        match c {
            LightControl::Exposure => s.exposure = 0.0,
            LightControl::Contrast => s.contrast = 0.0,
            LightControl::Highlights => s.highlights = 0.0,
            LightControl::Shadows => s.shadows = 0.0,
            LightControl::Whites => s.whites = 0.0,
            LightControl::Blacks => s.blacks = 0.0,
        }
        s
    }

    /// New set with one Color control reset to identity.
    pub fn reset_color(&self, c: ColorControl) -> Self {
        let mut s = *self;
        match c {
            ColorControl::Temp => s.temp = 0.0,
            ColorControl::Tint => s.tint = 0.0,
            ColorControl::Saturation => s.saturation = 0.0,
            ColorControl::Hue => s.hue = 0.0,
            ColorControl::Color => s.color = ColorSwatch::default(),
        }
        s
    }
}

/// One mask + its adjustments. `MaskDefinition` is the engine-tier parametric mask
/// (source of truth); `adjustments` is what applies through it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct MaskLayer {
    pub name: String,
    pub visible: bool,
    #[serde(default)]
    pub mask: MaskDefinition,
    #[serde(default)]
    pub adjustments: AdjustmentSet,
}

/// The `Op::LocalAdjustments` payload: an ordered stack of mask layers applied as a
/// single pipeline stage (design §13 — N masks inside one op).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct LocalAdjustments {
    #[serde(default)]
    pub layers: Vec<MaskLayer>,
}

impl LocalAdjustments {
    /// Visible layers, in stack order (the only ones that affect output).
    pub fn visible_layers(&self) -> impl Iterator<Item = &MaskLayer> {
        self.layers.iter().filter(|l| l.visible)
    }

    /// True when no visible layer would change the image (empty, all hidden, or every
    /// visible layer is an identity adjustment).
    pub fn is_identity(&self) -> bool {
        self.visible_layers().all(|l| l.adjustments.is_identity())
    }
}
```

- [ ] **Step 5: Declare the module + re-export.** In `ferrolite-pipeline/src/lib.rs`, add `mod local;` beside the other `mod` lines, and add to the `pub use` list:

```rust
pub use local::{
    AdjustmentSet, ColorControl, ColorSwatch, LightControl, LocalAdjustments, MaskLayer,
};
```

- [ ] **Step 6: Run tests to verify they pass.**

Run: `cargo test -p ferrolite-pipeline --lib local::tests`
Expected: PASS (6 tests).

- [ ] **Step 7: Commit.**

```bash
git add ferrolite-pipeline/Cargo.toml ferrolite-pipeline/src/local.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): LocalAdjustments/MaskLayer/AdjustmentSet model + per-control reset"
```

---

## Task 2: `Op::LocalAdjustments` + `OpKind` insertion after `Hsl`

**Files:**
- Modify: `ferrolite-pipeline/src/op.rs:148-188` (the `Op` enum, `OpKind` enum, `Op::kind`), plus a new accessor + tests.

**Interfaces:**
- Consumes: `crate::local::LocalAdjustments` (Task 1).
- Produces: `Op::LocalAdjustments(LocalAdjustments)`, `OpKind::LocalAdjustments = 5`, `OpStack::local_adjustments()`.

- [ ] **Step 1: Write the failing tests.** Add to the `#[cfg(test)] mod tests` in `op.rs`:

```rust
#[test]
fn local_adjustments_sorts_between_hsl_and_sharpen() {
    use crate::local::{AdjustmentSet, LocalAdjustments, MaskLayer};
    use ferrolite_mask::MaskDefinition;
    let la = LocalAdjustments { layers: vec![MaskLayer {
        name: "m".into(), visible: true, mask: MaskDefinition::default(),
        adjustments: AdjustmentSet { exposure: 0.5, ..Default::default() } }] };
    let s = OpStack::default()
        .set_op(Op::Sharpen(Sharpen { amount: 0.3, radius: 1 }))
        .set_op(Op::LocalAdjustments(la.clone()))
        .set_op(Op::Hsl(Hsl { bands: [HslBand { hue: 0.0, sat: 0.0, lum: 0.0 }; 8] }));
    let kinds: Vec<OpKind> = s.ops.iter().map(|o| o.kind()).collect();
    assert_eq!(kinds, vec![OpKind::Hsl, OpKind::LocalAdjustments, OpKind::Sharpen]);
    assert_eq!(s.local_adjustments(), Some(la));
}

#[test]
fn opkind_discriminants_place_local_adjustments_after_hsl() {
    assert_eq!(OpKind::Hsl as u8, 4);
    assert_eq!(OpKind::LocalAdjustments as u8, 5);
    assert_eq!(OpKind::Sharpen as u8, 6);
    assert_eq!(OpKind::LensCorrection as u8, 7);
    assert_eq!(OpKind::Geometry as u8, 8);
}

#[test]
fn opkind_renumber_does_not_change_serde_output() {
    // OpKind is a sort key, never serialized; Op serializes by variant name.
    // This exact JSON must be stable across the renumber.
    let s = OpStack::default()
        .set_op(Op::Exposure(Exposure { ev: 0.5 }))
        .set_op(Op::Sharpen(Sharpen { amount: 0.6, radius: 3 }));
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(
        json,
        r#"{"version":1,"ops":[{"Exposure":{"ev":0.5}},{"Sharpen":{"amount":0.6,"radius":3}}]}"#
    );
}
```

- [ ] **Step 2: Run to verify they fail.**

Run: `cargo test -p ferrolite-pipeline --lib op::tests::local_adjustments_sorts_between_hsl_and_sharpen`
Expected: FAIL — `Op::LocalAdjustments` / `OpKind::LocalAdjustments` / `local_adjustments` not defined.

- [ ] **Step 3: Add the variant + renumber.** In `ferrolite-pipeline/src/op.rs`:

At the top with the other `use`:
```rust
use crate::local::LocalAdjustments;
```

In `enum Op` (currently ends `Hsl(Hsl), Sharpen(Sharpen), ...`), insert after `Hsl(Hsl),`:
```rust
    LocalAdjustments(LocalAdjustments),
```

Replace the `enum OpKind` block with the renumbered version:
```rust
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    Exposure = 0,
    WhiteBalance = 1,
    Contrast = 2,
    ToneCurve = 3,
    Hsl = 4,
    LocalAdjustments = 5,
    Sharpen = 6,
    LensCorrection = 7,
    Geometry = 8,
}
```

In `Op::kind`, add the arm after the `Hsl` arm:
```rust
            Op::LocalAdjustments(_) => OpKind::LocalAdjustments,
```

- [ ] **Step 4: Add the accessor.** In `impl OpStack`, after `hsl()`:
```rust
    pub fn local_adjustments(&self) -> Option<LocalAdjustments> {
        self.ops.iter().find_map(|o| match o {
            Op::LocalAdjustments(l) => Some(l.clone()),
            _ => None,
        })
    }
```

- [ ] **Step 5: Run tests.**

Run: `cargo test -p ferrolite-pipeline --lib op::tests`
Expected: PASS (all existing op tests + the 3 new ones). If `full_seven_op_stack_is_in_canonical_order` or `lens_correction_sits_before_geometry_in_canonical_order` still pass unchanged, the renumber is transparent.

- [ ] **Step 6: Commit.**

```bash
git add ferrolite-pipeline/src/op.rs
git commit -m "feat(pipeline): Op::LocalAdjustments + OpKind after Hsl (serde output unchanged)"
```

---

## Task 3: `LocalAdjustUniform` + `local_adjust_uniform` + CPU reference `light_color_apply`

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs` (add the Pod struct, conversion, CPU ref, tests).
- Modify: `ferrolite-pipeline/src/lib.rs` (re-export `LocalAdjustUniform`).

**Interfaces:**
- Consumes: `crate::local::AdjustmentSet` (Task 1), existing `wb_multipliers`, `exposure_gain`, `CONTRAST_PIVOT`.
- Produces: `LocalAdjustUniform`, `local_adjust_uniform(&AdjustmentSet)`, `light_color_apply([f32;3], &AdjustmentSet)`. **The CPU `light_color_apply` and `local_adjust.wgsl` (Task 4) must implement identical math.**

- [ ] **Step 1: Write the failing tests.** Add to `#[cfg(test)] mod tests` in `uniforms.rs`:

```rust
#[test]
fn light_color_identity_is_a_no_op() {
    use crate::local::AdjustmentSet;
    let c = light_color_apply([0.4, 0.5, 0.6], &AdjustmentSet::default());
    assert!((c[0] - 0.4).abs() < 1e-6 && (c[1] - 0.5).abs() < 1e-6 && (c[2] - 0.6).abs() < 1e-6);
}

#[test]
fn light_color_exposure_plus_one_doubles() {
    use crate::local::AdjustmentSet;
    let c = light_color_apply([0.2, 0.2, 0.2], &AdjustmentSet { exposure: 1.0, ..Default::default() });
    assert!((c[0] - 0.4).abs() < 1e-4, "got {}", c[0]);
}

#[test]
fn light_color_contrast_pushes_away_from_pivot() {
    use crate::local::AdjustmentSet;
    // A value above the 0.18 pivot moves further up under positive contrast.
    let c = light_color_apply([0.5, 0.5, 0.5], &AdjustmentSet { contrast: 0.5, ..Default::default() });
    assert!(c[0] > 0.5, "above-pivot value brightened: {}", c[0]);
}

#[test]
fn light_color_full_desaturation_goes_grey() {
    use crate::local::AdjustmentSet;
    let c = light_color_apply([0.9, 0.1, 0.1], &AdjustmentSet { saturation: -1.0, ..Default::default() });
    assert!((c[0] - c[1]).abs() < 1e-4 && (c[1] - c[2]).abs() < 1e-4, "grey: {c:?}");
}

#[test]
fn light_color_warm_temp_raises_red_over_blue() {
    use crate::local::AdjustmentSet;
    let c = light_color_apply([0.5, 0.5, 0.5], &AdjustmentSet { temp: 0.8, ..Default::default() });
    assert!(c[0] > c[2], "warm temp: r={} b={}", c[0], c[2]);
}

#[test]
fn local_adjust_uniform_is_identity_when_default() {
    use crate::local::AdjustmentSet;
    let u = local_adjust_uniform(&AdjustmentSet::default());
    assert_eq!(u.exposure_gain, 1.0);
    assert_eq!(u.contrast_gain, 1.0);
    assert_eq!(u.wb_mul, [1.0, 1.0, 1.0]);
    assert_eq!(std::mem::size_of::<LocalAdjustUniform>() % 16, 0);
}

#[test]
fn reserved_fields_do_not_change_output() {
    use crate::local::AdjustmentSet;
    let a = AdjustmentSet { texture: 1.0, clarity: 1.0, dehaze: 1.0, sharpness: 1.0, noise: 1.0, ..Default::default() };
    assert_eq!(light_color_apply([0.3, 0.4, 0.5], &a), [0.3, 0.4, 0.5]);
}
```

- [ ] **Step 2: Run to verify they fail.**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests::light_color_identity_is_a_no_op`
Expected: FAIL — items not defined.

- [ ] **Step 3: Implement the uniform + conversion + CPU reference.** Add to `uniforms.rs`:

```rust
/// Max hue rotation (degrees) per unit `AdjustmentSet::hue`. Local hue spans a
/// full turn at ±1 (pragmatic; image science secondary, like `wb_multipliers`).
pub const MAX_LOCAL_HUE_DEG: f32 = 180.0;

/// GPU uniform for `local_adjust.wgsl`. `#[repr(C)]`, 16-byte aligned. Field order +
/// padding MIRROR the WGSL `struct P` exactly. `mask_origin` lets the tile tier read
/// a sub-region of a full-output mask (preview leaves it `[0,0]`).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LocalAdjustUniform {
    pub exposure_gain: f32, // 2^exposure
    pub contrast_gain: f32, // 1 + contrast
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32, // 1 + saturation (mix factor)
    pub hue_deg: f32,    // hue * MAX_LOCAL_HUE_DEG
    pub wb_mul: [f32; 3],
    pub color_amount: f32,
    pub color_rgb: [f32; 3],
    pub contrast_pivot: f32,
    pub mask_origin: [i32; 2],
    pub _pad: [f32; 2],
}

pub fn local_adjust_uniform(a: &crate::local::AdjustmentSet) -> LocalAdjustUniform {
    LocalAdjustUniform {
        exposure_gain: exposure_gain(a.exposure),
        contrast_gain: 1.0 + a.contrast,
        highlights: a.highlights,
        shadows: a.shadows,
        whites: a.whites,
        blacks: a.blacks,
        saturation: 1.0 + a.saturation,
        hue_deg: a.hue * MAX_LOCAL_HUE_DEG,
        wb_mul: wb_multipliers(a.temp, a.tint),
        color_amount: a.color.amount,
        color_rgb: [a.color.r, a.color.g, a.color.b],
        contrast_pivot: CONTRAST_PIVOT,
        mask_origin: [0, 0],
        _pad: [0.0; 2],
    }
}

fn luma709(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn rgb_to_hsl(c: [f32; 3]) -> [f32; 3] {
    let (r, g, b) = (c[0], c[1], c[2]);
    let mx = r.max(g.max(b));
    let mn = r.min(g.min(b));
    let l = (mx + mn) * 0.5;
    let d = mx - mn;
    if d <= 1e-6 { return [0.0, 0.0, l]; }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let mut h = if mx == r { ((g - b) / d) % 6.0 }
        else if mx == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
    h *= 60.0;
    if h < 0.0 { h += 360.0; }
    [h, s, l]
}
fn hsl_to_rgb(hsl: [f32; 3]) -> [f32; 3] {
    let (h, s, l) = (hsl[0] / 360.0, hsl[1], hsl[2]);
    if s <= 1e-6 { return [l, l, l]; }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |t_in: f32| -> f32 {
        let mut t = t_in;
        if t < 0.0 { t += 1.0; }
        if t > 1.0 { t -= 1.0; }
        if t < 1.0 / 6.0 { p + (q - p) * 6.0 * t }
        else if t < 1.0 / 2.0 { q }
        else if t < 2.0 / 3.0 { p + (q - p) * (2.0 / 3.0 - t) * 6.0 }
        else { p }
    };
    [hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0)]
}

/// CPU reference for the Light+Color point op. `local_adjust.wgsl` mirrors this
/// exactly (golden tolerance absorbs f16/driver drift). Order: exposure → tonal
/// region gains → contrast → wb → saturation → hue → color swatch. Output clamped ≥0.
pub fn light_color_apply(rgb: [f32; 3], a: &crate::local::AdjustmentSet) -> [f32; 3] {
    let u = local_adjust_uniform(a);
    let mut c = [rgb[0] * u.exposure_gain, rgb[1] * u.exposure_gain, rgb[2] * u.exposure_gain];
    let y = luma709(c);
    let hi = smoothstep(0.5, 1.0, y);
    let sh = 1.0 - smoothstep(0.0, 0.5, y);
    let wh = smoothstep(0.7, 1.0, y);
    let bl = 1.0 - smoothstep(0.0, 0.3, y);
    let region = (1.0 + u.highlights * hi) * (1.0 + u.shadows * sh)
        * (1.0 + u.whites * wh) * (1.0 + u.blacks * bl);
    for v in &mut c { *v *= region; }
    for v in &mut c { *v = (*v - u.contrast_pivot) * u.contrast_gain + u.contrast_pivot; }
    for i in 0..3 { c[i] *= u.wb_mul[i]; }
    let y2 = luma709(c);
    for v in &mut c { *v = y2 + (*v - y2) * u.saturation; }
    if u.hue_deg != 0.0 {
        let mut hsl = rgb_to_hsl([c[0].max(0.0), c[1].max(0.0), c[2].max(0.0)]);
        hsl[0] = (hsl[0] + u.hue_deg).rem_euclid(360.0);
        c = hsl_to_rgb(hsl);
    }
    if u.color_amount != 0.0 {
        for i in 0..3 { c[i] += (u.color_rgb[i] - c[i]) * u.color_amount; }
    }
    [c[0].max(0.0), c[1].max(0.0), c[2].max(0.0)]
}
```

- [ ] **Step 4: Re-export.** In `lib.rs`, add `LocalAdjustUniform` to the `pub use uniforms::{...}` list.

- [ ] **Step 5: Run tests.**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests`
Expected: PASS (existing + 7 new).

- [ ] **Step 6: Commit.**

```bash
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): Light+Color param->uniform + CPU reference (light_color_apply)"
```

---

## Task 4: `local_adjust.wgsl` masked-apply compute pass

**Files:**
- Create: `ferrolite-pipeline/src/shaders/local_adjust.wgsl`
- Modify: `ferrolite-pipeline/src/lib.rs` (`prewarm_shaders`)

**Interfaces:**
- Consumes: the `LocalAdjustUniform` layout (Task 3) as WGSL `struct P`.
- Produces: WGSL entry `main`; bind layout **0**=src color `texture_2d<f32>`, **1**=mask `texture_2d<f32>` (R32F, textureLoad), **2**=dst `texture_storage_2d<rgba16float, write>`, **3**=`P` uniform. Consumed by Task 5's `LocalAdjustmentsNode`.

- [ ] **Step 1: Write the shader.** Create `ferrolite-pipeline/src/shaders/local_adjust.wgsl`:

```wgsl
// Local Light+Color point op, blended by a mask. Mirrors uniforms::light_color_apply
// exactly. `dst[xy] = mix(src[xy], adjusted(src[xy]), mask[mask_origin + xy])`, so a
// mask value of 0 leaves the pixel untouched and 1 applies the full adjustment.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var mask: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;
struct P {
    exposure_gain: f32, contrast_gain: f32, highlights: f32, shadows: f32,
    whites: f32, blacks: f32, saturation: f32, hue_deg: f32,
    wb_mul: vec3<f32>, color_amount: f32,
    color_rgb: vec3<f32>, contrast_pivot: f32,
    mask_origin: vec2<i32>, pad: vec2<f32>,
};
@group(0) @binding(3) var<uniform> p: P;

fn luma709(c: vec3<f32>) -> f32 { return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722)); }

fn rgb2hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b)); let mn = min(c.r, min(c.g, c.b));
    let l = (mx + mn) * 0.5; let d = mx - mn;
    var h = 0.0; var s = 0.0;
    if (d > 1e-6) {
        s = d / (1.0 - abs(2.0 * l - 1.0));
        if (mx == c.r) { h = ((c.g - c.b) / d) % 6.0; }
        else if (mx == c.g) { h = (c.b - c.r) / d + 2.0; }
        else { h = (c.r - c.g) / d + 4.0; }
        h = h * 60.0; if (h < 0.0) { h = h + 360.0; }
    }
    return vec3<f32>(h, s, l);
}
fn hue2rgb(pp: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in; if (t < 0.0) { t = t + 1.0; } if (t > 1.0) { t = t - 1.0; }
    if (t < 1.0 / 6.0) { return pp + (q - pp) * 6.0 * t; }
    if (t < 1.0 / 2.0) { return q; }
    if (t < 2.0 / 3.0) { return pp + (q - pp) * (2.0 / 3.0 - t) * 6.0; }
    return pp;
}
fn hsl2rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x / 360.0; let s = hsl.y; let l = hsl.z;
    if (s <= 1e-6) { return vec3<f32>(l, l, l); }
    var q = l + s - l * s; if (l < 0.5) { q = l * (1.0 + s); }
    let pp = 2.0 * l - q;
    return vec3<f32>(hue2rgb(pp, q, h + 1.0 / 3.0), hue2rgb(pp, q, h), hue2rgb(pp, q, h - 1.0 / 3.0));
}

fn adjust(rgb: vec3<f32>) -> vec3<f32> {
    var c = rgb * p.exposure_gain;
    let y = luma709(c);
    let hi = smoothstep(0.5, 1.0, y);
    let sh = 1.0 - smoothstep(0.0, 0.5, y);
    let wh = smoothstep(0.7, 1.0, y);
    let bl = 1.0 - smoothstep(0.0, 0.3, y);
    let region = (1.0 + p.highlights * hi) * (1.0 + p.shadows * sh)
        * (1.0 + p.whites * wh) * (1.0 + p.blacks * bl);
    c = c * region;
    c = (c - vec3<f32>(p.contrast_pivot)) * p.contrast_gain + vec3<f32>(p.contrast_pivot);
    c = c * p.wb_mul;
    let y2 = luma709(c);
    c = vec3<f32>(y2) + (c - vec3<f32>(y2)) * p.saturation;
    if (p.hue_deg != 0.0) {
        var hsl = rgb2hsl(max(c, vec3<f32>(0.0)));
        hsl.x = hsl.x + p.hue_deg;
        hsl.x = hsl.x - floor(hsl.x / 360.0) * 360.0;
        c = hsl2rgb(hsl);
    }
    if (p.color_amount != 0.0) { c = c + (p.color_rgb - c) * p.color_amount; }
    return max(c, vec3<f32>(0.0));
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    let m = textureLoad(mask, p.mask_origin + xy, 0).r;
    let out = mix(c.rgb, adjust(c.rgb), clamp(m, 0.0, 1.0));
    textureStore(dst, xy, vec4<f32>(out, c.a));
}
```

- [ ] **Step 2: Add to prewarm.** In `lib.rs` `prewarm_shaders`, add to the array:
```rust
        ("local-adjust", include_str!("shaders/local_adjust.wgsl")),
```
Update the doc comment's pass count.

- [ ] **Step 3: Verify it compiles into the module cache.** This shader is exercised by Task 5's node golden; here just confirm the workspace builds.

Run: `cargo build -p ferrolite-pipeline`
Expected: PASS (no WGSL parse error surfaced at build; naga validates on first `shader_module`).

- [ ] **Step 4: Commit.**

```bash
git add ferrolite-pipeline/src/shaders/local_adjust.wgsl ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): local_adjust.wgsl Light+Color masked-apply pass"
```

---

## Task 5: `LocalAdjustmentsNode`

**Files:**
- Create: `ferrolite-pipeline/src/local_node.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (`mod local_node;`)
- Create: `ferrolite-pipeline/tests/local_golden.rs` (node golden; headless-skip)

**Interfaces:**
- Consumes: `PipelineImage`, `ferrolite_mask::{MaskBuffer, MaskDefinition, MaskComponent, CompositeMode, CompositePass, LinearGradientPass, RadialGradientPass, LumaRangePass, ColorRangePass, BrushRasterizer, stroke_dabs}`, `crate::uniforms::{LocalAdjustUniform, local_adjust_uniform}`, `crate::local::LocalAdjustments`.
- Produces: `LocalAdjustmentsNode` (`pub(crate)`), its `Node<PipelineImage>` + `Node for Rc<LocalAdjustmentsNode>` impls, and `new`/`set_mask_origin`/`set_full_dims`. Consumed by Tasks 7 & 9.

**Design notes (read before coding):**
- The node owns build-once instances of every mask pass + one build-once `local_adjust` apply pass, plus `layers: Rc<RefCell<LocalAdjustments>>` (non-`Copy` → `RefCell`, not `Cell`).
- Per `evaluate`: start from `inputs[0]` (post-`Hsl`); for each **visible** layer composite its `MaskDefinition` into one `MaskBuffer` at the **mask resolution**, then run the apply pass reading the input color, that mask (offset by `mask_origin`), and the layer's `local_adjust_uniform`, feeding the result forward. No visible/identity layers → clone input (identity).
- **Mask resolution:** `full_dims` (default = the input image dims; the tile tier sets it to the full output dims so the mask is composited once at full res and each tile samples its sub-region). `mask_origin` (default `[0,0]`; the tile tier sets the tile's global output origin so `textureLoad(mask, origin+xy)` reads the right region).
- **Component → `MaskBuffer`:**
  - `LinearGradient` → `LinearGradientPass::run`; `RadialGradient` → `RadialGradientPass::run`; `LumaRange`/`ColorRange` → the sampled passes over the **input color view** (post-`Hsl`, what the user perceives — §5.2).
  - `Brush { strokes }` → seed `MaskBuffer::alloc_zeroed`, then for each `Stroke` compute `stroke_dabs(&stroke.nodes)` and `BrushRasterizer::stamp_onto(&acc, &dabs, stroke.erase, (0,0), (w,h))`, threading the accumulator.
  - `Imported { .. }` → a zeroed buffer (inert in P1 — no producer; Plan 5 wires it). Documented.
  - Empty `MaskDefinition.components` → a **ones** buffer (identity mask = adjustment applies everywhere), unless `invert` (then zeroed). Matches `composite_scalar` (empty→1.0, empty+invert→0.0).
  - Non-empty → `CompositePass::composite(&[(buf, mode), ...], invert)`.
- **Full mask cache (tile tier):** recompositing per tile is wasteful. The node caches the composited mask per layer keyed by `(full_dims, layers snapshot)`; the cache is invalidated when `layers`/`full_dims` change. For the preview tier `full_dims` == image dims and the cache trivially holds one entry. (Implement the cache as `RefCell<Option<CachedMasks>>` storing the layer `Vec<MaskBuffer>` + the `LocalAdjustments` they were built from + `full_dims`; rebuild when any differ.)

- [ ] **Step 1: Write the node golden test first.** Create `ferrolite-pipeline/tests/local_golden.rs`:

```rust
mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{CompositeMode, MaskComponent, MaskDefinition, Vec2 as MVec2};
use ferrolite_pipeline::{
    AdjustmentSet, EditPipeline, LocalAdjustments, MaskLayer, Op, OpStack,
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
    let la = LocalAdjustments { layers: vec![MaskLayer {
        name: "spot".into(), visible: true,
        mask: MaskDefinition { components: vec![(
            MaskComponent::RadialGradient {
                center: MVec2::new(0.5, 0.5), radius: MVec2::new(0.3, 0.3),
                rotation: 0.0, feather: 0.4, invert: false },
            CompositeMode::Add)], invert: false },
        adjustments: AdjustmentSet { exposure: 1.0, ..Default::default() },
    }] };
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
    let la = LocalAdjustments { layers: vec![MaskLayer {
        name: "off".into(), visible: false, mask: MaskDefinition::default(),
        adjustments: AdjustmentSet { exposure: 2.0, ..Default::default() } }] };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));
    let mut p = EditPipeline::new(ctx, &src, stack, IDENTITY);
    let got = p.render_to_image();
    assert_eq!(common::max_abs_diff(&got, &base), 0, "hidden layer changed the image");
}
```

(This test also exercises Task 7's `EditPipeline` wiring — it will not compile/pass until Task 7 lands; that's expected. Run it at the end of Task 7. For now, Task 5 verifies the node in isolation via Step 3.)

- [ ] **Step 2: Implement the node.** Create `ferrolite-pipeline/src/local_node.rs`:

```rust
//! `LocalAdjustmentsNode` — the whole masked-adjustment stage as one
//! `Node<PipelineImage>`. Per visible layer: (engine) composite the
//! `MaskDefinition` into a single `MaskBuffer`, then (photo) apply the Light+Color
//! point op blended by the mask. Inserted after `Hsl`, before `Sharpen`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Node};
use ferrolite_mask::{
    stroke_dabs, BrushRasterizer, ColorRangePass, CompositeMode, CompositePass, LinearGradientPass,
    LumaRangePass, MaskBuffer, MaskComponent, MaskDefinition, RadialGradientPass, Rgb, Vec2,
};
use wgpu::util::DeviceExt;

use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::local::LocalAdjustments;
use crate::uniforms::{local_adjust_uniform, LocalAdjustUniform};

struct CachedMasks {
    layers: LocalAdjustments,
    full_dims: (u32, u32),
    masks: Vec<MaskBuffer>, // one per visible layer, in visible order
}

pub(crate) struct LocalAdjustmentsNode {
    ctx: Arc<GpuContext>,
    layers: Rc<RefCell<LocalAdjustments>>,
    // build-once passes
    linear: LinearGradientPass,
    radial: RadialGradientPass,
    luma: LumaRangePass,
    color: ColorRangePass,
    brush: BrushRasterizer,
    composite: CompositePass,
    // apply pass
    apply_bgl: wgpu::BindGroupLayout,
    apply_pipeline: wgpu::ComputePipeline,
    apply_out: RefCell<Option<PipelineImage>>,
    // tile-tier controls
    full_dims: RefCell<Option<(u32, u32)>>, // None → use input dims
    mask_origin: RefCell<[i32; 2]>,
    cache: RefCell<Option<CachedMasks>>,
}

impl LocalAdjustmentsNode {
    pub(crate) fn new(ctx: Arc<GpuContext>, layers: Rc<RefCell<LocalAdjustments>>) -> Self {
        let apply_bgl = ctx.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("local-adjust-bgl"),
            entries: &[
                // 0: src color (filterable ok; we textureLoad)
                wgpu::BindGroupLayoutEntry {
                    binding: 0, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None },
                // 1: mask (R32Float, non-filterable, textureLoad)
                wgpu::BindGroupLayoutEntry {
                    binding: 1, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None },
                // 2: dst storage
                wgpu::BindGroupLayoutEntry {
                    binding: 2, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly, format: PIPELINE_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2 },
                    count: None },
                // 3: uniform
                wgpu::BindGroupLayoutEntry {
                    binding: 3, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false, min_binding_size: None },
                    count: None },
            ],
        });
        let module = ctx.shader_module("local-adjust", include_str!("shaders/local_adjust.wgsl"));
        let layout = ctx.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("local-adjust"), bind_group_layouts: &[&apply_bgl], push_constant_ranges: &[] });
        let apply_pipeline = ctx.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("local-adjust"), layout: Some(&layout), module: &module,
            entry_point: "main", compilation_options: Default::default(), cache: None });
        Self {
            linear: LinearGradientPass::new(ctx.clone()),
            radial: RadialGradientPass::new(ctx.clone()),
            luma: LumaRangePass::new(ctx.clone()),
            color: ColorRangePass::new(ctx.clone()),
            brush: BrushRasterizer::new(ctx.clone()),
            composite: CompositePass::new(ctx.clone()),
            apply_bgl, apply_pipeline, apply_out: RefCell::new(None),
            full_dims: RefCell::new(None), mask_origin: RefCell::new([0, 0]),
            cache: RefCell::new(None), ctx, layers,
        }
    }

    pub(crate) fn set_mask_origin(&self, origin: [i32; 2]) { *self.mask_origin.borrow_mut() = origin; }
    pub(crate) fn set_full_dims(&self, dims: (u32, u32)) {
        let mut fd = self.full_dims.borrow_mut();
        if *fd != Some(dims) { *fd = Some(dims); self.cache.borrow_mut().take(); }
    }

    /// Invalidate the cached composited masks (call when `layers` change).
    pub(crate) fn invalidate(&self) { self.cache.borrow_mut().take(); }

    fn ones_mask(&self, w: u32, h: u32) -> MaskBuffer {
        let buf = MaskBuffer::alloc(&self.ctx, w, h);
        let ones = vec![1.0f32; (buf.width * buf.height) as usize];
        self.ctx.queue.write_texture(
            wgpu::ImageCopyTexture { texture: &buf.texture, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytemuck::cast_slice(&ones),
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(buf.width * 4),
                rows_per_image: Some(buf.height) },
            wgpu::Extent3d { width: buf.width, height: buf.height, depth_or_array_layers: 1 });
        buf
    }

    fn eval_component(&self, comp: &MaskComponent, color_view: &wgpu::TextureView,
                      w: u32, h: u32) -> MaskBuffer {
        match comp {
            MaskComponent::LinearGradient { start, end } =>
                self.linear.run(Vec2::new(start.x, start.y), Vec2::new(end.x, end.y), w, h),
            MaskComponent::RadialGradient { center, radius, rotation, feather, invert } =>
                self.radial.run(Vec2::new(center.x, center.y), Vec2::new(radius.x, radius.y),
                                *rotation, *feather, *invert, w, h),
            MaskComponent::LumaRange { lo, hi, softness } =>
                self.luma.run(*lo, *hi, *softness, color_view, w, h),
            MaskComponent::ColorRange { samples, tolerance, softness } => {
                let s: Vec<Rgb> = samples.iter().map(|c| Rgb::new(c.r, c.g, c.b)).collect();
                self.color.run(&s, *tolerance, *softness, color_view, w, h)
            }
            MaskComponent::Brush { strokes } => {
                let mut acc = MaskBuffer::alloc_zeroed(&self.ctx, w, h);
                for st in strokes {
                    let dabs = stroke_dabs(&st.nodes);
                    acc = self.brush.stamp_onto(&acc, &dabs, st.erase, (0, 0), (w, h));
                }
                acc
            }
            // Inert in P1 (no producer) — contributes nothing. Plan 5 wires it.
            MaskComponent::Imported { .. } => MaskBuffer::alloc_zeroed(&self.ctx, w, h),
        }
    }

    fn composite_mask(&self, def: &MaskDefinition, color_view: &wgpu::TextureView,
                      w: u32, h: u32) -> MaskBuffer {
        if def.components.is_empty() {
            return if def.invert { MaskBuffer::alloc_zeroed(&self.ctx, w, h) } else { self.ones_mask(w, h) };
        }
        let inputs: Vec<(MaskBuffer, CompositeMode)> = def.components.iter()
            .map(|(c, m)| (self.eval_component(c, color_view, w, h), *m)).collect();
        self.composite.composite(&inputs, def.invert)
    }

    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.apply_out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((w, h)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("local-adjust-out"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
                format: PIPELINE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[] });
            *out = Some(PipelineImage { texture: Arc::new(tex), width: w, height: h });
        }
        out.as_ref().unwrap().clone()
    }

    fn apply(&self, input: &PipelineImage, mask: &MaskBuffer, u: LocalAdjustUniform) -> PipelineImage {
        let dst = self.ensure_out(input.width, input.height);
        let ubuf = self.ctx.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("local-adjust-uniform"), contents: bytemuck::bytes_of(&u),
            usage: wgpu::BufferUsages::UNIFORM });
        let src_view = input.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mask_view = mask.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("local-adjust-bind"), layout: &self.apply_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&mask_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&dst_view) },
                wgpu::BindGroupEntry { binding: 3, resource: ubuf.as_entire_binding() },
            ] });
        let mut enc = self.ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("local-adjust-pass"), timestamp_writes: None });
            pass.set_pipeline(&self.apply_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(input.width.div_ceil(8), input.height.div_ceil(8), 1);
        }
        self.ctx.queue.submit([enc.finish()]);
        dst
    }
}

impl Node<PipelineImage> for LocalAdjustmentsNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let input = inputs[0];
        let layers = self.layers.borrow();
        if layers.is_identity() {
            return input.clone();
        }
        // Mask compositing resolution: full output dims (tile tier) or input dims.
        let (mw, mh) = self.full_dims.borrow().unwrap_or((input.width, input.height));
        let input_view = input.texture.create_view(&wgpu::TextureViewDescriptor::default());

        // (Re)build the composited-mask cache if layers/full_dims changed.
        let rebuild = {
            let c = self.cache.borrow();
            match &*c { Some(cm) => cm.layers != *layers || cm.full_dims != (mw, mh), None => true }
        };
        if rebuild {
            let masks: Vec<MaskBuffer> = layers.visible_layers()
                .map(|l| self.composite_mask(&l.mask, &input_view, mw, mh)).collect();
            *self.cache.borrow_mut() = Some(CachedMasks { layers: layers.clone(), full_dims: (mw, mh), masks });
        }
        let cache = self.cache.borrow();
        let cm = cache.as_ref().unwrap();

        let origin = *self.mask_origin.borrow();
        let mut current = input.clone();
        for (layer, mask) in layers.visible_layers().zip(cm.masks.iter()) {
            let mut u = local_adjust_uniform(&layer.adjustments);
            u.mask_origin = origin;
            current = self.apply(&current, mask, u);
        }
        current
    }
}

impl Node<PipelineImage> for Rc<LocalAdjustmentsNode> {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage { (**self).evaluate(inputs) }
}
```

Notes for the implementer:
- `PIPELINE_FORMAT` and `PipelineImage` are `pub(crate)` in `crate::image`.
- If `stroke_dabs` has a different signature than `stroke_dabs(&[BrushNode]) -> Vec<Dab>`, adapt the `Brush` arm — check `ferrolite-mask/src/stroke.rs`; the plan assumes the Plan-2 public export `stroke_dabs`. If it takes a `&Stroke` or spacing arg, thread it through.
- Range passes reference `color_view` which is the **current input** (post-Hsl) — matches §5.2.

- [ ] **Step 3: Add a headless-skip isolation test for the node.** Append to `local_golden.rs`:

```rust
#[test]
fn empty_mask_layer_applies_globally() {
    // An empty MaskDefinition = full mask → exposure applies everywhere; the
    // whole render should differ from the identity render.
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let src = common::gradient(W, H);
    let base = { let mut p = EditPipeline::new(ctx.clone(), &src, OpStack::default(), IDENTITY);
        p.render_to_image() };
    let la = LocalAdjustments { layers: vec![MaskLayer {
        name: "all".into(), visible: true, mask: MaskDefinition::default(),
        adjustments: AdjustmentSet { exposure: 1.0, ..Default::default() } }] };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));
    let mut p = EditPipeline::new(ctx, &src, stack, IDENTITY);
    let got = p.render_to_image();
    assert!(common::max_abs_diff(&got, &base) > 8, "empty mask should apply the adjustment globally");
}
```

- [ ] **Step 4: Declare the module.** In `lib.rs` add `mod local_node;` (no re-export needed — it's `pub(crate)`, used by `pipeline`/`tile_edit`).

- [ ] **Step 5: Build (compilation gate — golden run happens after Task 7 wiring).**

Run: `cargo build -p ferrolite-pipeline --tests`
Expected: PASS. (The golden tests need `EditPipeline` wiring — run them in Task 7/8.)

- [ ] **Step 6: Commit.**

```bash
git add ferrolite-pipeline/src/local_node.rs ferrolite-pipeline/src/lib.rs ferrolite-pipeline/tests/local_golden.rs
git commit -m "feat(pipeline): LocalAdjustmentsNode (composite mask -> Light+Color apply -> accumulate)"
```

---

## Task 6: Display→source inverse coordinate mapping

**Files:**
- Create: `ferrolite-pipeline/src/coord.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (`mod coord;` + re-export `display_to_source`)

**Interfaces:**
- Consumes: `crate::op::Geometry`, `crate::uniforms::geometry_uniform` (the `m`/`off`/`out_dims`/`src_dims` transform).
- Produces: `pub fn display_to_source(geo: Option<Geometry>, src_w: u32, src_h: u32, out_norm: (f32,f32)) -> (f32,f32)` — maps a point in **normalized output/crop space** ([0,1]² over the displayed, cropped+rotated image) to **normalized source space** ([0,1]² over the pre-geometry image), so the app can store mask params source-anchored. Lens is treated as identity (§5.2 fallback).

- [ ] **Step 1: Write the failing tests.** Create `ferrolite-pipeline/src/coord.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::{Aspect, CropRect, Geometry};

    fn approx(a: (f32, f32), b: (f32, f32)) {
        assert!((a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4, "{a:?} != {b:?}");
    }

    #[test]
    fn identity_geometry_is_the_identity_map() {
        approx(display_to_source(None, 100, 80, (0.25, 0.75)), (0.25, 0.75));
        approx(display_to_source(None, 100, 80, (0.0, 0.0)), (0.0, 0.0));
    }

    #[test]
    fn crop_maps_output_into_the_crop_window() {
        // Crop the centre half: output (0,0) → source (0.25,0.25); output (1,1) → (0.75,0.75).
        let geo = Geometry { crop: CropRect { x: 0.25, y: 0.25, w: 0.5, h: 0.5 },
            angle_deg: 0.0, aspect: Aspect::Free };
        approx(display_to_source(Some(geo), 100, 100, (0.0, 0.0)), (0.25, 0.25));
        approx(display_to_source(Some(geo), 100, 100, (1.0, 1.0)), (0.75, 0.75));
        approx(display_to_source(Some(geo), 100, 100, (0.5, 0.5)), (0.5, 0.5));
    }

    #[test]
    fn rotation_round_trips_through_the_center() {
        // The crop centre is invariant under rotation about it.
        let geo = Geometry { crop: CropRect::full(), angle_deg: 90.0, aspect: Aspect::Original };
        approx(display_to_source(Some(geo), 100, 100, (0.5, 0.5)), (0.5, 0.5));
    }
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p ferrolite-pipeline --lib coord::tests`
Expected: FAIL — `display_to_source` not defined.

- [ ] **Step 3: Implement.** Prepend to `coord.rs`:

```rust
//! Pure display→source inverse coordinate mapping. Mask shapes/strokes are stored
//! in normalized SOURCE coords (§5.2) so they stay anchored to content across
//! crop/rotate/aspect (all applied AFTER LocalAdjustments). The app inverse-maps a
//! display-space pointer to source coords through the active geometry; lens is
//! treated as identity here (the §5.2 fallback). No GPU — fully unit-tested.
//!
//! `geometry_uniform` already builds the output→source transform used by the GPU
//! resample: `src_px = m · out_px + off` (row-major 2×2 `m`). We reuse it: an output
//! point in [0,1] scales to output pixels, maps to source pixels, then normalizes by
//! source dims.

use crate::op::Geometry;
use crate::uniforms::geometry_uniform;

/// Map a normalized output/crop-space point (`out_norm` in [0,1]²) to normalized
/// source-space coords. `geo` is the active geometry op (None = identity).
pub fn display_to_source(
    geo: Option<Geometry>,
    src_w: u32,
    src_h: u32,
    out_norm: (f32, f32),
) -> (f32, f32) {
    let (u, out_w, out_h) = geometry_uniform(geo, src_w, src_h);
    let ox = out_norm.0 * out_w as f32;
    let oy = out_norm.1 * out_h as f32;
    // src_px = m · out_px + off  (m row-major [m00, m01, m10, m11]).
    let sx = u.m[0] * ox + u.m[1] * oy + u.off[0];
    let sy = u.m[2] * ox + u.m[3] * oy + u.off[1];
    (sx / u.src_dims[0], sy / u.src_dims[1])
}
```

- [ ] **Step 4: Declare + re-export.** In `lib.rs` add `mod coord;` and `pub use coord::display_to_source;`.

- [ ] **Step 5: Run tests.**

Run: `cargo test -p ferrolite-pipeline --lib coord::tests`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit.**

```bash
git add ferrolite-pipeline/src/coord.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): display->source inverse coord mapping through crop+rotate"
```

---

## Task 7: Wire `LocalAdjustmentsNode` into `EditPipeline` (preview tier)

**Files:**
- Modify: `ferrolite-pipeline/src/pipeline.rs` (insert node between `hsl` and `sharpen`; `set_stack` diff; `node_count`).

**Interfaces:**
- Consumes: `crate::local_node::LocalAdjustmentsNode`, `crate::local::LocalAdjustments`, `OpStack::local_adjustments`.
- Produces: an `EditPipeline` whose op chain is `… → Hsl → LocalAdjustments → Sharpen → Geometry`. `set_stack` updates the local layers + dirties the node only when they change.

- [ ] **Step 1: Write the failing invalidation test.** Add to `ferrolite-pipeline/tests/local_golden.rs`:

```rust
#[test]
fn local_adjust_edit_only_reevaluates_node_and_downstream() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), OpStack::default(), IDENTITY);
    let _ = pipe.evaluate();
    let before = pipe.eval_count();
    let la = LocalAdjustments { layers: vec![MaskLayer {
        name: "m".into(), visible: true, mask: MaskDefinition::default(),
        adjustments: AdjustmentSet { exposure: 0.5, ..Default::default() } }] };
    pipe.set_stack(OpStack::default().set_op(Op::LocalAdjustments(la)));
    let _ = pipe.evaluate();
    let delta = pipe.eval_count() - before;
    // Only LocalAdjustments + Sharpen + Geometry re-run (upstream cached).
    assert_eq!(delta, 3, "expected 3 downstream re-evals, got {delta}");
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p ferrolite-pipeline --test local_golden local_adjust_edit_only_reevaluates_node_and_downstream`
Expected: FAIL — `LocalAdjustments` node is not in the graph yet (delta will be 2, or the render won't reflect the op).

- [ ] **Step 3: Insert the node in `EditPipeline::new`.** In `pipeline.rs`:

Add imports:
```rust
use std::cell::RefCell;
use crate::local::LocalAdjustments;
use crate::local_node::LocalAdjustmentsNode;
```

Add struct fields (after `hsl_id`/`hsl`):
```rust
    local_adjust_id: NodeId,
    local_layers: Rc<RefCell<LocalAdjustments>>,
    local_node: Rc<LocalAdjustmentsNode>,
```

In `new`, after the `hsl_id` node and **before** the `sharpen` node, change `sharpen`'s input from `hsl_id` to the new node:
```rust
        let local_layers = Rc::new(RefCell::new(
            stack.local_adjustments().unwrap_or_default(),
        ));
        let local_node = Rc::new(LocalAdjustmentsNode::new(ctx.clone(), local_layers.clone()));
        let local_adjust_id = graph.add_node(Box::new(local_node.clone()), vec![hsl_id]);
```
Then change the sharpen node's inputs vec from `vec![hsl_id]` to `vec![local_adjust_id]`.

Add the three fields to the `Self { … }` initializer, and bump `node_count: 10` → `node_count: 11`.

- [ ] **Step 4: Handle it in `set_stack`.** In `EditPipeline::set_stack`, before `self.stack = stack;`, add:
```rust
        let la = stack.local_adjustments().unwrap_or_default();
        if *self.local_layers.borrow() != la {
            *self.local_layers.borrow_mut() = la;
            self.local_node.invalidate();
            self.graph.mark_dirty(self.local_adjust_id);
        }
```
(`invalidate` drops the composited-mask cache so the mask re-composites next evaluate.)

- [ ] **Step 5: Run the preview-tier tests.**

Run: `cargo test -p ferrolite-pipeline --test local_golden`
Expected: PASS — `radial_exposure_layer_matches_golden` (authors the golden on first run), `hidden_and_empty_layers_render_identical_to_source`, `empty_mask_layer_applies_globally`, `local_adjust_edit_only_reevaluates_node_and_downstream`.

Also confirm the existing goldens are byte-identical (identity LocalAdjustments must not change any prior render):

Run: `cargo test -p ferrolite-pipeline --test golden`
Expected: PASS — no drift (a stack with no `LocalAdjustments` op leaves the node in its `is_identity` → clone-input path).

- [ ] **Step 6: Commit** (include the authored golden PNG).

```bash
git add ferrolite-pipeline/src/pipeline.rs ferrolite-pipeline/tests/fixtures/local_radial_exposure.png
git commit -m "feat(pipeline): wire LocalAdjustmentsNode into EditPipeline (preview tier) + invalidation"
```

---

## Task 8: Full-stack golden — two-layer masked adjustment

**Files:**
- Modify: `ferrolite-pipeline/tests/local_golden.rs`

**Interfaces:** consumes the wired `EditPipeline` (Task 7). Produces the committed golden `two_layer_masked.png`.

- [ ] **Step 1: Write the golden test.** Add to `local_golden.rs`:

```rust
#[test]
fn two_layer_masked_adjustment_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // Layer 1: radial mask, +1 EV exposure. Layer 2: luma-range mask, warm temp.
    let radial = MaskLayer {
        name: "spot".into(), visible: true,
        mask: MaskDefinition { components: vec![(
            MaskComponent::RadialGradient {
                center: MVec2::new(0.35, 0.5), radius: MVec2::new(0.25, 0.25),
                rotation: 0.0, feather: 0.5, invert: false },
            CompositeMode::Add)], invert: false },
        adjustments: AdjustmentSet { exposure: 1.0, ..Default::default() },
    };
    let luma = MaskLayer {
        name: "brights".into(), visible: true,
        mask: MaskDefinition { components: vec![(
            MaskComponent::LumaRange { lo: 0.4, hi: 1.0, softness: 0.1 },
            CompositeMode::Add)], invert: false },
        adjustments: AdjustmentSet { temp: 0.6, ..Default::default() },
    };
    let la = LocalAdjustments { layers: vec![radial, luma] };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack, IDENTITY);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "two_layer_masked.png");
}
```

- [ ] **Step 2: Run (authors the golden on the dev GPU).**

Run: `cargo test -p ferrolite-pipeline --test local_golden two_layer_masked_adjustment_matches_golden`
Expected: PASS (writes `two_layer_masked.png` on first run; verify by eye it shows a brightened radial spot + warmed brights). Re-run to confirm it now diffs green.

- [ ] **Step 3: Commit** (include the golden PNG).

```bash
git add ferrolite-pipeline/tests/local_golden.rs ferrolite-pipeline/tests/fixtures/two_layer_masked.png
git commit -m "test(pipeline): full-stack two-layer masked-adjustment golden"
```

---

## Task 9: Wire into `TileEditPipeline` (full-res tier, pragmatic)

**Files:**
- Modify: `ferrolite-pipeline/src/tile_edit.rs`

**Interfaces:**
- Consumes: `LocalAdjustmentsNode`, `LocalAdjustments`, `OpStack::local_adjustments`, `edited_output_dims`.
- Produces: `TileEditPipeline` with `LocalAdjustments` between `hsl` and `sharpen`; `set_stack` re-derives layers; per-tile `set_mask_origin`.

**Design (read first):**
- Insert the node between `hsl_id` and `sharpen_id`, exactly as Task 7.
- Set `local_node.set_full_dims(edited_output_dims(&stack, src_w, src_h))` at construction, so the composited mask is built once at **full output resolution** (cached in the node) and each tile reads its sub-region.
- Per `produce_tile(coord)`, set `local_node.set_mask_origin([global_out_x, global_out_y])` where the global output origin is the tile's interior top-left in full-output pixels: `(coord.x * TILE_SIZE, coord.y * TILE_SIZE)` at LOD 0. (The tile chain's interior is `TILE_SIZE²` at the tile origin; the haloed extent is handled by the geometry head, but the color chain — including LocalAdjustments — runs over the haloed buffer. Set `mask_origin` to the **haloed** origin: `(coord.x*TILE_SIZE - halo, coord.y*TILE_SIZE - halo)`, matching how `extract_interior` offsets by `halo`.) Document this and cover it with the parity test.
- **Documented pragmatic limitation:** because the tile pipeline applies geometry at the head, the color chain (and thus LocalAdjustments) runs in **output space**. The full-output mask is composited in output-normalized coords, so under non-identity crop/rotate masks anchor to the cropped/rotated output frame rather than the source — the same accepted difference already noted for Sharpen in `tile_edit.rs`. For identity/translation geometry it is exact and matches the preview render. Add this to the module doc comment.
- **Memory note:** materializing the mask at full output resolution is a pragmatic P1 cost (a later optimization can stream/tile mask evaluation via a frame uniform). Only rebuilt when the layers change (`set_stack` → `invalidate`), never per tile.

- [ ] **Step 1: Write the failing parity test.** Add to `ferrolite-pipeline/tests/local_golden.rs`:

```rust
use ferrolite_pipeline::{GpuPyramidSource, TileEditPipeline};
use ferrolite_image::{TileCoord, TILE_SIZE};

#[test]
fn tile_masked_adjustment_matches_preview_region_identity_geometry() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // Source larger than one tile so the tile is a genuine sub-region.
    let sw = TILE_SIZE + 40; let sh = TILE_SIZE + 24;
    let src = common::gradient(sw, sh);
    let la = LocalAdjustments { layers: vec![MaskLayer {
        name: "lin".into(), visible: true,
        mask: MaskDefinition { components: vec![(
            MaskComponent::LinearGradient { start: MVec2::new(0.0, 0.0), end: MVec2::new(1.0, 0.0) },
            CompositeMode::Add)], invert: false },
        adjustments: AdjustmentSet { exposure: 0.8, ..Default::default() } }] };
    let stack = OpStack::default().set_op(Op::LocalAdjustments(la));

    // Whole-image reference.
    let mut preview = EditPipeline::new(ctx.clone(), &src, stack.clone(), IDENTITY);
    let whole = common::read_image_linear(&ctx, &preview.evaluate());

    // Tile (0,0), identity geometry → interior TILE_SIZE² must match the whole-image
    // top-left TILE_SIZE² region within tolerance.
    let pyramid = Arc::new(GpuPyramidSource::from_linear(&ctx, &src));
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
```

Check `GpuPyramidSource::from_linear` — use the actual constructor name in `gpu_pyramid.rs`; adapt the call if it differs (e.g. `GpuPyramidSource::new`).

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p ferrolite-pipeline --test local_golden tile_masked_adjustment_matches_preview_region_identity_geometry`
Expected: FAIL — the tile chain has no LocalAdjustments node, so the tile ignores the mask.

- [ ] **Step 3: Insert the node in `TileEditPipeline::new`.** In `tile_edit.rs`:

Add imports:
```rust
use std::cell::RefCell;
use crate::local::LocalAdjustments;
use crate::local_node::LocalAdjustmentsNode;
```

Add struct fields:
```rust
    local_adjust_id: NodeId,
    local_layers: Rc<RefCell<LocalAdjustments>>,
    local_node: Rc<LocalAdjustmentsNode>,
```

In `new`, after `hsl_id`, before `sharpen`:
```rust
        let local_layers = Rc::new(RefCell::new(stack.local_adjustments().unwrap_or_default()));
        let local_node = Rc::new(LocalAdjustmentsNode::new(ctx.clone(), local_layers.clone()));
        let (out_w, out_h) = crate::edited_output_dims(&stack, /* src_w */, /* src_h */);
        local_node.set_full_dims((out_w, out_h));
        let local_adjust_id = graph.add_node(Box::new(local_node.clone()), vec![hsl_id]);
```
Obtain `src_w`/`src_h` from `source.level_size(0)` (the pyramid's LOD-0 size) — the `GeometryHeadNode` already uses `self.source.level_size(lod)`; capture it before moving `source` into the head, e.g. `let (src_w, src_h) = source.level_size(0);` near the top of `new`.

Change the sharpen node input from `vec![hsl_id]` to `vec![local_adjust_id]`. Add the three fields to `Self { … }`.

- [ ] **Step 4: Set the per-tile mask origin + `set_stack` handling.**

In `produce_tile`, after `self.request.set(...)` and before `evaluate`:
```rust
        let gx = coord.x as i32 * ferrolite_image::TILE_SIZE as i32 - self.halo as i32;
        let gy = coord.y as i32 * ferrolite_image::TILE_SIZE as i32 - self.halo as i32;
        self.local_node.set_mask_origin([gx, gy]);
        self.graph.mark_dirty(self.local_adjust_id);
```
(The color chain runs over the haloed buffer of extent `haloed_tile_extent(halo)`; `mask_origin` shifts by `-halo` so `textureLoad(mask, origin + xy)` reads the correct full-output pixels, and `extract_interior`'s `halo` offset then lands on the interior. Confirm against the parity test.)

In `set_stack`, before `self.graph.mark_dirty(self.head_id);`:
```rust
        let la = stack.local_adjustments().unwrap_or_default();
        if *self.local_layers.borrow() != la {
            *self.local_layers.borrow_mut() = la;
            self.local_node.invalidate();
            self.graph.mark_dirty(self.local_adjust_id);
        }
```
Update the `set_stack` LIMITATION doc comment to note that a geometry change still requires a rebuild (unchanged), and that the local mask is composited at the full output resolution fixed at construction (a geometry/output-dims change → rebuild via `needs_full_rebuild`, same as today).

- [ ] **Step 5: Update the module doc comment** at the top of `tile_edit.rs` to record the output-space mask limitation (parallel to the existing Sharpen note) and the full-output mask materialization cost.

- [ ] **Step 6: Run the parity test + the existing tile-seam goldens.**

Run: `cargo test -p ferrolite-pipeline --test local_golden tile_masked_adjustment_matches_preview_region_identity_geometry`
Expected: PASS.

Run: `cargo test -p ferrolite-pipeline --test golden`
Expected: PASS — existing tile-seam/color goldens unchanged (identity LocalAdjustments = clone-input).

- [ ] **Step 7: Commit.**

```bash
git add ferrolite-pipeline/src/tile_edit.rs ferrolite-pipeline/tests/local_golden.rs
git commit -m "feat(pipeline): wire LocalAdjustments into TileEditPipeline (full-res, output-frame mask)"
```

---

## Task 10: `frl:ops` persistence round-trip + version tolerance

**Files:**
- Create: `ferrolite-pipeline/tests/local_persistence.rs`

**Interfaces:** consumes `serialize`/`deserialize` (pipeline), `ferrolite_catalog::xmp::{read_ops, write_ops}` (unchanged), the new model + `Op::LocalAdjustments`. Proves the sidecar path is transparent — **no `op.rs`/`serialize.rs`/`xmp.rs` code change is required** because the payload is the whole `OpStack` JSON and `Op` serializes by variant name.

- [ ] **Step 1: Confirm `ferrolite-catalog` is a dev-dependency.** In `ferrolite-pipeline/Cargo.toml` `[dev-dependencies]`, add if absent:
```toml
ferrolite-catalog = { workspace = true }
```

- [ ] **Step 2: Write the persistence tests.** Create `ferrolite-pipeline/tests/local_persistence.rs`:

```rust
//! `frl:ops` persistence for LocalAdjustments — pure/IO, runs on every OS in CI.

use ferrolite_mask::{CompositeMode, MaskComponent, MaskDefinition, MaskProvenance, RasterHandle, Vec2 as MVec2};
use ferrolite_pipeline::{
    deserialize, serialize, AdjustmentSet, ColorSwatch, LocalAdjustments, MaskLayer, Op, OpStack,
};

fn sample_stack() -> OpStack {
    let la = LocalAdjustments { layers: vec![
        MaskLayer { name: "sky".into(), visible: true,
            mask: MaskDefinition { components: vec![
                (MaskComponent::LinearGradient { start: MVec2::new(0.0, 0.0), end: MVec2::new(0.0, 1.0) }, CompositeMode::Add),
                (MaskComponent::Imported { handle: RasterHandle(7),
                    provenance: MaskProvenance { model_id: "sam2.1".into(), model_version: "1".into(), prompt: "click:0.5,0.5".into() } }, CompositeMode::Subtract),
            ], invert: false },
            adjustments: AdjustmentSet { exposure: -0.4, temp: 0.5,
                color: ColorSwatch { r: 0.1, g: 0.2, b: 0.9, amount: 0.3 }, ..Default::default() } },
        MaskLayer { name: "brush".into(), visible: false, mask: MaskDefinition::default(),
            adjustments: AdjustmentSet { contrast: 0.2, ..Default::default() } },
    ] };
    OpStack::default().set_op(Op::Exposure(ferrolite_pipeline::Exposure { ev: 0.25 }))
        .set_op(Op::LocalAdjustments(la))
}

#[test]
fn local_adjustments_round_trips_through_serialize() {
    let s = sample_stack();
    assert_eq!(deserialize(&serialize(&s)), Some(s));
}

#[test]
fn missing_local_adjustments_op_loads_as_none() {
    let json = r#"{"version":1,"ops":[{"Exposure":{"ev":0.5}}]}"#;
    let s = deserialize(json).unwrap();
    assert!(s.local_adjustments().is_none());
}

#[test]
fn adjustment_set_missing_fields_load_as_identity() {
    // A future/older payload with only some fields present.
    let json = r#"{"version":1,"ops":[{"LocalAdjustments":{"layers":[
        {"name":"m","visible":true,"mask":{"components":[],"invert":false},
         "adjustments":{"exposure":1.0}}]}}]}"#;
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
    ferrolite_catalog::xmp::write_ops(&p, &payload).unwrap();
    // Rating written after must not clobber frl:ops.
    ferrolite_catalog::xmp::write_rating(&p, ferrolite_catalog::Rating::new(4)).unwrap();
    let read = ferrolite_catalog::xmp::read_ops(&p).unwrap();
    assert_eq!(deserialize(&read), Some(sample_stack()));
    assert_eq!(ferrolite_catalog::xmp::read_rating(&p), Some(ferrolite_catalog::Rating::new(4)));
    let _ = std::fs::remove_dir_all(&dir);
}
```

Adjust `ferrolite_catalog::Rating` / `xmp::*` paths to the crate's actual public surface (check `ferrolite-catalog/src/lib.rs` exports; the functions used in `xmp.rs` tests are `read_ops`/`write_ops`/`read_rating`/`write_rating`/`Rating::new`). If `write_ops`/`read_ops` are not re-exported at the crate root, reference them via the module path they live in.

- [ ] **Step 3: Run.**

Run: `cargo test -p ferrolite-pipeline --test local_persistence`
Expected: PASS (4 tests). If the XMP test fails to resolve `ferrolite_catalog::xmp`, fix the import path (module may be `pub mod xmp` or re-exported functions) — the assertion logic stays.

- [ ] **Step 4: Commit.**

```bash
git add ferrolite-pipeline/Cargo.toml ferrolite-pipeline/tests/local_persistence.rs
git commit -m "test(pipeline): frl:ops round-trip for LocalAdjustments (version-tolerant, merge-preserving)"
```

---

## Task 11: Workspace gate + visual test plan handoff

**Files:** none (verification).

- [ ] **Step 1: Format.**

Run: `cargo fmt --all`
Then: `cargo fmt --all --check`
Expected: clean.

- [ ] **Step 2: Clippy (workspace, warnings as errors).**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean. Fix any lints in the new code (common ones here: `too_many_arguments` on shape wrappers — already `#[allow]`ed upstream; `needless_range_loop` in `light_color_apply` — rewrite with iterators if flagged).

- [ ] **Step 3: Full test suite.**

Run: `cargo test --workspace`
Expected: green (GPU goldens author-then-pass on the dev GPU; skip cleanly headless).

- [ ] **Step 4: Do NOT finish the branch.** This is 1 of 5 plans. Produce the visual test plan below and STOP — hold for the author's hands-on test before any merge/PR (CLAUDE.md).

**Visual test plan (hand to the author):**

This plan is **engine/pipeline-only — there is nothing to visually test in the running app yet.** No UI is wired (the Masking tool + panel is Plan 4), so no control, panel, or gesture in FerroLite reaches `LocalAdjustments` this phase. What changed is reachable only from tests:

- **Offline artifacts worth an optional glance:** the committed goldens
  `ferrolite-pipeline/tests/fixtures/local_radial_exposure.png` and
  `two_layer_masked.png` — open them and confirm the radial spot is brightened and
  (in the two-layer image) the brighter regions are visibly warmed. These were authored
  on the dev GPU (RTX 3060/3070-class); if they look wrong, the Light+Color math or mask
  compositing is off.
- **Where the real hands-on test lands:** Plan 4 (Develop Masking UI) — creating masks,
  dragging gradient/radial handles, painting brush strokes, the per-control resets, the
  colored overlay, and sub-frame slider response on the live preview. Test all of that then.

Confirm the gate is green, then report completion of Plan 3 and wait.

---

## Self-Review

**1. Spec coverage (§5–§7, §10, §12 plan 3):**
- `Op::LocalAdjustments(LocalAdjustments{ layers: Vec<MaskLayer> })` + `MaskLayer{name,visible,mask,adjustments}` — Task 1, 2. ✔
- `AdjustmentSet` = Light (exposure/contrast/highlights/shadows/whites/blacks) + Color (temp/tint/saturation/hue/color) + reserved neighborhood fields, no shader — Task 1, 3. ✔
- `OpKind::LocalAdjustments` after `Hsl`, discriminants renumbered, serde output unchanged (snapshot test) — Task 2. ✔
- Light+Color WGSL point-op + param→uniform units — Task 3, 4. ✔
- `LocalAdjustmentsNode` (composite → apply → accumulate) on the unchanged `Graph` — Task 5. ✔
- Op-order insertion after Hsl in both pipelines — Task 7 (preview), Task 9 (full-res). ✔
- Source-anchored masks + display→source inverse through crop+rotate + lens-identity fallback — Task 6. ✔
- `frl:ops` encode/decode, merge-preserving, version-tolerant → identity, read-on-open — Task 10 (transparent via serde + existing xmp). ✔
- Preview + full-res recompute + invalidation; painting-stays-preview-until-commit (full-res mask cache only rebuilt on layer change, not per pointer move); single version bump on commit (handled by OpStack replacement + `mark_dirty`); region-scoped optimization (mask cache + per-tile origin) with whole-version fallback (`invalidate`) — Tasks 7, 9. ✔
- Full-stack two-layer golden — Task 8; goldens auto-skip headless — all GPU tests use the `headless()` guard. ✔
- §10 error handling: empty/degenerate masks → identity (Task 5 empty→ones/zeroed; `stroke_dabs` on empty strokes yields no dabs → zeroed); Imported inert → zeroed; malformed sidecar → identity (existing xmp `.bak`, unchanged). ✔
- §13 decisions honored: one op holding `Vec<MaskLayer>`; stage after Hsl; executor unchanged; engine crate reused as nodes; AI seam serialized (round-trips in Task 10) with no producer. ✔

**2. Placeholder scan:** No "TBD"/"add error handling"/"similar to Task N". Every code step has complete code. Two explicit "verify the actual signature" notes (Task 5 `stroke_dabs`, Task 9 `GpuPyramidSource` ctor, Task 10 `ferrolite_catalog::xmp` path) are real integration checks against Plan-1/2/existing code, not placeholders — the assertion/logic is fully specified.

**3. Type consistency:** `LocalAdjustments`/`MaskLayer`/`AdjustmentSet`/`ColorSwatch`/`LightControl`/`ColorControl` names match across Tasks 1/2/3/5/7/9/10. `LocalAdjustUniform` fields (Task 3) mirror `local_adjust.wgsl struct P` (Task 4) exactly, including `mask_origin: vec2<i32>` and the trailing pad. `LocalAdjustmentsNode::{new,set_mask_origin,set_full_dims,invalidate}` consistent between definition (Task 5) and callers (Tasks 7, 9). `OpStack::local_adjustments()` used consistently. `display_to_source` signature stable (Task 6).

**Known scope boundary (per the approved decision):** full-res masks are composited in output space (exact for identity/translation geometry; output-frame-anchored under rotation — documented in `tile_edit.rs`, parallel to the existing Sharpen-in-output-space note), and materialized at full output resolution (a pragmatic P1 cost, rebuilt only on layer change). Source-anchored per-tile masking under arbitrary geometry (via a frame-aware shape extension) is a deliberate future optimization, not a Plan-3 deliverable.
