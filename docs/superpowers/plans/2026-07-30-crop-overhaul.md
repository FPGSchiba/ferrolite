# Crop Tool Overhaul — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `docs/superpowers/specs/2026-07-29-crop-overhaul-design.md` — fix the two
root-caused crop rendering bugs, restructure the crop panel per the V2 design (tabs disappear),
add aspect chips, and land manual Keystone V/H as a new pipeline capability (Auto/Guided
buttons ship disabled).

**Architecture:** Bug fixes are localized (crop_math in the app; geometry_uniform + geometry
shader in the pipeline). Keystone extends the `Geometry` op and the single geometry matrix
path (CPU reference in `uniforms.rs` mirrors the WGSL — the crate's established lockstep
pattern). The panel restructure lives in `tool_panel.rs` as a Crop-active branch.

**Tech Stack:** Rust, egui 0.29, wgpu/WGSL, existing `ferrolite-pipeline` graph.

## Global Constraints

- Icons ONLY via `ferrolite-app/src/icons.rs` Phosphor aliases.
- Every new editable control gets a per-control reset (`EguiSlider` reset column / shared
  affordance); keybound controls show `Keymap::hint` in tooltips.
- Sections use `section_header` + their own `Settings` disclosure flags; new flags MUST be
  added to `disclosure_snapshot` in `app.rs` (its count test will fail otherwise — that is
  the reminder mechanism, update the count).
- CPU/GPU lockstep: any WGSL math change mirrors `uniforms.rs`'s CPU reference with a parity
  test (existing pattern; PARITY tolerance conventions per the test suite).
- Golden regeneration ONLY with proven mechanism + documentation (session precedent).
- Old sidecars must load unchanged: new op fields are `#[serde(default)]` with identity
  defaults.
- Scoped gate per task (`cargo fmt -p X -- --check`, `clippy -p X --all-targets -- -D
  warnings`, `test -p X` plus dependents); repo gate by the coordinator at the end.
- Never block the UI thread.

---

### Task 1: Aspect-correct resize math (spec C1)

**Files:**
- Modify: `ferrolite-app/src/develop/crop_math.rs` (`resize`, ~lines 56-116; tests ~154-314)

The bug: `resize` enforces the aspect ratio, then two independent per-axis clamps run AFTER
(`crop_math.rs:106-114`) and silently break the ratio near image edges.

**Interfaces:** `resize`'s signature is unchanged; only its internal constraint order changes.

- [ ] **Step 1: failing tests.** Add boundary ratio tests: for each handle (all 8) and ratios
  {1.0, 1.5, 16/9 adjusted by a non-square sensor factor}, drag the pointer far past each
  edge/corner and assert the result keeps `|(w/h) - target| < 1e-4` AND stays inside `[0,1]²`
  AND respects the min-size floor. Extend `resize_adversarial_does_not_panic` into
  `resize_adversarial_keeps_ratio` (same random sweep, now asserting ratio whenever
  `aspect.is_some()`). Run `cargo test -p ferrolite-app crop_math` — the new tests FAIL.
- [ ] **Step 2: fix.** Restructure `resize`'s constrained path: after the free resize +
  aspect derivation, compute the maximum feasible SCALE of the aspect-true rect that fits
  bounds + min-size, anchored at the drag's fixed corner/edge, and scale both axes together —
  delete the independent per-axis clamps for the aspect path (keep them for free resize).
- [ ] **Step 3:** `cargo test -p ferrolite-app crop_math` green; scoped gate on
  ferrolite-app.
- [ ] **Step 4: Commit** `fix(develop): crop resize keeps the aspect ratio at image edges`

### Task 2: Geometry sampling matrix from rounded output dims (spec C2, part 1)

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs` (`geometry_uniform`, ~lines 371-416; tests
  ~1394-1431)

The bug: output dims are rounded (`out_w = crop_w_px.round()`), but the sampling matrix/offset
derive from the UN-rounded fractional crop rect — the last output row/column samples up to
0.5px outside the crop.

- [ ] **Step 1: failing test.** `geometry_uniform_fractional_crop_stays_in_bounds`: for a set
  of fractional crops (e.g. cx=0.1003, cw=0.4997 on a 4001×2999 source), with and without
  rotation, map every output-corner texel center through `m`/`off` and assert the resulting
  source UV lies within the crop rect (±half a source texel). Run — FAIL.
- [ ] **Step 2: fix.** Derive the matrix/offset from the ROUNDED `out_w`/`out_h` (the
  effective crop extent in source space becomes `out_w_px / src_w` etc.), so output texel
  centers map exactly into the true crop extent. Keep the rotation path consistent.
- [ ] **Step 3:** all geometry tests green; scoped gate on ferrolite-pipeline (+
  `cargo test -p ferrolite-app` for consumers).
- [ ] **Step 4: Commit** `fix(pipeline): geometry sampling matrix derives from rounded output
  dims`

### Task 3: Clamp sampling to the crop sub-rect (spec C2, part 2)

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs` (GeometryUniform struct: add crop-bounds
  vec4; CPU reference apply fn), `ferrolite-pipeline/src/shaders/geometry.wgsl` (~lines
  73-87), `ferrolite-pipeline/src/nodes.rs` (uniform upload if the struct grows)
