# P3 Plan 3 — Dehaze (Dark Channel Prior) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a bipolar global **Dehaze** op (Dark Channel Prior, He et al.) — negative amount adds haze — as a `ferrolite-pipeline` halo node inserted after `Contrast`/before `ToneCurve`, exposed through a new **Effects** develop tab with a per-control-reset slider.

**Architecture:** Dehaze is a neighbourhood (halo) op in the same class as `Sharpen`: a single WGSL compute pass over a haloed tile, driven by a `PointOpNode<DehazeUniform>` (reuses the existing point-op bind layout — no new GPU plumbing). The Dark Channel Prior needs a **whole-image atmospheric-light estimate `A`**; this is computed **once** on the decoded preview image (CPU, subsampled, bounded) and passed to every tile as a **uniform** — never estimated per-tile. The pixel recovery `J = (I − A)/max(t, t₀) + A` (and its symmetric add-haze branch) lives in a **pure reusable function** `dehaze_recover` (design §2.5) that both the WGSL kernel mirrors and the future per-mask path (§7) will reuse.

**Tech Stack:** Rust, `wgpu` + WGSL compute, `egui`/`eframe`, `bytemuck` Pod uniforms. No new dependencies (pure-Rust math — design §1/§2.3).

## Global Constraints

_Copied verbatim from the P3 design (`2026-07-08-p3-tone-and-color-grading-design.md`) and CLAUDE.md. Every task's requirements implicitly include this section._

- **Scope (do not expand):** one new `Dehaze { amount: [-1,1], radius }` bipolar op (negative = add haze); Dark Channel Prior — dark-channel min-filter (**user-exposed** patch radius = halo, plumbed like `Sharpen`), transmission map, recovery `J=(I-A)/max(t,t0)+A`, blend by amount. **Author-requested extension to spec §5.1** (which specified `Dehaze { amount }` only): the patch radius is exposed as a second control, exactly mirroring `Sharpen { amount, radius }` — the design already required the radius to be "plumbed like Sharpen," so this only surfaces it in the op + UI. Atmospheric light `A` is a **whole-image** estimate computed once on the preview-resolution image and passed to every tile as a **uniform (NOT per-tile)**. **No guided-filter refinement** (out of scope). **GLOBAL op only** (no per-mask/local variant — that is deferred to the §7 "P3-local" follow-up).
- **Op order (design §2.1):** insert `OpKind::Dehaze` **after `Contrast`, before `ToneCurve`**; renumber the tail. Final target order: `Exposure · WhiteBalance · Contrast · Dehaze · ToneCurve · Hsl · ColorGrade · LocalAdjustments · Sharpen · LensCorrection · Geometry`.
- **Serde-safe renumber (design §2.1):** `OpKind` is a sort key that is **never serialized** (`Op` serializes by variant name). Inserting a variant + renumbering is mechanical and requires no sidecar migration. **The `opkind_renumber_does_not_change_serde_output` guard test MUST be kept and extended.**
- **Back-compat (design §2.2):** all new state is additive; a sidecar written before this plan deserializes to today's exact behavior. `Op::Dehaze` is simply absent from older sidecars (→ no dehaze op → identity).
- **Contracts (design §2.3):** the GPU executor (`ferrolite-gpu`) is **not modified** — Dehaze is supplied as a `ferrolite-pipeline` node (contract 4). Dehaze is a **halo consumer** on the source-agnostic VT, exactly the `Sharpen` class (contract 5). `A`'s estimate runs on the already-decoded preview image, not per-frame on the UI thread (contract 1). No engine-tier edits, no copyleft, no new deps.
- **Reusable-math (design §2.5):** the core transform MUST be a **pure function** (`dehaze_recover`) in `ferrolite-pipeline`, independent of the node/shader wiring. No transform logic may live only inside a node's `apply`/shader-setup.
- **UI (CLAUDE.md + design §2.4):** the Dehaze slider MUST have a **per-control reset** (reuse the `EguiSlider` reset column — it is baked in). The new tab's icon MUST come from the `icons` module (a new Phosphor alias in `icons.rs`) — no raw glyphs, no hand-drawn `Painter` icons. Build GPU pipelines/shaders **once** and reuse (pre-warm at startup); never rebuild per image/edit.
- **Rust style:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean; immutable `OpStack` (`set_op`/`reset` return new stacks); `#[repr(C)]` uniform field order MIRRORS the WGSL `struct P` exactly.
- **Merge note:** this is the **LAST** of the three P3 plans; rebase onto whatever P3 branches already merged and re-number `OpKind` to the §2.1 target order.

**Branch:** `feat/p3-dehaze` off `main`.

**Workspace gate (run after every task; must stay green):**
```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## File Structure

**`ferrolite-pipeline` (photo tier):**
- Modify `src/op.rs` — add `Dehaze` struct, `Op::Dehaze` variant, `OpKind::Dehaze = 3` (renumber tail), `dehaze()` accessor, `is_identity`; extend guard/discriminant tests. *(Task 1)*
- Create `src/dehaze.rs` — the pure DCP math: constants, `dark_channel`, `estimate_atmospheric_light`, `dehaze_recover`, `dehaze_halo`, `DehazeUniform`, `dehaze_uniform`. *(Task 2)*
- Create `src/shaders/dehaze.wgsl` — the neighbourhood compute pass mirroring `dehaze_recover`. *(Task 3)*
- Modify `src/pipeline.rs` (`EditPipeline`) — insert the dehaze node; compute `A` from the CPU source; `set_stack`; `node_count`. *(Task 3)*
- Modify `src/tile_edit.rs` (`TileEditPipeline`) — insert the dehaze node; `set_dehaze_atmos` setter; fold `dehaze_halo` into the tile halo. *(Task 4)*
- Modify `src/lib.rs` — module decl + re-exports (`Dehaze`, `dehaze_recover`, `estimate_atmospheric_light`, `dehaze_halo`, `DehazeUniform`); add `dehaze` to `prewarm_shaders`. *(Tasks 1–4)*
- Modify `tests/golden.rs` — whole-image dehaze golden + tiled-vs-whole parity. *(Tasks 3–4)*

**`ferrolite-export` (photo tier):**
- Modify `src/render.rs` — `render_tiled` gains an `atmospheric_light: [f32; 3]` param, set on its `TileEditPipeline`. *(Task 5)*
- Modify `src/job.rs` — pass `A` through (computed from the decoded source). *(Task 5)*
- Modify `tests/render_golden.rs` — pass the neutral `A` at existing call sites. *(Task 5)*

**`ferrolite-app` (photo tier, GPL binary):**
- Modify `src/develop/ops_edit.rs` — `set_dehaze`; extend `needs_full_rebuild`. *(Task 6)*
- Modify `src/icons.rs` — add the `EFFECTS` alias + test entry. *(Task 6)*
- Modify `src/develop/base_tabs.rs` — new `EffectsTab: PanelTab`; register in `base_tabs()`. *(Task 7)*
- Modify `src/viewer/edit_producer.rs` — `EditTileProducer::set_dehaze_atmos` delegate. *(Task 8)*
- Modify `src/app.rs` — compute `A` from the preview source; call `set_dehaze_atmos` on every producer/export path; thread `A` to export. *(Task 8)*
- Modify `src/export/{mod.rs,batch.rs}` — compute + pass `A` into `render_tiled`. *(Task 8)*

---

## Task 1: Op model — `Dehaze` op + `OpKind` insert/renumber

**Files:**
- Modify: `ferrolite-pipeline/src/op.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (export `Dehaze`)

**Interfaces:**
- Produces: `pub struct Dehaze { pub amount: f32, pub radius: u32 }` (`Clone, Copy, PartialEq, Debug, Serialize, Deserialize`); `Op::Dehaze(Dehaze)`; `OpKind::Dehaze = 3` (and renumbered `ToneCurve=4 … Geometry=10`); `OpStack::dehaze(&self) -> Option<Dehaze>`; `Dehaze::is_identity(&self) -> bool` (`amount == 0.0` — a radius alone has no effect). Mirrors `Sharpen { amount, radius }`.

- [ ] **Step 1: Write the failing tests**

Add these tests to the `#[cfg(test)] mod tests` block in `ferrolite-pipeline/src/op.rs`:

