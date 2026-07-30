# Unified Maskable Adjustments — Phase 2b: Per-Mask Tone Curve, HSL & Color Grading

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The three "Phase 2b" hints in Mask scope un-grey: tone curve, HSL, and color grading apply per-mask through the existing `local-adjust` layer pass, edited by the same widgets the global scope uses.

**Architecture:** `LocalAdjustmentsNode` is the single per-mask apply point (preview `EditPipeline`, tiled `TileEditPipeline`, and export all drive the same node), so the GPU work lands once: the per-layer uniform grows HSL-band and grade parameters, a per-layer 3×256 tone-curve LUT buffer joins the bind group (mirroring the global `CurveNode`'s binding), and the WGSL `adjust()` appends curve → HSL → grade between the existing hue step and the color-swatch overlay. The CPU reference `light_color_apply` (which the shader mirrors and the parity test composes against) is extended in lockstep. UI-side, `curve_widget`/`hsl_widget`/`grade_widget` re-scope from `&OpStack` to `&ScopedEdit`, so both scopes share one widget implementation — the same move Phase 2a made for sliders.

**Tech Stack:** Rust, wgpu/WGSL compute, egui; existing shared pure transforms `tone_curve_luts`, `color_grade_px` (public since P1 precisely so the per-mask path reuses them); Phase 2a's `ScopedEdit`/`EditScope`.

**Spec:** `docs/superpowers/specs/2026-07-28-unified-maskable-adjustments-design.md` §4 (phased shader wiring). Out of scope: vibrance (no shader anywhere — Phase 3), per-mask sharpen/NR/dehaze (neighborhood — Phase 4), global H/S/W/B (Phase 3).

## Global Constraints

- Branch: `feat/ui-v2-rewrite`. Never commit to `main`.
- **Behavior preservation for existing output:** all new per-layer parameters are zero-identity; a layer with default curve/HSL/grade must produce BIT-IDENTICAL output to today's pass (existing masked-edit renders, goldens, and the export suite must not change). The existing Light+Color math in `local_adjust.wgsl` `adjust()` — including its op order — is untouched; new steps are appended after the hue step, before the color-swatch overlay.
- **CPU/GPU lockstep:** `uniforms::light_color_apply` (the CPU reference) and `local_adjust.wgsl` must implement the same math; the existing parity composition test in `local_node.rs` is the enforcement point and must be extended, not bypassed.
- The WGSL band/curve/grade math must be PORTED from the existing global shaders (`shaders/hsl.wgsl`, `shaders/curve.wgsl`, `shaders/color_grade.wgsl`) — same formulas, not re-derived. `color_grade_px`/`tone_curve_luts` stay the single CPU-side sources of truth.
- UI: one widget implementation per control, scoped via `ScopedEdit` (Phase 2a pattern); per-control reset stays intact inside each widget; mask-scope disclosure uses the existing `mask_tone_curve_open`/`mask_color_hsl_open`/`mask_color_grading_open` flags (already persisted).
- Mask-scope edits keep emitting `kind = OpKind::LocalAdjustments` (ScopedEdit coerces — undo sealing depends on it).
- Subagents run the **scoped gate** named per task; coordinator runs the repo gate at the end.

---

### Task 1: Model + CPU reference — extended per-layer uniform data and `light_color_apply`

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs` (`LocalAdjustUniform`, `local_adjust_uniform`, `light_color_apply`)
- Modify: `ferrolite-pipeline/src/op.rs` (`set_op` normalization consolidation — clears two parked ledger items)

**Interfaces:**
- Consumes: `AdjustmentSet.{tone_curve, hsl, color_grade}` (Phase 1 fields), `tone_curve_luts(&ToneCurve) -> [[f32; 256]; 3]`, `color_grade_px(rgb, &ColorGradeUniform) -> [f32; 3]` (verify exact existing signatures in `uniforms.rs`/`lib.rs` and use them verbatim), `hsl_uniform(Option<Hsl>) -> HslUniform`, `AdjustmentSet::normalized()`.
- Produces (Task 2 relies on these exact shapes):
  - `LocalAdjustUniform` grows, appended AFTER `contrast_pivot` (the existing 64-byte prefix is untouched — WGSL field offsets must stay stable):

```rust
    // ── Phase 2b: per-layer curve/HSL/grade (identity when the layer leaves them default) ──
    /// 8 bands × (hue, sat, lum, pad) — same packing as the global `HslUniform`.
    pub hsl_bands: [[f32; 4]; 8],
    /// Same packing as the global `ColorGradeUniform` (shadows/midtones/highlights/global/params).
    pub grade_shadows: [f32; 4],
    pub grade_midtones: [f32; 4],
    pub grade_highlights: [f32; 4],
    pub grade_global: [f32; 4],
    pub grade_params: [f32; 4],
    /// x = curve active (LUT differs from the linear ramp), y = hsl active,
    /// z = grade active, w = pad. Skip flags so identity layers pay no extra math.
    pub active_flags: [f32; 4],
```

  - `local_adjust_uniform(&AdjustmentSet) -> LocalAdjustUniform` fills them: `hsl_bands` via the same packing `hsl_uniform` uses (reuse it: `hsl_uniform(Some(a.hsl)).bands`); grade fields via `color_grade_uniform(Some(a.color_grade))`'s fields (reuse the existing packer — it is `pub` or make it `pub(crate)` if needed); flags from `!a.tone_curve.is_identity()` / hsl / `!a.color_grade.is_identity()` as 1.0/0.0.
  - `pub fn local_layer_lut(a: &AdjustmentSet) -> [[f32; 256]; 3]` — thin wrapper over `tone_curve_luts(&a.tone_curve)` so Task 2's node has one named entry point.
  - `light_color_apply(rgb, a)` (CPU reference, test-only) extended after its existing hue step and before the color-swatch step, mirroring what Task 2's WGSL will do:
    1. tone curve: `let luts = tone_curve_luts(&a.tone_curve);` then per channel `c[i] = sample_lut(&luts[i], c[i])` where `sample_lut` linearly interpolates the 256-entry LUT over [0,1] input (clamped) — EXACTLY the sampling the global curve pass applies (read `shaders/curve.wgsl` and mirror its indexing/interp; if a CPU-side equivalent already exists in `uniforms.rs` tests, reuse it).
    2. HSL bands: port the band-weight + hue/sat/lum application from `shaders/hsl.wgsl` to CPU (same constants, same weight function). Skip entirely when `a.hsl.is_identity()`.
    3. grade: `c = color_grade_px(c, &color_grade_uniform(Some(a.color_grade)))` (already pure + shared). Skip when identity.

- `set_op` consolidation (clears the parked "set_op arms could reuse normalized" + Phase-1 "Hsl -0.0" items): replace the per-arm inline identity checks with a single tail — after the match writes raw params, run `d.global = d.global.normalized();` and normalize layer sets in the `Op::LocalAdjustments` arm (`la.layers` each `layer.adjustments = layer.adjustments.normalized()`). Delete the now-redundant per-arm `if x.is_identity()` branches. All existing op.rs tests must still pass unchanged (they assert outcomes, not mechanism); the `identity_valued_set_op_is_byte_equal_to_default` test now also holds for `Op::Hsl` with `-0.0` — extend that test with the `-0.0` HSL case (serde-string comparison, mirroring its ColorGrade sub-case).

- [ ] **Step 1: Write the failing tests** (in `uniforms.rs`'s test module)

```rust
#[test]
fn extended_local_uniform_is_identity_safe() {
    let a = AdjustmentSet::default();
    let u = local_adjust_uniform(&a);
    assert_eq!(u.active_flags, [0.0; 4]);
    assert_eq!(u.hsl_bands, [[0.0; 4]; 8]);
    // Identity LUT is the linear ramp.
    let luts = local_layer_lut(&a);
    assert!((luts[0][0] - 0.0).abs() < 1e-6);
    assert!((luts[0][255] - 1.0).abs() < 1e-6);
}

#[test]
fn cpu_reference_applies_curve_hsl_grade() {
    // Curve: a strong lift must brighten the reference output.
    let mut a = AdjustmentSet::default();
    a.tone_curve.points = vec![(0.0, 0.3), (1.0, 1.0)];
    let lifted = light_color_apply([0.2, 0.2, 0.2], &a);
    let base = light_color_apply([0.2, 0.2, 0.2], &AdjustmentSet::default());
    assert!(lifted[0] > base[0], "curve lift raises output");

    // Grade: a saturated shadows tint must move channel balance.
    let mut a = AdjustmentSet::default();
    a.color_grade.shadows = crate::op::GradeWheel {
        hue: 210.0,
        sat: 0.5,
        lum: 0.0,
    };
    let graded = light_color_apply([0.1, 0.1, 0.1], &a);
    assert_ne!(graded, [0.1, 0.1, 0.1]);

    // Identity set stays a pure pass-through (bit-stable vs the old reference).
    let id = light_color_apply([0.4, 0.5, 0.6], &AdjustmentSet::default());
    let old = {
        // exposure-only path unchanged: 0 EV ⇒ input unchanged through the whole chain
        [0.4, 0.5, 0.6]
    };
    assert_eq!(id, old, "identity extension is a no-op");
}
```

(Use a real `GradeWheel { hue: 210.0, sat: 0.5, lum: 0.0 }` literal — the placeholder call above only marks where it goes.)

- [ ] **Step 2: Run to verify failure** — `cargo test -p ferrolite-pipeline extended_local cpu_reference` → FAIL (fields/fn missing).
- [ ] **Step 3: Implement** per the Produces block. Read `shaders/hsl.wgsl` and `shaders/curve.wgsl` FIRST and port their math faithfully (constants included).
- [ ] **Step 4: Run** `cargo test -p ferrolite-pipeline` → PASS (all existing uniform/op tests green — esp. the untouched-prefix expectation: any test asserting `LocalAdjustUniform` size/layout must be updated deliberately, never deleted).
- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-pipeline -- --check
cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings
cargo test -p ferrolite-pipeline
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/op.rs
git commit -m "feat(pipeline): per-layer curve/HSL/grade uniform data + CPU reference; set_op normalization consolidated"
```

---

### Task 2: GPU — extended `local_adjust.wgsl` + per-layer LUT binding in `LocalAdjustmentsNode`

**Files:**
- Modify: `ferrolite-pipeline/src/shaders/local_adjust.wgsl`
- Modify: `ferrolite-pipeline/src/local_node.rs` (bind-group layout + per-layer LUT buffer + uniform upload)
- Modify: `ferrolite-pipeline/src/lib.rs` ONLY if `prewarm_shaders` embeds shader sources that need no change (it includes the same file — verify nothing else references the WGSL binding count)

**Interfaces:**
- Consumes: Task 1's uniform fields + `local_layer_lut`; the global `CurveNode`'s LUT binding as the pattern to mirror (`nodes.rs:645-760` — buffer type, WGSL-side array declaration from `shaders/curve.wgsl`).
- Produces: the per-mask pass applies curve → HSL → grade; preview, tiled, and export tiers all pick it up (single node). No public API change.

- [ ] **Step 1: WGSL.** Extend `struct P` with the Task 1 fields (same order — WGSL offsets must match the Rust struct; `array<vec4<f32>, 8>` for bands, five `vec4<f32>` for grade, one `vec4<f32>` flags). Add `@group(0) @binding(4)` for the 3×256 LUT using the SAME buffer type + WGSL declaration style as `curve.wgsl`'s LUT binding. In `adjust()`, after the existing hue block and BEFORE the color-swatch block, append:

```wgsl
    // Phase 2b: per-layer tone curve (LUT), HSL bands, color grade — ported from
    // the global curve/hsl/color_grade passes; identity-skipped via flags.
    if (p.active_flags.x != 0.0) { c = curve_sample(c); }
    if (p.active_flags.y != 0.0) { c = hsl_bands_apply(c); }
    if (p.active_flags.z != 0.0) { c = grade_apply(c); }
```

with `curve_sample`/`hsl_bands_apply`/`grade_apply` ported verbatim (same math, same constants, same clamping) from `curve.wgsl`/`hsl.wgsl`/`color_grade.wgsl`.

- [ ] **Step 2: Node.** In `local_node.rs`: add the LUT binding to the bind-group layout; per layer in `apply()`, build the LUT via `crate::uniforms::local_layer_lut(&l.adjustments)` and upload it with the same `create_buffer_init` pattern the per-layer uniform already uses (a fresh small buffer per dispatch matches the existing style; do NOT invent caching here — the mask-def cache handles the expensive part and layer counts are small).
- [ ] **Step 3: Extend the parity test.** `local_node.rs`'s CPU-composition parity test (the one composing via `light_color_apply`, ~line 438-491) gains a layer whose adjustments set a non-identity curve (points `[(0.0,0.2),(1.0,1.0)]`), one HSL band (`bands[0].sat = 0.4`), and a grade (`shadows = GradeWheel { hue: 210.0, sat: 0.5, lum: 0.0 }`) — CPU vs GPU output must agree within the test's existing tolerance. Also add an identity-extension guard: a layer with ONLY the old Light+Color fields set must produce the same output as before the change (assert against `light_color_apply`, which Task 1 kept bit-stable for identity curve/hsl/grade).
- [ ] **Step 4: Run** `cargo test -p ferrolite-pipeline` (GPU tests included) → PASS. Then `cargo test -p ferrolite-export` → PASS (goldens unchanged — identity layers bit-identical).
- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-pipeline -- --check
cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings
cargo test -p ferrolite-pipeline
cargo test -p ferrolite-export
git add ferrolite-pipeline/src/shaders/local_adjust.wgsl ferrolite-pipeline/src/local_node.rs
git commit -m "feat(pipeline): per-mask tone curve/HSL/grade in the local-adjust pass (preview+tiled+export)"
```

---

### Task 3: UI — scoped widgets; the three mask-scope hints un-grey

**Files:**
- Modify: `ferrolite-app/src/develop/curve_widget.rs`, `hsl_widget.rs`, `grade_widget.rs` (signatures: `&OpStack` → `&ScopedEdit`)
- Modify: `ferrolite-app/src/develop/base_tabs.rs` (LightTab Tone Curve + ColorTab HSL/Grading sections render the widgets in BOTH scopes)
- Modify: `ferrolite-app/src/develop/ops_edit.rs` (remove `set_tone_curve`/`set_color_grade` IF zero callers remain — grep first; `hsl_widget` uses raw `set_op`, which routes through the Task 1 normalization)

**Interfaces:**
- Consumes: Phase 2a's `ScopedEdit` (`.set() -> Option<&AdjustmentSet>`, `.write(new_set, kind, commit)`), the per-scope disclosure flags (already wired), `theme::TEXT_FAINT` hint style for the `MaskNone` case.
- Produces: `curve_widget::show(ui: &mut egui::Ui, scoped: &ScopedEdit) -> Option<EditOutcome>`; `hsl_widget::show(ui, scoped: &ScopedEdit, band: &mut usize) -> Option<EditOutcome>`; `grade_widget::show(ui, scoped: &ScopedEdit) -> Option<EditOutcome>`.

Widget conversion pattern (apply to each of the three):
1. Read: replace `stack.tone_curve()`/`stack.hsl()`/`stack.color_grade()` with `let Some(set) = scoped.set() else { render the faint "Create or select a mask first" hint and return None; };` then the field (`set.tone_curve.clone()` / `set.hsl` / `set.color_grade`) — note the field is ALWAYS present (no Option): the widget's "seed defaults when op absent" branches collapse to just using the field value.
2. Write: replace `set_tone_curve(stack, new)`/`stack.set_op(...)`/`set_color_grade(stack, new)` with `let mut new_set = set.clone(); new_set.tone_curve = new; return scoped.write(new_set, OpKind::ToneCurve, commit);` (HSL → `OpKind::Hsl`, grade → `OpKind::ColorGrade`; ScopedEdit coerces to LocalAdjustments in mask scope). Identity-eliding is automatic (`with_global`/`with_layer_adjustments` normalize).
3. Internal widget UI state (drag points, selected band) is untouched.
4. Per-control reset affordances inside the widgets stay exactly as they are — they now reset the SCOPED value (write identity through the same path).

`base_tabs.rs`: in the Tone Curve / COLOR (HSL) / COLOR GRADING sections, delete the `EditScope::Mask(_) | EditScope::MaskNone` hint arms and call the widget with the already-constructed `scoped` for ALL scopes (the widget itself handles `MaskNone` via `scoped.set() == None`). Global call sites change from `curve_widget::show(ui, &stack)` to `curve_widget::show(ui, &scoped)` etc.

- [ ] **Step 1: Write the failing test** (in `base_tabs.rs` or a widget test module — mirror the existing real-viewer fixture from `mask_scope_uses_its_own_section_flags`)

```rust
#[test]
fn scoped_curve_write_lands_in_the_selected_mask() {
    // Build a doc with one mask; a Mask(0)-scoped write of a curve must land in
    // layers[0].adjustments.tone_curve and leave the global curve untouched.
    use crate::develop::scope::{EditScope, ScopedEdit};
    use ferrolite_pipeline::{OpKind, OpStack};
    let doc = crate::develop::mask_edit::create_mask(&OpStack::default(), "M".into());
    let scoped = ScopedEdit::new(EditScope::Mask(0), &doc);
    let mut new_set = scoped.set().unwrap().clone();
    new_set.tone_curve.points = vec![(0.0, 0.2), (1.0, 1.0)];
    let out = scoped.write(new_set, OpKind::ToneCurve, true).unwrap();
    assert_eq!(out.kind, OpKind::LocalAdjustments);
    assert!(!out.stack.layers[0].adjustments.tone_curve.is_identity());
    assert!(out.stack.global.tone_curve.is_identity());
}
```

- [ ] **Step 2: Run to verify failure** — it PASSES already at the scope level (Phase 2a machinery) — so instead this test is the REGRESSION anchor; the failing part of this task is compilation: convert one widget (`curve_widget`) first and watch `cargo check -p ferrolite-app` drive out every call site.
- [ ] **Step 3: Convert all three widgets + base_tabs call sites** per the pattern. Grep + remove dead `ops_edit` setters (with their tests) only at zero callers.
- [ ] **Step 4: Run** `cargo test -p ferrolite-app` (full crate) → PASS.
- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app
git add -u ferrolite-app/src
git commit -m "feat(develop): tone curve/HSL/grading widgets are scoped — live per-mask, one implementation"
```

---

## Coordinator wrap-up (not a subagent task)

1. `rustup update stable`, full repo gate.
2. Visual test plan for the author:
   - **Per-mask curve:** select a mask → Light tab → Tone Curve now shows the real curve editor (no hint). Drag a point up → only the masked region brightens; the global Tone Curve (Adjust mode) is unchanged. Per-point delete + Reset work; undo steps one gesture at a time.
   - **Per-mask HSL:** Color tab in Mask scope → band swatches + sliders live; drag a band's Sat → only masked pixels of that hue family shift. Band selection is shared between scopes (deliberate — flag if it feels wrong).
   - **Per-mask grading:** wheels live in Mask scope; a shadows tint applies only inside the mask; Blending/Balance behave as globally.
   - **Identity guard:** an existing masked edit (Light+Color only) renders EXACTLY as before this phase — compare against memory/screenshot; exported JPEG of an old edit unchanged.
   - **Scope isolation:** set a global curve AND a different mask curve on the same image — both apply (global first, then masked on top); resetting the mask curve leaves the global one.
   - **MaskNone:** Mask tool, no selection → the three sections show the faint "create or select" hint.
   - **Perf feel:** dragging a mask curve point should feel like dragging a mask exposure slider (same single-pass path) — flag any new sluggishness at fit and 1:1.
3. Wait for the author's verdict; then Phase 3 (fused layer engine — the perf phase) or Phase 4 (per-mask neighborhood) per the spec's sequencing, author's choice.
