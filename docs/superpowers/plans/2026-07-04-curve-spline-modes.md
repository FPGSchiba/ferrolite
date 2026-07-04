# Curve Spline Modes + Reusable Component — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a **Smooth** (monotone cubic) interpolation mode to the tone curve alongside **Linear**, and extract the interactive curve widget into a reusable `ferrolite-app` component ready for future per-channel color curves.

**Architecture:** `ferrolite-pipeline` (photo tier) gains a `CurveMode` enum + a `mode` field on `ToneCurve`, and its CPU-side LUT bake (`curve_lut`/`curve_interp`) interpolates per mode (Linear = today; Smooth = Fritsch–Carlson monotone cubic Hermite). The interpolation is exposed publicly so the `ferrolite-app` widget renders the exact applied shape. The widget moves to `ferrolite-app/src/widgets/curve.rs` as a reusable `curve_editor`, and `develop/curve_widget.rs` becomes a thin adapter.

**Tech Stack:** Rust, egui 0.29, wgpu (LUT applied on GPU, unchanged), serde.

## Global Constraints

- **Tiers:** only `ferrolite-pipeline` + `ferrolite-app` change (both photo tier). NO engine-tier edits (`ferrolite-image`/`-gpu`/`-vt`). Interpolation is pure math — **no new dependencies, no copyleft**.
- **Additive + back-compat:** `mode` is an additive `#[serde(default)]` field on the existing `ToneCurve`; sidecars written before this feature (no `mode` key) MUST deserialize as `CurveMode::Linear` (today's exact behavior). New in-app curves default to `CurveMode::Smooth`.
- **Contracts:** tone curve stays a `ferrolite-pipeline` node on the unchanged `Graph<PipelineImage>`; GPU still applies a 256-entry LUT — only the CPU bake changes.
- **CLAUDE.md:** the mode selector gets its own per-control reset (shared reset affordance); no UI-thread blocking (LUT bake is the same cheap 256-entry CPU pass); green gate (`cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace`) then HOLD for the author's visual test.
- **Rust style:** `cargo fmt`; clippy `-D warnings`; no `unwrap()` outside tests; files focused.

---

## Phase 1 — Pipeline: CurveMode + monotone-cubic interpolation

### Task 1: `CurveMode` enum + `ToneCurve.mode` field (back-compat serde)

**Files:**
- Modify: `ferrolite-pipeline/src/op.rs`
- Test: same file (`#[cfg(test)]`).

**Interfaces:**
- Produces: `pub enum CurveMode { Linear, Smooth }` (Default = Linear); `ToneCurve { points, mode }`.

- [ ] **Step 1: Write the failing test** in `op.rs` tests:

```rust
#[test]
fn tonecurve_without_mode_field_deserializes_as_linear() {
    // A sidecar written before this feature has no `mode` key.
    let json = r#"{ "points": [[0.0,0.0],[1.0,1.0]] }"#;
    let tc: ToneCurve = serde_json::from_str(json).unwrap();
    assert_eq!(tc.mode, CurveMode::Linear);
}

#[test]
fn tonecurve_mode_roundtrips() {
    let tc = ToneCurve { points: vec![(0.0, 0.0), (1.0, 1.0)], mode: CurveMode::Smooth };
    let s = serde_json::to_string(&tc).unwrap();
    assert_eq!(serde_json::from_str::<ToneCurve>(&s).unwrap(), tc);
}
```

> **Implementer:** confirm `serde_json` is available as a dev-dependency of `ferrolite-pipeline`; if the test needs it and it's missing, add `serde_json = { workspace = true }` under `[dev-dependencies]` in `ferrolite-pipeline/Cargo.toml` (it is already a workspace dep).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p ferrolite-pipeline tonecurve`
Expected: FAIL (no `mode` field / no `CurveMode`).

- [ ] **Step 3: Implement** in `op.rs`:

```rust
/// Interpolation between tone-curve control points.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum CurveMode {
    /// Piecewise linear (sharp corners at control points).
    Linear,
    /// Monotone cubic Hermite (smooth, monotonic, no overshoot).
    Smooth,
}

impl Default for CurveMode {
    fn default() -> Self {
        CurveMode::Linear // legacy back-compat: sidecars without `mode` load as Linear
    }
}
```

And extend `ToneCurve`:

```rust
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ToneCurve {
    /// Control points in [0,1]×[0,1] (x ascending). Identity = `[(0,0),(1,1)]`
    /// or empty. Baked to a 256-entry monotone LUT by `uniforms::curve_lut`.
    pub points: Vec<(f32, f32)>,
    /// Interpolation mode. Absent in pre-feature sidecars → Linear (serde default).
    #[serde(default)]
    pub mode: CurveMode,
}
```

> **Implementer:** this breaks every `ToneCurve { points: ... }` literal in the crate (and in `ferrolite-app`). Update each construction site to add `mode: <value>`:
> - In `ferrolite-pipeline` (`serialize.rs`, `op.rs` tests, `tests/golden.rs`): use `mode: CurveMode::Linear` unless a test specifically targets Smooth.
> - Leave `ferrolite-app` sites for Phase 3 (they compile-break until then; Phase 1's own crate must build + test green). If a cross-crate build is run, `ferrolite-app` will fail to compile until Phase 3 — that's expected; verify with `cargo test -p ferrolite-pipeline` (crate-scoped) in this phase.

- [ ] **Step 4: Run to verify passes**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS (new serde tests + existing pipeline tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-pipeline/src/op.rs ferrolite-pipeline/src/serialize.rs ferrolite-pipeline/tests/golden.rs ferrolite-pipeline/Cargo.toml
git commit -m "feat(pipeline): add CurveMode + ToneCurve.mode (serde default Linear)"
```

### Task 2: Monotone cubic interpolation in `curve_lut` (+ public exposure)

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (expose `CurveMode` + interpolation)
- Modify: call sites of `curve_lut` (`nodes.rs` / wherever it's baked) to pass the mode.
- Test: `uniforms.rs` (`#[cfg(test)]`).

**Interfaces:**
- Consumes: `crate::op::CurveMode`.
- Produces: `pub fn curve_lut(points: &[(f32,f32)], mode: CurveMode) -> [f32; 256]`; a public re-export so `ferrolite-app` can sample the same interpolation (e.g. `pub use uniforms::curve_lut;` and `pub use op::CurveMode;` in `lib.rs`).

- [ ] **Step 1: Write failing tests** in `uniforms.rs`:

```rust
#[test]
fn linear_mode_matches_legacy_lut() {
    // Linear must reproduce the pre-feature piecewise-linear LUT exactly.
    let pts = [(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)];
    let lut = curve_lut(&pts, crate::op::CurveMode::Linear);
    // midpoint pulled below diagonal, endpoints pinned (same asserts as the old test)
    assert!(lut[128] < 128.0 / 255.0);
    assert!((lut[0] - 0.0).abs() < 1e-6);
    assert!((lut[255] - 1.0).abs() < 1e-6);
}

#[test]
fn smooth_passes_through_control_points() {
    // At a control point's x, the smooth LUT equals its y (within LUT quantization).
    let pts = [(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)];
    let lut = curve_lut(&pts, crate::op::CurveMode::Smooth);
    let idx = (0.5 * 255.0).round() as usize; // x = 0.5
    assert!((lut[idx] - 0.25).abs() < 0.02, "smooth LUT hits the control point");
}

#[test]
fn smooth_is_monotonic_and_no_overshoot() {
    let pts = [(0.0, 0.0), (0.3, 0.7), (0.7, 0.72), (1.0, 1.0)]; // steep then flat — classic overshoot trap
    let lut = curve_lut(&pts, crate::op::CurveMode::Smooth);
    for i in 1..256 {
        assert!(lut[i] >= lut[i - 1] - 1e-6, "monotonic non-decreasing at {i}");
        assert!((0.0..=1.0).contains(&lut[i]), "no overshoot outside [0,1] at {i}");
    }
    // No overshoot above the local max (0.72) in the flat middle region: sample x≈0.5
    let mid = (0.5 * 255.0).round() as usize;
    assert!(lut[mid] <= 0.72 + 1e-3, "monotone cubic must not bulge above neighboring control y");
}
```

- [ ] **Step 2: Run to verify fails**

Run: `cargo test -p ferrolite-pipeline curve`
Expected: FAIL (`curve_lut` arity changed / Smooth not implemented).

- [ ] **Step 3: Implement.** Change `curve_interp` to take the mode and add the monotone-cubic branch; thread `mode` through `curve_lut`:

```rust
pub fn curve_lut(points: &[(f32, f32)], mode: crate::op::CurveMode) -> [f32; 256] {
    let mut pts: Vec<(f32, f32)> = points
        .iter()
        .map(|&(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
        .collect();
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if pts.is_empty() {
        pts = vec![(0.0, 0.0), (1.0, 1.0)];
    }
    // Precompute monotone-cubic tangents once (only used by Smooth).
    let tangents = (mode == crate::op::CurveMode::Smooth).then(|| fritsch_carlson_tangents(&pts));

    let mut lut = [0.0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let x = i as f32 / 255.0;
        *slot = match mode {
            crate::op::CurveMode::Linear => curve_interp_linear(&pts, x),
            crate::op::CurveMode::Smooth => curve_interp_smooth(&pts, tangents.as_ref().unwrap(), x),
        };
    }
    for i in 1..256 {
        if lut[i] < lut[i - 1] {
            lut[i] = lut[i - 1];
        }
    }
    lut
}

/// Rename of the existing linear interpolation (unchanged body).
fn curve_interp_linear(pts: &[(f32, f32)], x: f32) -> f32 {
    if x <= pts[0].0 { return pts[0].1; }
    let last = pts[pts.len() - 1];
    if x >= last.0 { return last.1; }
    for w in pts.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        if x >= x0 && x <= x1 {
            let t = if (x1 - x0).abs() < 1e-9 { 0.0 } else { (x - x0) / (x1 - x0) };
            return y0 + t * (y1 - y0);
        }
    }
    last.1
}

/// Fritsch–Carlson monotone tangents for control points (x ascending).
fn fritsch_carlson_tangents(pts: &[(f32, f32)]) -> Vec<f32> {
    let n = pts.len();
    if n < 2 { return vec![0.0; n]; }
    // Secant slopes.
    let mut d = vec![0.0f32; n - 1];
    for i in 0..n - 1 {
        let dx = pts[i + 1].0 - pts[i].0;
        d[i] = if dx.abs() < 1e-9 { 0.0 } else { (pts[i + 1].1 - pts[i].1) / dx };
    }
    // Initial tangents (average of adjacent secants; ends = one-sided).
    let mut m = vec![0.0f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = (d[i - 1] + d[i]) / 2.0;
    }
    // Fritsch–Carlson limiter: enforce monotonicity / no overshoot.
    for i in 0..n - 1 {
        if d[i].abs() < 1e-9 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            let alpha = m[i] / d[i];
            let beta = m[i + 1] / d[i];
            let s = alpha * alpha + beta * beta;
            if s > 9.0 {
                let tau = 3.0 / s.sqrt();
                m[i] = tau * alpha * d[i];
                m[i + 1] = tau * beta * d[i];
            }
        }
    }
    m
}

fn curve_interp_smooth(pts: &[(f32, f32)], m: &[f32], x: f32) -> f32 {
    if x <= pts[0].0 { return pts[0].1; }
    let last = pts[pts.len() - 1];
    if x >= last.0 { return last.1; }
    for i in 0..pts.len() - 1 {
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[i + 1];
        if x >= x0 && x <= x1 {
            let h = x1 - x0;
            if h.abs() < 1e-9 { return y1; }
            let t = (x - x0) / h;
            let t2 = t * t;
            let t3 = t2 * t;
            let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
            let h10 = t3 - 2.0 * t2 + t;
            let h01 = -2.0 * t3 + 3.0 * t2;
            let h11 = t3 - t2;
            return h00 * y0 + h10 * h * m[i] + h01 * y1 + h11 * h * m[i + 1];
        }
    }
    last.1
}
```

Update the `curve_lut` call site(s) (the tone-curve node's LUT bake — `grep -rn "curve_lut" ferrolite-pipeline/src`) to pass the op's `mode`. In `lib.rs`, add `pub use op::CurveMode;` and expose the interpolation for the app: `pub use uniforms::curve_lut;` (or, to avoid widening the whole `uniforms` module, add `pub fn curve_sample(points: &[(f32,f32)], mode: CurveMode, x: f32) -> f32` that builds tangents + dispatches, and `pub use` that). Keep whichever is the smaller public surface; the app needs to draw the curve, so exposing `curve_lut` (sample 256 points) is sufficient.

- [ ] **Step 4: Run to verify passes**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS (new curve tests + existing goldens still green for Linear).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/lib.rs ferrolite-pipeline/src/nodes.rs
git commit -m "feat(pipeline): monotone-cubic Smooth curve mode + public curve_lut"
```

### Task 3: Smooth-mode golden test

**Files:**
- Modify: `ferrolite-pipeline/tests/golden.rs` (+ a new golden image beside the existing tone-curve golden).

- [ ] **Step 1:** Read the existing `tone_curve_darken_midtones_matches_golden` test and its golden-image setup. Add a sibling test `tone_curve_smooth_matches_golden` that builds `Op::ToneCurve(ToneCurve { points: vec![(0.0,0.0),(0.5,0.3),(1.0,1.0)], mode: CurveMode::Smooth })`, evaluates, and compares to a newly-captured golden.
- [ ] **Step 2:** Generate the golden the same way the existing tone-curve golden is generated (follow the crate's golden-capture convention — e.g. an env var / regenerate step documented near the test). Commit the golden asset.
- [ ] **Step 3: Run**

Run: `cargo test -p ferrolite-pipeline golden`
Expected: PASS (both Linear and Smooth tone-curve goldens).

- [ ] **Step 4: Commit**

```bash
git add ferrolite-pipeline/tests/golden.rs ferrolite-pipeline/tests/<golden asset path>
git commit -m "test(pipeline): golden for Smooth tone-curve mode"
```

---

## Phase 2 — App: reusable curve component

### Task 4: Extract `curve_editor` into `widgets/curve.rs`

**Files:**
- Create: `ferrolite-app/src/widgets/curve.rs`
- Modify: `ferrolite-app/src/widgets/mod.rs` (or wherever `widgets` submodules are declared) to add `pub mod curve;`
- Reference: current `ferrolite-app/src/develop/curve_widget.rs` (the interaction to migrate) + `develop/curve_math.rs`.

**Interfaces:**
- Consumes: `ferrolite_pipeline::CurveMode`, `ferrolite_pipeline::curve_lut` (or `curve_sample`), `crate::develop::curve_math`, `crate::widgets::draw_reset_arrow`.
- Produces:
```rust
pub struct CurveStyle { pub curve_color: egui::Color32, pub point_color: egui::Color32 }
pub struct CurveEdit {
    pub points: Vec<(f32, f32)>,
    pub mode: ferrolite_pipeline::CurveMode,
    pub reset: bool,
    pub commit: bool,
}
pub fn curve_editor(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    points: &[(f32, f32)],
    mode: ferrolite_pipeline::CurveMode,
    style: &CurveStyle,
) -> Option<CurveEdit>;
```

- [ ] **Step 1:** Read the CURRENT `develop/curve_widget.rs` in full. Move its interaction + paint logic into `widgets/curve.rs::curve_editor`, generalizing:
  - Take `points`, `mode`, `style`, and an `id_source` (salt all `ui.memory`/`ui.interact` ids with it, so two curve editors on one screen don't collide — replace the current `resp.id`-only keys with ids derived from `id_source`).
  - Preserve ALL Spec 4.1 §3.E behavior: bigger hit radius, hover highlight, `grab_or_insert`, click-to-select, double-click / right-click / select+Delete deletion (endpoints protected), and the written-out "Reset" button under the curve.
  - **Render the curve using the pipeline interpolation for `mode`:** sample `ferrolite_pipeline::curve_lut(&points, mode)` (256 values) and draw that polyline, so Smooth shows a smooth curve and Linear shows straight segments — matching the applied result. (Control-point dots + interaction stay in normalized space as today.)
  - Use `style.curve_color`/`point_color` instead of hardcoded `theme::ACCENT`.
  - Return `CurveEdit { points, mode, reset, commit }` (mode unchanged here — the selector is Task 5). `reset=true` when the Reset button is clicked; `commit` per the existing commit semantics (drag-release / discrete change / delete / insert).

- [ ] **Step 2:** Build the app (the tone curve still uses the old widget until Task 6, so both may exist briefly; ensure `widgets/curve.rs` compiles standalone).

Run: `cargo build -p ferrolite-app`
Expected: compiles (Phase 1 pipeline changes are in; `ferrolite-app`'s own `ToneCurve` literals are fixed in Task 6 — if the app doesn't yet build because of Task 1's `mode` field, do the minimal literal fix here or note it for Task 6; prefer making the app compile at each task by adding `mode: CurveMode::Linear` to existing app-side `ToneCurve {..}` literals now).

> **Implementer:** because Task 1 added a required `mode` field, `ferrolite-app` will not compile until its `ToneCurve {..}` literals include `mode`. Fix those literal sites in THIS task (add `mode: ferrolite_pipeline::CurveMode::Linear` to existing constructions) so the crate builds; Task 6 changes the new-curve default to Smooth at the creation site.

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/widgets/curve.rs ferrolite-app/src/widgets/mod.rs ferrolite-app/src/develop/
git commit -m "feat(app): reusable curve_editor widget (mode-aware rendering)"
```

### Task 5: Mode selector (Linear/Smooth) with per-control reset

**Files:**
- Modify: `ferrolite-app/src/widgets/curve.rs`

- [ ] **Step 1:** Add a mode selector below the curve, beside the "Reset" button: a small segmented control (two `selectable_label`s "Linear" / "Smooth") bound to the incoming `mode`. When the user changes it, return `CurveEdit { points: <current>, mode: <new>, reset: false, commit: true }` (mode change is a committed edit).
- [ ] **Step 2:** Give the selector its **own per-control reset** (CLAUDE.md rule): a `draw_reset_arrow` next to it that, when the current mode differs from the default, resets the mode to the default. Define the "default" as `CurveMode::Smooth` (the app's new-curve default) — clicking reset returns the selector to Smooth. (Rationale: within an active edit, the per-control reset restores the app default, consistent with sliders resetting to their neutral value.)
- [ ] **Step 3:** Build.

Run: `cargo build -p ferrolite-app`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/widgets/curve.rs
git commit -m "feat(app): curve mode selector (Linear/Smooth) with per-control reset"
```

---

## Phase 3 — App: tone-curve wiring + defaults

### Task 6: Tone-curve adapter over the reusable widget (new-curve default Smooth)

**Files:**
- Modify: `ferrolite-app/src/develop/curve_widget.rs`

**Interfaces:**
- Consumes: `crate::widgets::curve::{curve_editor, CurveStyle, CurveEdit}`, `ferrolite_pipeline::{CurveMode, ToneCurve, Op, OpKind, OpStack}`, `crate::theme`.

- [ ] **Step 1:** Rewrite `develop/curve_widget.rs::show(ui, stack) -> Option<EditOutcome>` as a thin adapter:
  - Read the current curve: `let tc = stack.tone_curve(); let points = tc.as_ref().map(|t| t.points.clone()).filter(|p| !p.is_empty()).unwrap_or_else(curve_math::identity_points); let mode = tc.map(|t| t.mode).unwrap_or(CurveMode::Smooth);`
    - Note: `tone_curve()` returns `None` when no curve op exists → the editor shows an identity curve with mode = **Smooth** (the new-curve default), so a user's first edit is smooth.
  - Call `widgets::curve::curve_editor(ui, "tone_curve", &points, mode, &CurveStyle { curve_color: theme::ACCENT, point_color: theme::ACCENT_BRIGHT })`.
  - Map the returned `CurveEdit`:
    - `reset == true` → `EditOutcome { stack: stack.reset(OpKind::ToneCurve), kind: OpKind::ToneCurve, commit: true }`.
    - else if the returned points are identity → `stack.reset(OpKind::ToneCurve)` (same as today's is_identity path), commit per `edit.commit`.
    - else → `stack.set_op(Op::ToneCurve(ToneCurve { points: edit.points, mode: edit.mode }))`, commit per `edit.commit`.
- [ ] **Step 2:** Remove any now-dead code in `curve_widget.rs` that moved to the reusable widget (the file should now be small — just the adapter). Keep `curve_math` as-is (used by the widget).
- [ ] **Step 3: Run**

Run: `cargo test -p ferrolite-app` and `cargo build -p ferrolite-app`
Expected: compiles; tests pass (curve_math tests unchanged; known-flaky `retain_visible_thumbnail_jobs_cancels_offscreen_only` may need one retry).

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/develop/curve_widget.rs
git commit -m "feat(app): tone curve uses reusable curve_editor; new curves default Smooth"
```

---

## Phase 4 — Final gate

### Task 7: Workspace gate + hold for visual test

- [ ] **Step 1:** `cargo fmt --all`
- [ ] **Step 2:** `cargo clippy --workspace --all-targets -- -D warnings` (fix any)
- [ ] **Step 3:** `cargo test --workspace` (all pass; retry the one known-flaky test once if needed)
- [ ] **Step 4:** Commit any fmt/clippy fixes: `git add -A && git commit -m "chore: fmt + clippy clean for curve spline modes"`
- [ ] **Step 5: STOP** — do not merge. Hold for the author's visual test (combined with the Spec 4.1 modal fixes): verify Linear vs Smooth curve shape matches the applied image, the mode selector + its reset work, new curves start Smooth, a legacy-edited image still loads Linear, and the reusable widget behaves exactly as the Spec 4.1 tone-curve overhaul did.

---

## Self-review (coverage map spec → tasks)

- Spec §3.1 CurveMode + interpolation + public exposure → Tasks 1, 2.
- Spec §3.2 reusable `curve_editor` (migrate interaction, mode-aware rendering) → Task 4; mode selector + per-control reset → Task 5.
- Spec §3.3 tone-curve wiring + new-curve-default-Smooth / legacy-Linear → Task 6 (+ serde default in Task 1).
- Spec §4 testing: serde back-compat → Task 1; interpolation unit tests → Task 2; Smooth golden → Task 3; visual → Task 7 hold.
- Spec §2 tiers/contracts: pipeline-only math (no deps), additive `mode`, per-control reset (Task 5), green gate + hold (Task 7).