```rust
    #[test]
    fn dehaze_default_and_identity() {
        // A radius alone (amount 0) has no render effect → identity.
        assert!(Dehaze { amount: 0.0, radius: 8 }.is_identity());
        assert!(!Dehaze { amount: 0.5, radius: 8 }.is_identity());
        assert!(!Dehaze { amount: -0.5, radius: 8 }.is_identity());
    }

    #[test]
    fn dehaze_sorts_between_contrast_and_tone_curve() {
        let s = OpStack::default()
            .set_op(Op::ToneCurve(ToneCurve::default()))
            .set_op(Op::Dehaze(Dehaze { amount: 0.4, radius: 8 }))
            .set_op(Op::Contrast(Contrast { amount: 0.1 }));
        let kinds: Vec<OpKind> = s.ops.iter().map(|o| o.kind()).collect();
        assert_eq!(
            kinds,
            vec![OpKind::Contrast, OpKind::Dehaze, OpKind::ToneCurve]
        );
        assert_eq!(s.dehaze(), Some(Dehaze { amount: 0.4, radius: 8 }));
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
    fn dehaze_roundtrips_and_renumber_is_serde_stable() {
        // OpKind is a sort key, never serialized; Op serializes by variant name,
        // so inserting Dehaze must not perturb the JSON of other ops.
        let s = OpStack::default()
            .set_op(Op::Exposure(Exposure { ev: 0.5 }))
            .set_op(Op::Dehaze(Dehaze { amount: -0.25, radius: 8 }))
            .set_op(Op::Sharpen(Sharpen {
                amount: 0.6,
                radius: 3,
            }));
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(
            json,
            r#"{"version":1,"ops":[{"Exposure":{"ev":0.5}},{"Dehaze":{"amount":-0.25,"radius":8}},{"Sharpen":{"amount":0.6,"radius":3}}]}"#
        );
        assert_eq!(serde_json::from_str::<OpStack>(&json).unwrap(), s);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline --lib op::`
Expected: FAIL — `Dehaze` unresolved, `OpKind::Dehaze` unknown, `dehaze()` missing.

- [ ] **Step 3: Add the `Dehaze` struct**

In `ferrolite-pipeline/src/op.rs`, immediately after the `Contrast` struct (around line 31), add:

```rust
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
```

- [ ] **Step 4: Add the `Op::Dehaze` variant + `kind()` arm**

In the `Op` enum (around line 295), add `Dehaze(Dehaze),` immediately after `Contrast(Contrast),`:

```rust
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
```

In `Op::kind()` (around line 326), add the arm after the `Contrast` arm:

```rust
            Op::Contrast(_) => OpKind::Contrast,
            Op::Dehaze(_) => OpKind::Dehaze,
            Op::ToneCurve(_) => OpKind::ToneCurve,
```

- [ ] **Step 5: Insert + renumber `OpKind`**

Replace the `OpKind` enum body (around line 312) with the renumbered order:

```rust
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
```

- [ ] **Step 6: Add the `dehaze()` accessor**

In `impl OpStack`, after the `contrast()` accessor (around line 409), add:

```rust
    pub fn dehaze(&self) -> Option<Dehaze> {
        self.ops.iter().find_map(|o| match o {
            Op::Dehaze(d) => Some(*d),
            _ => None,
        })
    }
```

- [ ] **Step 7: Export `Dehaze` from the crate**

In `ferrolite-pipeline/src/lib.rs`, add `Dehaze` to the `pub use op::{...}` list (keep alphabetical grouping with the other op structs):

```rust
pub use op::{
    Aspect, ColorGrade, Contrast, Correction, CropRect, CurveMode, Dehaze, Exposure, Geometry,
    GradeWheel, Hsl, HslBand, LensCorrection, Op, OpKind, OpStack, ParametricCurve, PointCurve,
    Sharpen, ToneCurve, WhiteBalance, STACK_VERSION,
};
```

- [ ] **Step 8: Update the existing `opkind_discriminants_after_colorgrade_insert` test**

That test (around line 790) still asserts the pre-Dehaze discriminants (`Hsl == 4` … `Geometry == 9`) and will now fail. Delete it — it is fully superseded by the new `opkind_discriminants_after_dehaze_insert` test from Step 1 (which asserts the post-Dehaze order). The `opkind_renumber_does_not_change_serde_output` test (around line 941) is unaffected (its stack has no Dehaze) — keep it as-is; it is the load-bearing guard.

- [ ] **Step 9: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --lib op::`
Expected: PASS (all op tests, including the retained `opkind_renumber_does_not_change_serde_output` guard).

- [ ] **Step 10: Run the workspace gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS. (The `ferrolite-app`/`ferrolite-export` crates still compile — `Op`/`OpKind` matches elsewhere are exhaustive only within `op.rs`; the new variant is additive to the public enum. If any downstream `match` over `Op`/`OpKind` fails to compile, that is surfaced here and fixed in the task that owns that file — but as of this plan no downstream code matches all `Op` variants.)

- [ ] **Step 11: Commit**

```bash
git add ferrolite-pipeline/src/op.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): add Dehaze op + OpKind insert after Contrast (serde-safe renumber)"
```

---

## Task 2: Pure DCP math — `dehaze.rs`

**Files:**
- Create: `ferrolite-pipeline/src/dehaze.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (module decl + re-exports)

**Interfaces:**
- Consumes: `crate::op::Dehaze`; `ferrolite_image::LinearRgbaF32` (fields `width: u32`, `height: u32`, `pixels: Vec<f32>` — RGBA, 4 floats/pixel, display-linear).
- Produces:
  - Constants `DEHAZE_DEFAULT_RADIUS: u32 = 8`, `MAX_DEHAZE_RADIUS: u32 = 64`, `DEHAZE_OMEGA: f32 = 0.95`, `DEHAZE_T0: f32 = 0.1`, `DEHAZE_ATMOS_NEUTRAL: [f32; 3] = [1.0, 1.0, 1.0]`, `DEHAZE_ATMOS_MIN: f32 = 1e-3`, `MAX_ATMOS_SAMPLES: usize = 262_144`.
  - `pub fn dark_channel(rgb: [f32; 3]) -> f32`
  - `pub fn estimate_atmospheric_light(img: &ferrolite_image::LinearRgbaF32) -> [f32; 3]`
  - `pub fn dehaze_recover(px: [f32; 3], dark: f32, a: [f32; 3], amount: f32) -> [f32; 3]`
  - `pub fn dehaze_halo(op: Option<crate::op::Dehaze>) -> u32`
  - `#[repr(C)] pub struct DehazeUniform { amount: f32, radius: i32, omega: f32, t0: f32, atmos: [f32; 4] }` (`Pod, Zeroable, PartialEq, Debug, Clone, Copy`)
  - `pub fn dehaze_uniform(op: Option<crate::op::Dehaze>, atmos: [f32; 3]) -> DehazeUniform`