- Test: parity + an edge-assertion test

The bug: the sampler clamps to the WHOLE source texture, so a rotated crop's out-of-bounds
corners smear the frame edge ("last pixel extruded outward").

**Interfaces:** `GeometryUniform` gains `crop_bounds: [f32; 4]` (min_u, min_v, max_u, max_v in
source-normalized space, inset by half a source texel). WGSL binding layout stays compatible
(append to the uniform struct; keep 16-byte alignment).

- [ ] **Step 1: failing test.** CPU-reference test: rotate a fractional crop so a corner maps
  outside the source; assert the clamped sample coordinate equals the crop-rect edge (NOT the
  source-texture edge). Run — FAIL (no clamp exists).
- [ ] **Step 2: implement.** Clamp `base_uv` to `crop_bounds` in the WGSL immediately before
  `textureSampleLevel`; mirror in the CPU reference; populate `crop_bounds` in
  `geometry_uniform` (full-frame = the half-texel-inset full rect, so no-crop behavior is
  unchanged).
- [ ] **Step 3: edge-assertion GPU test** (in the pipeline's GPU test suite, following the
  existing golden/fixture harness): render a rotated fractional crop of a gradient fixture and
  assert the output's last row/column are NOT duplicates of their neighbors (the artifact's
  signature). If an existing golden shifts, apply the adjudication rule: prove the mechanism
  (this clamp) and document before regenerating.
- [ ] **Step 4:** parity + goldens green; scoped gate on ferrolite-pipeline + ferrolite-app +
  ferrolite-export (consumers).
- [ ] **Step 5: Commit** `fix(pipeline): geometry sampling clamps to the crop rect (kills the
  edge-smear artifact)`

### Task 4: Keystone fields on the Geometry op (spec C4, part 1)

**Files:**
- Modify: `ferrolite-pipeline/src/op.rs` (`Geometry` struct + its tests ~770-863)
- Modify: `ferrolite-app/src/develop/ops_edit.rs` (`needs_full_rebuild` — keystone is a
  geometry-tier change, same as `angle_deg`)

**Interfaces:** `Geometry` gains `#[serde(default)] pub keystone_v: f32` and
`#[serde(default)] pub keystone_h: f32` (identity 0.0, UI range −1..1). Every constructor /
`CropRect::full()` site compiles via `..Default::default()` or explicit zeros — check
`is_identity`/`has_edits` logic includes them.

- [ ] **Step 1: failing tests.** op.rs: serde round-trip with keystone values; an OLD payload
  (JSON without the fields) deserializes with zeros; `is_identity` false when keystone ≠ 0.
  ops_edit: `needs_full_rebuild` fires on a keystone-only change. Run — FAIL.
- [ ] **Step 2: implement** the fields + identity handling + rebuild-key inclusion.
- [ ] **Step 3:** scoped gate on ferrolite-pipeline + ferrolite-app.
- [ ] **Step 4: Commit** `feat(pipeline): Geometry op carries keystone V/H (identity-default,
  old sidecars unchanged)`