- [ ] **Step 1: Write the failing tests (as the new module's test block)**

Create `ferrolite-pipeline/src/dehaze.rs` with ONLY the doc comment + tests first (implementation added in Step 3), so the RED step compiles-to-fail on missing items:

```rust
//! Pure Dark Channel Prior (He et al.) dehaze math — no GPU. The GPU pass
//! (`shaders/dehaze.wgsl`) mirrors `dehaze_recover` exactly; the atmospheric
//! light `A` is a whole-image estimate computed once (design §5.3) and handed to
//! every tile as a uniform. `dehaze_recover` is the reusable transform the future
//! per-mask path (design §7) will call unchanged (design §2.5).

use crate::op::Dehaze;
use ferrolite_image::LinearRgbaF32;

/// Default dark-channel min-filter patch radius (px), seeded for a brand-new op
/// by the Effects tab. The radius is USER-EXPOSED (`Dehaze::radius`); this is only
/// the initial value. Design §5.2 suggests 7–15.
pub const DEHAZE_DEFAULT_RADIUS: u32 = 8;
/// Safety cap on the dehaze patch radius (px): bounds the min-filter loop and
/// prevents a u32→i32 wrap to negative (mirrors `MAX_SHARPEN_RADIUS`).
pub const MAX_DEHAZE_RADIUS: u32 = 64;
/// Haze-retention factor ω (design §5.2, step 3): keep a little haze for realism.
pub const DEHAZE_OMEGA: f32 = 0.95;
/// Transmission floor t₀ (design §5.2, step 4): avoids divide-by-~0 noise blow-up.
pub const DEHAZE_T0: f32 = 0.1;
/// The identity-safe atmospheric light used before a real estimate is available
/// (e.g. `TileEditPipeline` before `set_dehaze_atmos`, or a no-dehaze export).
/// With `amount == 0` the recovery is identity regardless of `A`, so this is only
/// ever a placeholder for the no-op case.
pub const DEHAZE_ATMOS_NEUTRAL: [f32; 3] = [1.0, 1.0, 1.0];
/// Floor each `A` channel to this to keep the `I/A` and `/max(t,t0)` divisions finite.
pub const DEHAZE_ATMOS_MIN: f32 = 1e-3;
/// Cap on pixels scanned by `estimate_atmospheric_light` (it subsamples above
/// this). Bounds the CPU cost to sub-millisecond regardless of image size so it
/// is safe to run at pipeline construction (CLAUDE.md rule 1 — no multi-ms UI work).
pub const MAX_ATMOS_SAMPLES: usize = 262_144;

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: u32, h: u32, rgb: [f32; 3]) -> LinearRgbaF32 {
        let mut px = Vec::with_capacity((w * h) as usize * 4);
        for _ in 0..(w * h) {
            px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
        }
        LinearRgbaF32::new(w, h, px).unwrap()
    }

    #[test]
    fn dark_channel_is_min_of_rgb() {
        assert_eq!(dark_channel([0.2, 0.5, 0.9]), 0.2);
        assert_eq!(dark_channel([0.7, 0.3, 0.6]), 0.3);
    }

    #[test]
    fn recover_is_identity_at_zero_amount() {
        let px = [0.4, 0.5, 0.6];
        let out = dehaze_recover(px, 0.5, [0.9, 0.9, 0.9], 0.0);
        for c in 0..3 {
            assert!((out[c] - px[c]).abs() < 1e-6, "amount 0 must be identity");
        }
    }

    #[test]
    fn positive_amount_pushes_away_from_atmosphere() {
        // A hazy mid-grey pixel under a bright atmosphere: removing haze must move
        // it AWAY from A (darker here, since px < A) — i.e. increased contrast.
        let px = [0.6, 0.6, 0.6];
        let a = [0.9, 0.9, 0.9];
        let dark = 0.6; // normalized dark channel (I/A) ~ 0.6/0.9
        let out = dehaze_recover(px, dark, a, 1.0);
        assert!(out[0] < px[0], "haze removal moves a below-A pixel down: {out:?}");
    }

    #[test]
    fn negative_amount_pulls_toward_atmosphere() {
        // Adding haze pulls the pixel TOWARD A (lower contrast).
        let px = [0.3, 0.3, 0.3];
        let a = [0.9, 0.9, 0.9];
        let out = dehaze_recover(px, 0.6, a, -1.0);
        assert!(out[0] > px[0], "adding haze lifts a below-A pixel toward A: {out:?}");
        assert!(out[0] <= a[0] + 1e-6);
    }

    #[test]
    fn recover_roundtrips_toward_identity_near_zero() {
        // Small +/- amounts straddle the input (monotone in amount at fixed dark/A).
        let px = [0.5, 0.4, 0.55];
        let a = [0.85, 0.85, 0.85];
        let up = dehaze_recover(px, 0.5, a, 0.2);
        let down = dehaze_recover(px, 0.5, a, -0.2);
        // +amount (remove) moves away from A; -amount (add) moves toward A.
        assert!(up[0] < px[0] && down[0] > px[0]);
    }

    #[test]
    fn estimate_atmosphere_picks_the_bright_hazy_region() {
        // A dark scene (low dark-channel) with a bright hazy sky patch: A should
        // track the bright patch, not the dark foreground.
        let mut img = flat(64, 64, [0.05, 0.05, 0.06]);
        // Top 8 rows = bright haze.
        for y in 0..8u32 {
            for x in 0..64u32 {
                let i = ((y * 64 + x) * 4) as usize;
                img.pixels[i] = 0.9;
                img.pixels[i + 1] = 0.92;
                img.pixels[i + 2] = 0.95;
            }
        }
        let a = estimate_atmospheric_light(&img);
        assert!(a[0] > 0.7 && a[1] > 0.7 && a[2] > 0.7, "A tracks the bright haze: {a:?}");
    }

    #[test]
    fn estimate_atmosphere_is_floored_not_zero() {
        let a = estimate_atmospheric_light(&flat(8, 8, [0.0, 0.0, 0.0]));
        assert!(a.iter().all(|&c| c >= DEHAZE_ATMOS_MIN), "A is floored: {a:?}");
    }

    #[test]
    fn dehaze_halo_is_op_radius_or_zero() {
        assert_eq!(dehaze_halo(None), 0);
        // amount 0 contributes no halo even with a radius set.
        assert_eq!(dehaze_halo(Some(Dehaze { amount: 0.0, radius: 10 })), 0);
        assert_eq!(dehaze_halo(Some(Dehaze { amount: 0.5, radius: 10 })), 10);
        assert_eq!(dehaze_halo(Some(Dehaze { amount: -0.5, radius: 6 })), 6);
        // Clamped to MAX_DEHAZE_RADIUS (no u32→i32 wrap).
        assert_eq!(
            dehaze_halo(Some(Dehaze { amount: 0.5, radius: u32::MAX })),
            MAX_DEHAZE_RADIUS
        );
    }

    #[test]
    fn dehaze_uniform_identity_and_layout() {
        let u = dehaze_uniform(None, DEHAZE_ATMOS_NEUTRAL);
        assert_eq!(u.amount, 0.0);
        assert_eq!(u.radius, 0);
        // 32 bytes, 16-aligned (mirrors the WGSL `struct P`).
        assert_eq!(std::mem::size_of::<DehazeUniform>(), 32);
        assert_eq!(std::mem::size_of::<DehazeUniform>() % 16, 0);
        // Present op carries its OWN radius (clamped) + floored atmosphere.
        let u2 = dehaze_uniform(Some(Dehaze { amount: 0.5, radius: 12 }), [0.0, 0.5, 1.0]);
        assert_eq!(u2.radius, 12);
        assert!(u2.atmos[0] >= DEHAZE_ATMOS_MIN);
        assert_eq!(u2.atmos[1], 0.5);
        let u3 = dehaze_uniform(Some(Dehaze { amount: 0.5, radius: u32::MAX }), DEHAZE_ATMOS_NEUTRAL);
        assert_eq!(u3.radius, MAX_DEHAZE_RADIUS as i32);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline --lib dehaze::`
Expected: FAIL — `dark_channel`/`estimate_atmospheric_light`/`dehaze_recover`/`dehaze_halo`/`dehaze_uniform`/`DehazeUniform` not found. (Module must first be declared — do Step 4's `mod dehaze;` line if the module isn't picked up; the test command needs the module compiled.)

Note: to make the RED compile far enough to *run* and fail on assertions vs. missing symbols, you may add `mod dehaze;` to `lib.rs` now (Step 4). Either way the RED state must show these tests failing, not passing.

- [ ] **Step 3: Write the implementation**

Insert the implementation into `ferrolite-pipeline/src/dehaze.rs` **above** the `#[cfg(test)]` block (after the constants):

```rust
/// Per-pixel dark channel: the min of the three linear channels.
pub fn dark_channel(rgb: [f32; 3]) -> f32 {
    rgb[0].min(rgb[1]).min(rgb[2])
}

/// Whole-image atmospheric-light estimate `A` (design §5.3): the mean RGB of the
/// brightest ~0.1% of pixels by per-pixel dark channel. Subsamples to at most
/// `MAX_ATMOS_SAMPLES` pixels so the cost is bounded (safe at construction, off
/// the per-frame path — CLAUDE.md rule 1). Each channel is floored to
/// `DEHAZE_ATMOS_MIN` so downstream divisions stay finite. Deterministic (fixed
/// stride), so the preview and tiled tiers computing it from the same image agree.
pub fn estimate_atmospheric_light(img: &LinearRgbaF32) -> [f32; 3] {
    let n = (img.width as usize) * (img.height as usize);
    if n == 0 {
        return DEHAZE_ATMOS_NEUTRAL;
    }
    let stride = (n / MAX_ATMOS_SAMPLES).max(1);
    // (dark_channel, [r,g,b]) for each sampled pixel.
    let mut samples: Vec<(f32, [f32; 3])> = Vec::new();
    let mut i = 0usize;
    while i < n {
        let base = i * 4;
        let rgb = [img.pixels[base], img.pixels[base + 1], img.pixels[base + 2]];
        samples.push((dark_channel(rgb), rgb));
        i += stride;
    }
    // Brightest 0.1% by dark channel (at least one).
    let keep = (samples.len() / 1000).max(1);
    samples.sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut acc = [0.0f32; 3];
    for (_, rgb) in samples.iter().take(keep) {
        for c in 0..3 {
            acc[c] += rgb[c];
        }
    }
    let inv = 1.0 / keep as f32;
    [
        (acc[0] * inv).max(DEHAZE_ATMOS_MIN),
        (acc[1] * inv).max(DEHAZE_ATMOS_MIN),
        (acc[2] * inv).max(DEHAZE_ATMOS_MIN),
    ]
}

/// Per-pixel DCP recovery (design §5.2) — the reusable transform (design §2.5)
/// the WGSL kernel mirrors exactly. `dark` is the patch dark channel of the
/// NORMALIZED image `I/A` in `[0,1]` (computed by the caller/shader over the halo
/// patch). Transmission `t = 1 - ω·dark`, floored at `t0` for recovery:
///   remove-haze  J_c = (I_c - A_c)/max(t, t0) + A_c
///   add-haze  hazed_c = A_c + (I_c - A_c)·t          (symmetric, toward A)
/// `amount >= 0` blends I→J by `amount`; `amount < 0` blends I→hazed by `|amount|`.
/// Not clamped (out-of-range values pass through; display clamps later).
pub fn dehaze_recover(px: [f32; 3], dark: f32, a: [f32; 3], amount: f32) -> [f32; 3] {
    let t = (1.0 - DEHAZE_OMEGA * dark).clamp(0.0, 1.0);
    let te = t.max(DEHAZE_T0);
    let mut out = [0.0f32; 3];
    for c in 0..3 {
        let j = (px[c] - a[c]) / te + a[c];
        let hazed = a[c] + (px[c] - a[c]) * t;
        out[c] = if amount >= 0.0 {
            px[c] + amount * (j - px[c])
        } else {
            px[c] + (-amount) * (hazed - px[c])
        };
    }
    out
}

/// Halo (px) a tiled full-res dehaze pass must over-fetch: the op's patch radius
/// (clamped) when active, else 0 (mirrors `sharpen_halo`). Consumed by the tile
/// producer; a radius change therefore triggers `needs_full_rebuild`, an
/// amount-only change does not.
pub fn dehaze_halo(op: Option<Dehaze>) -> u32 {
    match op {
        Some(d) if d.amount != 0.0 => d.radius.min(MAX_DEHAZE_RADIUS),
        _ => 0,
    }
}

/// GPU uniform for `dehaze.wgsl`. `#[repr(C)]`, 16-byte aligned; field order +
/// padding MIRROR the WGSL `struct P` exactly. `atmos` is `[r, g, b, pad]`.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DehazeUniform {
    pub amount: f32,
    pub radius: i32,
    pub omega: f32,
    pub t0: f32,
    pub atmos: [f32; 4],
}

/// Build the dehaze uniform from the op + the whole-image atmospheric light.
/// Absent/identity op → `amount 0`, `radius 0` (the shader takes its passthrough
/// branch). `atmos` is floored so the shader's `I/A` division is finite.
pub fn dehaze_uniform(op: Option<Dehaze>, atmos: [f32; 3]) -> DehazeUniform {
    let (amount, r) = op.map(|d| (d.amount, d.radius)).unwrap_or((0.0, 0));
    // A no-op amount contributes no radius (shader passthrough); otherwise clamp.
    let radius = if amount != 0.0 {
        r.min(MAX_DEHAZE_RADIUS) as i32
    } else {
        0
    };
    DehazeUniform {
        amount,
        radius,
        omega: DEHAZE_OMEGA,
        t0: DEHAZE_T0,
        atmos: [
            atmos[0].max(DEHAZE_ATMOS_MIN),
            atmos[1].max(DEHAZE_ATMOS_MIN),
            atmos[2].max(DEHAZE_ATMOS_MIN),
            0.0,
        ],
    }
}
```

- [ ] **Step 4: Declare + export the module**

In `ferrolite-pipeline/src/lib.rs`, add `mod dehaze;` in the module list (alphabetically, after `mod coord;`/before `mod gpu_pyramid;` — i.e. after `mod coord;`):

```rust
mod coord;
mod dehaze;
mod gpu_pyramid;
```

Add the re-export (a new `pub use` line after the `pub use coord::...` line):

```rust
pub use dehaze::{
    dehaze_halo, dehaze_recover, estimate_atmospheric_light, DehazeUniform, DEHAZE_ATMOS_NEUTRAL,
    DEHAZE_DEFAULT_RADIUS, MAX_DEHAZE_RADIUS,
};
```

Note: `dehaze_uniform`, `dark_channel`, and the ω/t₀/floor constants stay crate-internal (used by `pipeline.rs`/`tile_edit.rs`); only the pure reusable transform + the halo + the uniform type + the constants the app/export need (`DEHAZE_ATMOS_NEUTRAL` for export, `DEHAZE_DEFAULT_RADIUS`/`MAX_DEHAZE_RADIUS` for the Effects tab slider) are `pub` (design §2.5).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --lib dehaze::`
Expected: PASS (all `dehaze::tests`).

- [ ] **Step 6: Run the workspace gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-pipeline/src/dehaze.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): pure Dark Channel Prior dehaze math (dehaze_recover, atmospheric light, uniform)"
```

---

## Task 3: `dehaze.wgsl` + wire the node into `EditPipeline` (whole-image preview)

**Files:**
- Create: `ferrolite-pipeline/src/shaders/dehaze.wgsl`
- Modify: `ferrolite-pipeline/src/pipeline.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (`prewarm_shaders`)
- Modify: `ferrolite-pipeline/tests/golden.rs` (whole-image dehaze golden)

**Interfaces:**
- Consumes: `crate::dehaze::{dehaze_uniform, estimate_atmospheric_light, DehazeUniform}`; the existing `PointOpNode<U>` (bind layout 0=src,1=dst,2=uniform); `crate::uniforms::*`.
- Produces: `EditPipeline` gains a `dehaze` node between `contrast_id` and `tone_curve_id`; internal fields `dehaze_id: NodeId`, `dehaze: Rc<Cell<DehazeUniform>>`, `dehaze_atmos: [f32; 3]`; `node_count` becomes `13`. `set_stack` updates the dehaze uniform (preserving `dehaze_atmos`).

- [ ] **Step 1: Write the WGSL pass**

Create `ferrolite-pipeline/src/shaders/dehaze.wgsl` (mirrors `dehaze_recover`; reuses the point-op bind layout, like `sharpen.wgsl`):

```wgsl
// Dehaze (Dark Channel Prior, He et al.). Neighbourhood op: the dark channel is a
// local min over a `radius` patch of the NORMALIZED image min(rgb / A). Mirrors
// `dehaze::dehaze_recover` exactly. A (atmospheric light) is a whole-image
// constant supplied as a uniform (design §5.3), NOT estimated per tile.
// Reuses the point-op bind layout (0 = src, 1 = dst, 2 = uniform).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P { amount: f32, radius: i32, omega: f32, t0: f32, atmos: vec4<f32> };
@group(0) @binding(2) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(src));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);

    if (p.amount == 0.0 || p.radius <= 0) {
        textureStore(dst, xy, c);
        return;
    }

    let a = p.atmos.rgb;
    // Local dark channel of the normalized image I/A over the patch.
    var dark = 1.0;
    for (var dy = -p.radius; dy <= p.radius; dy = dy + 1) {
        for (var dx = -p.radius; dx <= p.radius; dx = dx + 1) {
            let q = clamp(xy + vec2<i32>(dx, dy), vec2<i32>(0, 0), dims - vec2<i32>(1, 1));
            let n = textureLoad(src, q, 0).rgb / a;
            let m = min(n.r, min(n.g, n.b));
            dark = min(dark, m);
        }
    }

    let t = clamp(1.0 - p.omega * dark, 0.0, 1.0);
    let te = max(t, p.t0);
    let j = (c.rgb - a) / te + a;          // remove-haze
    let hazed = a + (c.rgb - a) * t;       // add-haze (toward A)
    var out = c.rgb;
    if (p.amount >= 0.0) {
        out = c.rgb + p.amount * (j - c.rgb);
    } else {
        out = c.rgb + (-p.amount) * (hazed - c.rgb);
    }
    textureStore(dst, xy, vec4<f32>(out, c.a));
}
```

- [ ] **Step 2: Wire the node into `EditPipeline::new`**

In `ferrolite-pipeline/src/pipeline.rs`:

Add the import (extend the `use crate::dehaze::...` — create the line if absent, near the other `use crate::...`):

```rust
use crate::dehaze::{dehaze_uniform, estimate_atmospheric_light, DehazeUniform};
```

Add fields to the `EditPipeline` struct (after the `contrast` field group, before `tone_curve_id`):

```rust
    dehaze_id: NodeId,
    dehaze: Rc<Cell<DehazeUniform>>,
    /// Whole-image atmospheric light, estimated once from the CPU source at
    /// construction (design §5.3) and reused by every `set_stack` (it is an image
    /// property, independent of the edit stack).
    dehaze_atmos: [f32; 3],
```

In `EditPipeline::new`, immediately after the `contrast` node block (after `let contrast_id = graph.add_node(..., vec![wb_id]);`), add:

```rust
        let dehaze_atmos = estimate_atmospheric_light(source);
        let dehaze = Rc::new(Cell::new(dehaze_uniform(stack.dehaze(), dehaze_atmos)));
        let dehaze_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/dehaze.wgsl"),
            "dehaze",
            dehaze.clone(),
        );
        let dehaze_id = graph.add_node(Box::new(dehaze_node), vec![contrast_id]);
```

Change the tone-curve node's input from `contrast_id` to `dehaze_id`:

```rust
        let tone_curve_id = graph.add_node(Box::new(tone_curve_node), vec![dehaze_id]);
```

In the `Self { ... }` initializer, add `dehaze_id,`, `dehaze,`, `dehaze_atmos,` (place them next to the `contrast*` entries), and bump `node_count`:

```rust
            node_count: 13,
```

- [ ] **Step 3: Update `EditPipeline::set_stack`**

In `set_stack`, after the `contrast` block and before the `tone_curve` (`luts`) block, add:

```rust
        let d = dehaze_uniform(stack.dehaze(), self.dehaze_atmos);
        if d != self.dehaze.get() {
            self.dehaze.set(d);
            self.graph.mark_dirty(self.dehaze_id);
        }
```