### Task 5: Keystone homography in the geometry pass (spec C4, part 2)

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs` (`geometry_uniform`: compose the projective
  warp; CPU reference), `ferrolite-pipeline/src/shaders/geometry.wgsl` (perspective divide)
- Test: CPU/GPU parity fixture + a monotonicity unit test

**Interfaces:** the uniform's 2×3 affine (`m`/`off`) generalizes to a 3×3 homography
(`[f32; 12]` as three padded rows, or a mat3x3 + existing fields — pick the layout that keeps
existing affine behavior bit-identical when keystone == 0). WGSL: `let p = H * vec3(uv, 1.0);
let base_uv = p.xy / p.z;` then the Task-3 crop clamp.

Keystone math (exact): `keystone_v = kv` tilts vertically — top edge scales by `1/(1+|kv|·c)`
when kv > 0 etc. Use the standard unit-square homography: corners displaced
`top_inset = max(kv, 0) * 0.5 * K`, `bottom_inset = max(-kv, 0) * 0.5 * K` horizontally for
keystone_v (and the transpose for keystone_h), with `K = 0.35` (the strength constant — a
full slider throw insets the far edge 17.5% per side; tune only via this named constant).
Solve the 4-point homography from the displaced corners (closed form for this symmetric case
— derive in code with a comment, no general DLT needed).

- [ ] **Step 1: failing tests.** (a) keystone == 0 reproduces the current affine mapping
  bit-identically for a grid of UVs (guards every existing golden); (b) kv > 0 maps the top
  edge's sampled span WIDER than the bottom's (converging verticals corrected) — assert the
  sign convention explicitly; (c) CPU/GPU parity on a fixture with kv=0.5, kh=−0.3 + crop +
  rotation within the suite's tolerance.
- [ ] **Step 2: implement** uniform layout + WGSL divide + CPU mirror. Zero-keystone path must
  not change any golden (test (a) is the proof; if a golden still drifts, STOP and
  investigate — that is a layout/precision bug, not an inherent-precision case).
- [ ] **Step 3:** scoped gate on ferrolite-pipeline + ferrolite-app + ferrolite-export.
- [ ] **Step 4: Commit** `feat(pipeline): manual keystone V/H as a homography in the geometry
  pass`

### Task 6: Dedicated crop panel — tabs disappear (spec C3)

**Files:**
- Modify: `ferrolite-app/src/develop/tool_panel.rs` (~lines 84-97: `tab_items` assembly)
- Modify: `ferrolite-app/src/develop/tools/crop.rs` (`CropTab` becomes the dedicated panel
  content: CROP & TRANSFORM + GEOMETRY sections)
- Modify: `ferrolite-app/src/settings/dto.rs` (+ `disclosure_snapshot` in app.rs): flags
  `crop_transform_open`, `crop_geometry_open` (default true)
- Test: `tool_panel` routing test + panel content tests

**Interfaces:** when `ts.active == ToolId::Crop`, `tool_panel` renders NO base tabs — only the
dedicated panel (sibling branch to the Mask header injection at ~line 38, but REPLACING the
tab row). Contents per spec/V2 README:69:

- **CROP & TRANSFORM** (section_header + `crop_transform_open`): Angle slider (existing, with
  per-control reset) · Aspect ComboBox ("Original" + existing presets) · a wrapping chip row —
  Original / 1:1 / 4:3 / 3:2 / 16:9 / 5:4 / Custom (reuse `widgets::chips` styling; selected
  chip accent-tinted; chips and combo write the SAME aspect state; "Custom" is selected-state
  only, shown when the current aspect matches no preset) · "Reset crop" button (existing).
- **GEOMETRY** (section_header + `crop_geometry_open`): Keystone V and Keystone H
  `EguiSlider`s (−1..1, step 0.01, default 0, per-control reset; `OpKind::Geometry` edits,
  commit-on-release like Angle) · "Auto Perspective" and "Guided Upright" buttons rendered
  disabled with hover reason "Coming with automatic perspective analysis" · the sliders write
  `Geometry.keystone_v/h` through the same edit path Angle uses.

- [ ] **Step 1: failing test.** `tool_panel` test: with Crop active, the rendered tab set
  contains NO Light/Color/Effects items (assert via the same seam existing tab tests use);
  with Adjust/Mask active, unchanged. A settings test: the two new flags exist, default true,
  and `disclosure_snapshot`'s count test is updated (it fails until you bump it — that's
  expected RED).
- [ ] **Step 2: implement** the branch + panel. Keep `CropTool`'s canvas overlay behavior
  untouched.
- [ ] **Step 3:** scoped gate on ferrolite-app.
- [ ] **Step 4: Commit** `feat(develop): dedicated crop panel replaces the tabs (aspect chips,
  keystone, disabled auto-upright)`

### Task 7: Leaving crop feels safe (spec C5)

**Files:**
- Modify: `ferrolite-app/src/develop/crop_overlay.rs` (Escape-cancels-drag)
- Test: crop_overlay / tool-switch tests

- [ ] **Step 1: failing test.** Pure-logic test for the new drag-cancel: mid-drag state +
  Escape ⇒ the pre-drag rect is restored and NO EditOutcome is emitted that frame. (Structure
  the drag state so this is testable — e.g. store `drag_origin_rect` when a handle drag
  starts.)
- [ ] **Step 2: implement.** Escape while dragging a handle cancels that drag (restore
  `drag_origin_rect`, emit a non-committed preview restore). Verify (by reading the
  tool-switch path) that exiting the Crop tool commits nothing by itself — add a regression
  test asserting tool-switch emits no EditOutcome from crop code; document the finding in the
  test comment.
- [ ] **Step 3:** scoped gate on ferrolite-app.
- [ ] **Step 4: Commit** `feat(develop): Escape cancels a crop handle drag; exiting crop
  commits nothing`

### Task 8: Export renders keystone (spec C4 verification)

**Files:**
- Test: `ferrolite-export` (one integration test following its existing render-test pattern)

- [ ] **Step 1:** Test: export a small fixture with keystone_v=0.5 + a crop; assert the output
  differs from the keystone-0 export AND matches the pipeline's CPU reference mapping at a few
  probe pixels (tolerance per the crate's existing tests). If ferrolite-export shares the
  geometry uniform end-to-end (expected), this is verification only — if it has a separate
  geometry path that ignores keystone, STOP and report BLOCKED with the divergence.
- [ ] **Step 2:** scoped gate on ferrolite-export.
- [ ] **Step 3: Commit** `test(export): keystone renders through the export path`

---

## Post-plan (coordinator)

- Repo gate on latest stable; author visual test checklist (crop feel: aspect chips, keystone
  behavior/strength constant K, edge artifacts gone at fractional crops + rotation, no-tabs
  panel, Escape-cancel).
- Fold accepted changes into `docs/design/V2/README.md`.