- [ ] **Step 4: Add `dehaze` to `prewarm_shaders`**

In `ferrolite-pipeline/src/lib.rs` `prewarm_shaders`, add the entry after `("contrast", ...)`:

```rust
        ("contrast", include_str!("shaders/contrast.wgsl")),
        ("dehaze", include_str!("shaders/dehaze.wgsl")),
        ("tone-curve", include_str!("shaders/tone_curve.wgsl")),
```

Update the `prewarm_shaders` doc comment count ("Eleven passes" → "Twelve passes", and mention dehaze).

- [ ] **Step 5: Write the whole-image golden test**

Add to `ferrolite-pipeline/tests/golden.rs` (follow the existing pattern in that file — reuse its `ctx`/`common::gradient` helpers; check the top of the file for the exact helper names and the headless-skip guard used by neighboring tests, and match them):

```rust
#[test]
fn dehaze_positive_increases_contrast_on_hazy_image() {
    let Some(ctx) = ferrolite_gpu::GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = std::sync::Arc::new(ctx);
    // A low-contrast "hazy" gradient: values compressed toward a bright floor.
    let (w, h) = (64u32, 64u32);
    let mut px = Vec::with_capacity((w * h) as usize * 4);
    for y in 0..h {
        for _x in 0..w {
            let v = 0.6 + 0.25 * (y as f32 / h as f32); // 0.60..0.85, low spread
            px.extend_from_slice(&[v, v, v, 1.0]);
        }
    }
    let src = ferrolite_image::LinearRgbaF32::new(w, h, px).unwrap();
    const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    let base = ferrolite_pipeline::OpStack::default();
    let dehazed = base.set_op(ferrolite_pipeline::Op::Dehaze(ferrolite_pipeline::Dehaze {
        amount: 1.0,
        radius: ferrolite_pipeline::DEHAZE_DEFAULT_RADIUS,
    }));

    let mut p0 = ferrolite_pipeline::EditPipeline::new(ctx.clone(), &src, base, IDENTITY);
    let mut p1 = ferrolite_pipeline::EditPipeline::new(ctx.clone(), &src, dehazed, IDENTITY);
    let a = p0.render_to_image();
    let b = p1.render_to_image();

    // Range (max - min) over the red channel: dehaze must widen it (more contrast).
    let range = |buf: &[u8]| {
        let (mut lo, mut hi) = (255u8, 0u8);
        for px in buf.chunks_exact(4) {
            lo = lo.min(px[0]);
            hi = hi.max(px[0]);
        }
        hi as i32 - lo as i32
    };
    assert!(
        range(&b) > range(&a),
        "positive dehaze widens tonal range: before={} after={}",
        range(&a),
        range(&b)
    );
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --test golden dehaze_positive_increases_contrast_on_hazy_image`
Expected: PASS (or the printed headless-skip line if no GPU adapter — acceptable, matches sibling goldens).

- [ ] **Step 7: Run the workspace gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS. (The `node_count` invalidation tests — if any assert a specific count — now expect 13; update any that assert the old 12. Grep: `rg "node_count" ferrolite-pipeline` and fix expected values.)

- [ ] **Step 8: Commit**

```bash
git add ferrolite-pipeline/src/shaders/dehaze.wgsl ferrolite-pipeline/src/pipeline.rs ferrolite-pipeline/src/lib.rs ferrolite-pipeline/tests/golden.rs
git commit -m "feat(pipeline): dehaze WGSL pass + EditPipeline node (whole-image preview path)"
```

---

## Task 4: Wire dehaze into `TileEditPipeline` + `set_dehaze_atmos` + halo

**Files:**
- Modify: `ferrolite-pipeline/src/tile_edit.rs`
- Modify: `ferrolite-pipeline/tests/golden.rs` (tiled-vs-whole parity with dehaze)

**Interfaces:**
- Consumes: `crate::dehaze::{dehaze_halo, dehaze_uniform, DehazeUniform, DEHAZE_ATMOS_NEUTRAL}`.
- Produces: `TileEditPipeline` gains a `dehaze` node between `contrast_id` and `tone_curve_id`; fields `dehaze_id: NodeId`, `dehaze: Rc<Cell<DehazeUniform>>`, `dehaze_atmos: [f32; 3]`; `pub fn set_dehaze_atmos(&mut self, atmos: [f32; 3])`; the constructor's `halo` folds in `dehaze_halo(stack.dehaze())`. `set_stack` updates the dehaze uniform preserving `dehaze_atmos`.

- [ ] **Step 1: Write the failing parity test**

Add to `ferrolite-pipeline/tests/golden.rs` (mirror the EXISTING tiled-vs-whole parity test in that file — find it via `rg "TileEditPipeline::new" ferrolite-pipeline/tests/golden.rs` and copy its pyramid/compare scaffolding, including how it builds `GpuPyramidSource`, iterates tiles, and its tolerance):

```rust
#[test]
fn dehaze_tiled_matches_whole_image() {
    let Some(ctx) = ferrolite_gpu::GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = std::sync::Arc::new(ctx);
    const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    // Reuse the existing helper the sibling parity test uses (gradient/source +
    // compare_tiled_to_whole). If that helper is private to the test file, follow
    // its exact body here. The KEY difference vs. the sibling test: dehaze needs
    // the SAME atmospheric light on both tiers, so estimate it once from the CPU
    // source and set it on BOTH pipelines.
    let src = common::gradient(300, 200); // whatever the sibling parity test uses
    let stack = ferrolite_pipeline::OpStack::default().set_op(ferrolite_pipeline::Op::Dehaze(
        ferrolite_pipeline::Dehaze {
            amount: 0.8,
            radius: ferrolite_pipeline::DEHAZE_DEFAULT_RADIUS,
        },
    ));
    let atmos = ferrolite_pipeline::estimate_atmospheric_light(&src);

    let mut whole = ferrolite_pipeline::EditPipeline::new(ctx.clone(), &src, stack.clone(), IDENTITY);
    // EditPipeline estimates A internally from `src`; assert it matches what we
    // will hand the tiled tier (same fn, same image → identical).
    let whole_img = whole.render_to_image();

    let pyramid = std::sync::Arc::new(ferrolite_pipeline::GpuPyramidSource::new(&ctx, &src));
    let mut tep = ferrolite_pipeline::TileEditPipeline::new(
        ctx.clone(),
        pyramid,
        stack,
        IDENTITY,
        None,
        None,
    );
    tep.set_dehaze_atmos(atmos);

    // Stitch the produced tiles and compare interior pixels to `whole_img` within
    // the sibling test's tolerance (dehaze is haloed, so seams must match — this
    // is exactly what the halo fold-in guarantees). Reuse the sibling's stitch +
    // assert_close helper.
    common::assert_tiles_match_whole(&ctx, &mut tep, &whole_img, src.width, src.height);
}
```

> If `ferrolite-pipeline/tests/golden.rs` has no reusable stitch helper, replicate the stitch/compare loop from the nearest existing tiled-vs-whole test (e.g. the `Sharpen` or geometry parity test) inline here — do NOT invent a new helper name that doesn't exist.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p ferrolite-pipeline --test golden dehaze_tiled_matches_whole_image`
Expected: FAIL — `set_dehaze_atmos` not found (and, once that compiles but before the node is wired, a pixel mismatch because the tiled tier applies no dehaze).

- [ ] **Step 3: Wire the node into `TileEditPipeline::new`**

In `ferrolite-pipeline/src/tile_edit.rs`:

Add the import (extend the existing `use crate::uniforms::{...}` group with a new `use crate::dehaze::...` line):

```rust
use crate::dehaze::{dehaze_halo, dehaze_uniform, DehazeUniform, DEHAZE_ATMOS_NEUTRAL};
```

Add struct fields (after the `contrast` field, before `tone_curve`):

```rust
    dehaze_id: NodeId,
    dehaze: Rc<Cell<DehazeUniform>>,
    dehaze_atmos: [f32; 3],
```

Fold the dehaze halo into the constructed `halo` (the line currently `let halo = sharpen_halo(...).max(lens_halo_px(...));`):

```rust
        let halo = sharpen_halo(stack.sharpen())
            .max(lens_halo_px(lc.as_ref(), warp_grid))
            .max(dehaze_halo(stack.dehaze()));
```

In the graph build, after the `contrast_id` node block and before the `tone_curve` node block, insert (the atmosphere starts NEUTRAL — the app/export sets the real value via `set_dehaze_atmos` right after construction, exactly like `set_vig_amount`):

```rust
        let dehaze_atmos = DEHAZE_ATMOS_NEUTRAL;
        let dehaze = Rc::new(Cell::new(dehaze_uniform(stack.dehaze(), dehaze_atmos)));
        let dehaze_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/dehaze.wgsl"),
                "dehaze",
                dehaze.clone(),
            )),
            vec![contrast_id],
        );
```

Change the tone-curve node's input from `vec![contrast_id]` to `vec![dehaze_id]`:

```rust
        let tone_curve_id = graph.add_node(
            Box::new(CurveNode::new(ctx.clone(), tone_curve.clone())),
            vec![dehaze_id],
        );
```

Add `dehaze_id,`, `dehaze,`, `dehaze_atmos,` to the `Self { ... }` initializer (near the `contrast` entry).

- [ ] **Step 4: Update `set_stack` + add `set_dehaze_atmos`**

In `TileEditPipeline::set_stack`, after the `self.contrast.set(...)` line, add:

```rust
        self.dehaze
            .set(dehaze_uniform(stack.dehaze(), self.dehaze_atmos));
```

Add the setter method (place it near `set_vig_amount`, mirroring its shape):

```rust
    /// Set the whole-image atmospheric light `A` for the dehaze pass (design
    /// §5.3). Computed ONCE by the caller from the preview-resolution image and
    /// handed to every tile as a uniform — never estimated per tile. Buffer write
    /// only (no rebuild); re-derives the dehaze uniform from the current stack's
    /// amount + this `A`. Call right after construction (like `set_vig_amount`).
    pub fn set_dehaze_atmos(&mut self, atmos: [f32; 3]) {
        if atmos != self.dehaze_atmos {
            self.dehaze_atmos = atmos;
            // Re-derive with the amount + radius currently baked into the uniform.
            let cur = self.dehaze.get();
            let op = if cur.amount != 0.0 {
                Some(crate::op::Dehaze {
                    amount: cur.amount,
                    radius: cur.radius.max(0) as u32,
                })
            } else {
                None
            };
            self.dehaze.set(dehaze_uniform(op, atmos));
            self.graph.mark_dirty(self.dehaze_id);
        }
    }
```

> Note: `set_dehaze_atmos` must run AFTER `set_stack` (or before any `produce_tile`) so `cur.amount` reflects the current op. The app calls it right after `new`/`set_stack`, matching the vignette setters' call order.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p ferrolite-pipeline --test golden dehaze_tiled_matches_whole_image`
Expected: PASS (or headless-skip line). If it fails on seam pixels, the halo fold-in (Step 3) or the `set_dehaze_atmos` ordering is wrong — debug per systematic-debugging, do not loosen the tolerance.

- [ ] **Step 6: Run the workspace gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS. (`ferrolite-export`/`ferrolite-app` still compile — `TileEditPipeline::new`'s signature is unchanged; `set_dehaze_atmos` is additive. Export dehaze correctness is wired in Task 5/8.)

- [ ] **Step 7: Commit**

```bash
git add ferrolite-pipeline/src/tile_edit.rs ferrolite-pipeline/tests/golden.rs
git commit -m "feat(pipeline): dehaze node in TileEditPipeline + set_dehaze_atmos + halo fold-in (tiled parity)"
```

---

## Task 5: Export — thread `A` into `render_tiled`

**Files:**
- Modify: `ferrolite-export/src/render.rs`
- Modify: `ferrolite-export/src/job.rs`
- Modify: `ferrolite-export/tests/render_golden.rs`

**Interfaces:**
- Consumes: `ferrolite_pipeline::{DEHAZE_ATMOS_NEUTRAL}`; `TileEditPipeline::set_dehaze_atmos`.
- Produces: `render_tiled(...)` gains a trailing `atmospheric_light: [f32; 3]` parameter, set on its `TileEditPipeline` before the tile loop. `job.rs` passes the value through (the export job's decoded source is where the app computes it in Task 8; the export crate itself just plumbs the param).

- [ ] **Step 1: Add the param to `render_tiled` and set it on the pipeline**

In `ferrolite-export/src/render.rs`, add `atmospheric_light: [f32; 3]` to the `render_tiled` signature (append it as the last parameter before `progress`, or as the final param — match the file's convention; keep `progress` last as it currently is by inserting `atmospheric_light` just before `cancel`):

```rust
pub fn render_tiled(
    // ... existing params ...
    output_space: WorkingSpace,
    lens_db: Option<&Arc<LensfunDb>>,
    depth: BitDepth,
    atmospheric_light: [f32; 3],
    cancel: &CancelToken,
    progress: &mut dyn FnMut(u32, u32),
) -> Result<RenderedImage, ExportError> {
```

Immediately after the `let mut pipeline = TileEditPipeline::new(...)` block, add:

```rust
    // Dehaze's atmospheric light is a whole-image constant (design §5.3): set it
    // on the tiled producer before rendering any tile. With a no-dehaze stack this
    // is a harmless no-op (amount 0 → identity regardless of A).
    pipeline.set_dehaze_atmos(atmospheric_light);
```

- [ ] **Step 2: Pass it through `job.rs`**

In `ferrolite-export/src/job.rs`, the `render_tiled(...)` call must forward an `atmospheric_light`. The export `job` runs `render_tiled` from within a background job. It receives the CPU source used to build the pyramid — compute `A` there. Find where the job has the decoded `LinearRgbaF32` (or the pyramid's source); add:

```rust
    let atmospheric_light = ferrolite_pipeline::estimate_atmospheric_light(&source_linear);
```

(where `source_linear` is the decoded `LinearRgbaF32` already present in the job — grep the file for the `LinearRgbaF32`/`GpuPyramidSource::new` it holds; if the job only receives a pre-built pyramid and no CPU image, thread `atmospheric_light` as a new parameter of the job entry fn instead, computed by the app in Task 8). Forward it into the `render_tiled(...)` call.

- [ ] **Step 3: Update the render-golden tests**

In `ferrolite-export/tests/render_golden.rs`, every `render_tiled(...)` call now needs the new arg. These tests use no-dehaze stacks, so pass the neutral constant:

```rust
        ferrolite_pipeline::DEHAZE_ATMOS_NEUTRAL,
```

Insert it at the correct positional slot (just before the `cancel`/`&cancel` argument) in each call.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-export`
Expected: PASS (or headless-skip lines). The goldens must be byte-identical to before (neutral `A` + no dehaze op = the pre-existing identity path).

- [ ] **Step 5: Run the workspace gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS. (If `ferrolite-app` calls `render_tiled` directly, it will now fail to compile — that call is fixed in Task 8; if the app only calls the `job.rs` entry fn, the app is unaffected here. Grep `rg "render_tiled" ferrolite-app` to confirm; if there are direct app call sites, either fix them here or note them for Task 8. Prefer fixing them in Task 8 where `A` is computed from the viewer source.)

- [ ] **Step 6: Commit**

```bash
git add ferrolite-export/src/render.rs ferrolite-export/src/job.rs ferrolite-export/tests/render_golden.rs
git commit -m "feat(export): thread whole-image atmospheric light into render_tiled for dehaze"
```

---

## Task 6: App edit setter + rebuild predicate + icon

**Files:**
- Modify: `ferrolite-app/src/develop/ops_edit.rs`
- Modify: `ferrolite-app/src/icons.rs`

**Interfaces:**
- Consumes: `ferrolite_pipeline::{Dehaze, Op, OpStack, OpKind, dehaze_halo}`.
- Produces: `ops_edit::set_dehaze(s: &OpStack, amount: f32, radius: u32) -> OpStack`; `needs_full_rebuild` additionally returns true when `dehaze_halo` differs; `icons::EFFECTS: &str`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `ferrolite-app/src/develop/ops_edit.rs`:

```rust
    #[test]
    fn set_dehaze_identity_when_amount_zero() {
        // Radius alone (amount 0) creates no op.
        let s = set_dehaze(&OpStack::default(), 0.0, 8);
        assert!(s.dehaze().is_none(), "zero amount = no dehaze op");
        let s = set_dehaze(&OpStack::default(), 0.5, 8);
        assert_eq!(
            s.dehaze(),
            Some(ferrolite_pipeline::Dehaze { amount: 0.5, radius: 8 })
        );
        // Negative (add-haze) is a real edit too.
        let s = set_dehaze(&OpStack::default(), -0.3, 12);
        assert_eq!(
            s.dehaze(),
            Some(ferrolite_pipeline::Dehaze { amount: -0.3, radius: 12 })
        );
    }

    #[test]
    fn needs_full_rebuild_on_dehaze_halo_change() {
        let base = OpStack::default();
        // Enabling dehaze introduces a halo → must rebuild the tiled producer.
        let on = set_dehaze(&base, 0.5, 8);
        assert!(needs_full_rebuild(&base, &on), "dehaze on = halo change");
        // Amount-only change (radius unchanged): halo is unchanged → NO rebuild
        // (uniform-only, like a color op).
        let on2 = set_dehaze(&base, 0.9, 8);
        assert!(!needs_full_rebuild(&on, &on2), "amount-only: no rebuild");
        // Radius change alters the halo → rebuild (same as Sharpen's radius).
        let on3 = set_dehaze(&base, 0.9, 16);
        assert!(needs_full_rebuild(&on2, &on3), "radius change: rebuild");
        // Turning dehaze off removes the halo → rebuild.
        assert!(needs_full_rebuild(&on, &base), "dehaze off = halo change");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-app ops_edit::`
Expected: FAIL — `set_dehaze` not found; `needs_full_rebuild` does not yet consider dehaze.

- [ ] **Step 3: Add `set_dehaze`**

In `ferrolite-app/src/develop/ops_edit.rs`, extend the top `use` to include `Dehaze` and `dehaze_halo`:

```rust
use ferrolite_pipeline::{
    dehaze_halo, sharpen_halo, ColorGrade, Contrast, Dehaze, Exposure, LensCorrection, Op, OpStack,
    Sharpen, ToneCurve, WhiteBalance,
};
```

Add the setter after `set_contrast` (keeping the canonical order — dehaze follows contrast):

```rust
pub fn set_dehaze(s: &OpStack, amount: f32, radius: u32) -> OpStack {
    if amount == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::Dehaze)
    } else {
        s.set_op(Op::Dehaze(Dehaze { amount, radius }))
    }
}
```

- [ ] **Step 4: Extend `needs_full_rebuild`**

Add the dehaze-halo clause to `needs_full_rebuild`:

```rust
pub fn needs_full_rebuild(old: &OpStack, new: &OpStack) -> bool {
    old.geometry() != new.geometry()
        || sharpen_halo(old.sharpen()) != sharpen_halo(new.sharpen())
        || dehaze_halo(old.dehaze()) != dehaze_halo(new.dehaze())
        || lens_rebuild_key(old) != lens_rebuild_key(new)
}
```

- [ ] **Step 5: Add the `EFFECTS` icon alias**

In `ferrolite-app/src/icons.rs`, add after the `GRADE` alias (around line 21):

```rust
pub const EFFECTS: &str = p::SPARKLE;
```

(`egui_phosphor::regular::SPARKLE` is verified to exist in 0.7.3. `SPARKLE` reads as a generic "effects" mark — apt for the tab that will later host clarity/texture/grain. If a more dehaze-specific glyph is preferred, `p::CLOUD_FOG` also exists and is verified.)

Add `("EFFECTS", EFFECTS),` to the `every_alias_is_nonempty` test array in the same file.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-app ops_edit:: && cargo test -p ferrolite-app icons::`
Expected: PASS.

- [ ] **Step 7: Run the workspace gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add ferrolite-app/src/develop/ops_edit.rs ferrolite-app/src/icons.rs
git commit -m "feat(app): set_dehaze edit helper, dehaze in needs_full_rebuild, EFFECTS icon alias"
```

---

## Task 7: App — the Effects tab (Dehaze slider)

**Files:**
- Modify: `ferrolite-app/src/develop/base_tabs.rs`

**Interfaces:**
- Consumes: `ops_edit::set_dehaze`; `EguiSlider`; `PanelTab`/`TabId`; `EditOutcome`; `OpKind::Dehaze`; `ferrolite_pipeline::DEHAZE_DEFAULT_RADIUS`.
- Produces: `pub struct EffectsTab` implementing `PanelTab` (`id() == TabId("effects")`, `label() == "Effects"`); registered in `base_tabs()` after `CurveTab` (so the tab order reads Light · Color · Grade · Curve · **Effects** · Detail · Optics). Two controls: bipolar **Dehaze** amount + unipolar **Radius** (px), each with the `EguiSlider` reset column — mirroring `DetailTab`'s Sharpen amount+radius pair.

- [ ] **Step 1: Add the `EffectsTab`**

In `ferrolite-app/src/develop/base_tabs.rs`, add after the `CurveTab` impl (before `DetailTab`):

```rust
pub struct EffectsTab;
impl PanelTab for EffectsTab {
    fn id(&self) -> TabId {
        TabId("effects")
    }
    fn label(&self) -> &str {
        "Effects"
    }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        let stack = state.viewer.as_ref()?.op_stack.clone();
        let mut out: Option<EditOutcome> = None;

        // Seed both controls from the op; a brand-new op uses the default radius
        // (mirrors DetailTab's Sharpen amount+radius seeding). Per-control reset is
        // the EguiSlider reset column (CLAUDE.md — load-bearing).
        let d = stack.dehaze();
        let mut amount = d.map(|d| d.amount).unwrap_or(0.0);
        let mut radius = d
            .map(|d| d.radius as f32)
            .unwrap_or(ferrolite_pipeline::DEHAZE_DEFAULT_RADIUS as f32);

        // Dehaze amount (bipolar): >0 removes haze, <0 adds haze.
        let ra = ui.add(EguiSlider {
            label: "Dehaze",
            value: &mut amount,
            min: -1.0,
            max: 1.0,
            default: 0.0,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: true,
            signed: true,
        });
        // Radius (px) of the dark-channel patch (unipolar; drives the halo).
        let rr = ui.add(EguiSlider {
            label: "Radius",
            value: &mut radius,
            min: 1.0,
            max: 24.0,
            default: ferrolite_pipeline::DEHAZE_DEFAULT_RADIUS as f32,
            step: 1.0,
            decimals: 0,
            unit: " px",
            bipolar: false,
            signed: false,
        });
        if ra.changed() || rr.changed() {
            out = Some(EditOutcome {
                stack: ops_edit::set_dehaze(&stack, amount, radius.round() as u32),
                kind: OpKind::Dehaze,
                commit: (ra.drag_stopped() || rr.drag_stopped()) || !(ra.dragged() || rr.dragged()),
            });
        }

        out
    }
}
```

- [ ] **Step 2: Register it in `base_tabs()`**

Add `Box::new(EffectsTab),` after `Box::new(CurveTab),`:

```rust
pub fn base_tabs() -> Vec<Box<dyn PanelTab>> {
    vec![
        Box::new(LightTab),
        Box::new(ColorTab),
        Box::new(GradeTab),
        Box::new(CurveTab),
        Box::new(EffectsTab),
        Box::new(DetailTab),
        Box::new(OpticsTab),
    ]
}
```

- [ ] **Step 3: Verify `EguiSlider`/`ops_edit`/`OpKind` are in scope**

`base_tabs.rs` already imports `EguiSlider`, `ops_edit`, and `OpKind` (used by `LightTab`/`DetailTab`). No new imports needed. Confirm by building.

- [ ] **Step 4: Build + run the (compile-level) tests**

Run: `cargo build -p ferrolite-app && cargo test -p ferrolite-app`
Expected: PASS (compiles; existing app tests unaffected).

- [ ] **Step 5: Run the workspace gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/develop/base_tabs.rs
git commit -m "feat(app): Effects develop tab with per-control-reset bipolar Dehaze slider"
```

---

## Task 8: App runtime — compute `A` once and thread it to every producer/export

**Files:**
- Modify: `ferrolite-app/src/viewer/edit_producer.rs` (`set_dehaze_atmos` delegate)
- Modify: `ferrolite-app/src/app.rs` (compute `A`; call the setter at every producer build; thread `A` to export)
- Modify: `ferrolite-app/src/export/mod.rs`, `ferrolite-app/src/export/batch.rs` (compute + pass `A` to the export job/`render_tiled`)

**Interfaces:**
- Consumes: `ferrolite_pipeline::estimate_atmospheric_light`; `TileEditPipeline::set_dehaze_atmos`; `EditPipeline` (computes `A` internally, no app action needed for the preview tier).
- Produces: `EditTileProducer::set_dehaze_atmos(&mut self, atmos: [f32; 3])`; every `TileEditPipeline` the app builds has `A` set from the current image's preview source; export builds compute `A` from the decoded source.

- [ ] **Step 1: Add the `EditTileProducer` delegate**

In `ferrolite-app/src/viewer/edit_producer.rs`, add after `set_vig_manual` (mirror its shape):

```rust
    /// Set the dehaze atmospheric light on the underlying tiled pipeline (design
    /// §5.3). Called once per image after the producer is built.
    pub fn set_dehaze_atmos(&mut self, atmos: [f32; 3]) {
        self.pipeline.set_dehaze_atmos(atmos);
    }
```

- [ ] **Step 2: Compute `A` at each full-res producer build (`app.rs`)**

There are the `TileEditPipeline::new` producer builds in `app.rs` (the full-decode path ~line 1212 and the lens-baked rebuild ~line 1380, plus any in `set_preview_and_full` ~line 1552). At EACH, the CPU preview source is on the viewer as `raw_preview_source` (RAW) or `preview_source` (Standard). Immediately after `let mut producer = viewer::EditTileProducer::new(tep);` and the existing `producer.set_vig_amount(...)/set_vig_manual(...)` calls, add:

```rust
            // Whole-image atmospheric light for dehaze (design §5.3): estimate once
            // from the decoded preview source and hand it to the tiled producer.
            // Same fn + same source the preview EditPipeline uses internally, so
            // the two tiers agree. Cheap + bounded (subsampled) — safe here.
            if let Some(src) = v
                .raw_preview_source
                .as_ref()
                .or(v.preview_source.as_ref())
            {
                producer.set_dehaze_atmos(ferrolite_pipeline::estimate_atmospheric_light(src));
            }
```

> Adjust the borrow to the surrounding code: at each site `v` is the mutable viewer borrow already in scope (the same one used for `v.pyramid`/`v.op_stack`). If the source `Arc`s are named differently at a given site (grep `preview_source`/`raw_preview_source` around each build), use whichever holds the decoded `LinearRgbaF32` for the current image. If neither is present yet at a site (e.g. a rebuild before the preview source is stored), skip — the `if let Some` guard handles it, and the next rebuild will set it. For the common live-edit path, `raw_preview_source`/`preview_source` is always populated by the time the producer exists.

- [ ] **Step 3: Verify the preview tier needs no change**

The preview `EditPipeline` computes `A` internally from its `source` arg (Task 3). At every `EditPipeline::new` in `app.rs` the `source` is the same `raw_preview_source`/`preview_source`/`src` used above, so no extra call is needed for the preview tier. Confirm by reading each `EditPipeline::new` site — if any builds the preview from a DIFFERENT image than the tiled producer, note it (it would only affect dehaze consistency, and both still use `estimate_atmospheric_light` on their own source; acceptable). No code change in this step.

- [ ] **Step 4: Thread `A` into the export path**

In `ferrolite-app/src/export/mod.rs` and `ferrolite-app/src/export/batch.rs`, each builds a `GpuPyramidSource::new(&gpu, &linear)` (or from `ExportSource::FullResCpu(img)`) then invokes the export job / `render_tiled`. Right where the CPU `LinearRgbaF32` (`linear`/`img`) is in scope, compute:

```rust
    let atmospheric_light = ferrolite_pipeline::estimate_atmospheric_light(&linear);
```

and forward it into the export job entry fn / `render_tiled` call (the param added in Task 5). If the export job entry fn (in `ferrolite-export/src/job.rs`) does NOT already have the CPU image (only the pyramid), add an `atmospheric_light: [f32; 3]` parameter to that entry fn and pass this value; that is the cleanest split (the app owns the CPU image; the export crate just plumbs the value to `render_tiled`). Update Task 5's `job.rs` accordingly if you took that route.

- [ ] **Step 5: Build + run the app + export tests**

Run: `cargo build -p ferrolite-app && cargo test -p ferrolite-app && cargo test -p ferrolite-export`
Expected: PASS.

- [ ] **Step 6: Run the workspace gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/viewer/edit_producer.rs ferrolite-app/src/app.rs ferrolite-app/src/export/mod.rs ferrolite-app/src/export/batch.rs
git commit -m "feat(app): estimate whole-image atmospheric light once and thread it to tiled + export dehaze"
```

---

## Final: workspace gate + visual test handoff

- [ ] **Step 1: Full workspace gate**

Run:
```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all green.

- [ ] **Step 2: STOP — hand the author the visual test plan (CLAUDE.md, load-bearing)**

Do NOT merge/PR. The automated gate being green is necessary but not sufficient. Present the numbered visual test checklist below and hold for Jann's hands-on results; address any issues found before finishing the branch.

**Visual test plan (Develop module, real app):**

1. **Effects tab appears & resets.** Open any image → Develop. In the right panel tab strip, confirm a new **Effects** tab between **Curve** and **Detail**. Open it: a **Dehaze** slider (centered at 0, bipolar) and a **Radius** slider (px, unipolar, seeded at 8). *Failure:* tab missing/mis-ordered, Dehaze not bipolar/not centered, or Radius missing.
2. **Dehaze removes haze (positive).** Use a genuinely hazy shot (distant landscape/fog). Drag Dehaze right toward +1. Expect increasing local contrast and color "punch," haze lifting — smoothly during the drag (no stutter/freeze). *Failure:* no visible change, banding, a multi-second freeze on drag, or a hard jump.
3. **Dehaze adds haze (negative).** Drag left toward −1. Expect the image getting flatter/foggier (contrast pulled toward a uniform bright veil) — the symmetric inverse. *Failure:* negative does nothing, or looks identical to positive.
4. **Per-control reset (both sliders).** After non-zero settings, reset the **Dehaze** slider (double-click / reset arrow column): it returns to 0 and the image to its pre-dehaze look. Separately reset **Radius**: it returns to 8 without touching the amount. Neither reset may disturb the other control. *Failure:* a reset absent, or it disturbs its neighbour.
   - **Radius effect & no-freeze on rebuild.** With a strong Dehaze, drag **Radius** up (e.g. 8 → 24): the transmission estimate coarsens (softer, larger-scale haze lift). A radius change rebuilds the full-res producer (halo change) — confirm no multi-second freeze, just a brief re-render. *Failure:* radius does nothing, or a radius drag stalls the UI.
5. **Preview ↔ full-res consistency.** At fit-zoom set a strong dehaze, then zoom to 1:1 so full-res tiles render. The dehaze look must be seamless across tile boundaries and match the fit-zoom preview (no per-tile brightness/contrast seams). *This is the whole-image-`A` design point.* *Failure:* visible tile seams or a preview-vs-1:1 mismatch in the dehazed look.
6. **No-freeze on open/navigate.** Open several images in a row (some large RAW). The `A` estimate runs at open; confirm no added hitch vs. before. *Failure:* a stall on open attributable to dehaze.
7. **Persistence round-trip.** Set a dehaze amount, navigate away and back (or reopen). The amount must persist and re-render identically. Confirm an image edited BEFORE this feature (no Dehaze in its sidecar) still opens unchanged. *Failure:* amount lost, or an old sidecar renders differently.
8. **Export matches preview.** Export an image with a strong dehaze; open the exported file. The dehaze look must match the in-app 1:1 render. *Failure:* export shows no/different dehaze (would indicate `A` not threaded into `render_tiled`).

Fixtures: use one clearly hazy RAW and one clear/low-haze image (to confirm dehaze on a clear image is subtle, not destructive). Note item 5 needs zoom to 1:1 to exercise the tiled producer.

---

## Self-Review (completed against design §5 + §2)

- **§5.1 op model** → Task 1 (`Dehaze { amount, radius }`, `Op::Dehaze`, `dehaze()`). **Deviation (author-approved):** spec §5.1 listed `Dehaze { amount }` only; the patch radius is exposed as a second control (mirroring `Sharpen { amount, radius }`) per the author's request. Consistent with §5.2's "patch radius `r` ... plumbed like Sharpen" — the radius was always going to exist; it is now user-facing rather than a fixed constant.
- **§5.2 algorithm** (dark channel min-filter, transmission, recovery, bipolar blend) → Task 2 pure math (`dark_channel`, `dehaze_recover`) + Task 3 WGSL (mirrors it).
- **§5.3 tiling — `A` is a whole-image uniform, once, not per-tile** → Task 2 `estimate_atmospheric_light` (bounded/subsampled), Task 3 `EditPipeline` internal estimate, Task 4 `set_dehaze_atmos` uniform on the tiled tier, Task 8 app computes once + threads to producers + export.
- **§5.4 UI — new Effects tab, bipolar slider, per-control reset, icon alias** → Task 6 (`EFFECTS` icon) + Task 7 (`EffectsTab` with the bipolar Dehaze slider **and** the unipolar Radius slider, each with its own `EguiSlider` reset column).
- **§5.5 tests** — identity at amount 0 (Task 2), synthetic hazy +/- behavior (Task 2), halo reported correctly (Task 2 `dehaze_halo` + Task 4 fold-in + parity golden), golden at fixed positive amount (Task 3) → all covered.
- **§2.1 op order + serde-safe renumber + guard test kept** → Task 1 (insert after Contrast, renumber tail, `opkind_renumber_does_not_change_serde_output` retained + extended by `dehaze_roundtrips_and_renumber_is_serde_stable`).
- **§2.5 pure reusable fn** → `dehaze_recover` is a free `pub fn` in `dehaze.rs`, reused by the shader-mirror and (future) per-mask path; no transform logic lives only in the node.
- **§2.3 contracts** — GPU executor untouched (Dehaze is a `PointOpNode`); halo consumer (halo fold-in); `A` off the per-frame path (bounded estimate at construction). No new deps.
- **No placeholders:** every code step has full code; the two "match the sibling test scaffolding" notes (golden stitch helper) point at existing in-repo tests to copy rather than inventing names — the implementer must read those tests, which is why exact helper names are deliberately not fabricated here.
- **Type consistency:** `set_dehaze_atmos([f32;3])`, `estimate_atmospheric_light(&LinearRgbaF32)->[f32;3]`, `dehaze_recover([f32;3],f32,[f32;3],f32)->[f32;3]`, `dehaze_halo(Option<Dehaze>)->u32`, `DehazeUniform` used identically across Tasks 2/3/4/5/8.
