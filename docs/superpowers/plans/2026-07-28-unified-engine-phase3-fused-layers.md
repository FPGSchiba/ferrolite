# Unified Maskable Adjustments — Phase 3: Fused Layer Engine (two segments)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse the six global point-op passes into two fused engine passes (plus one per mask layer), prove output parity against committed goldens, prove the perf win against committed baselines, and un-grey the remaining "Phase 3" controls (global H/S/W/B, Saturation/Hue, color swatch, and Vibrance in both scopes).

**Architecture:** Both pipelines wire `…→ contrast → dehaze-recovery → tone-curve → hsl → color-grade → local-adjust → sharpen →…` (preview: `pipeline.rs:126-233`; tile: `tile_edit.rs:200-284` — identical shape). Dehaze splits the point-op run, so the engine lands as TWO instances of ONE node/shader:
- **Light engine** (at exposure's position, before dehaze): one pass applying the GLOBAL set's light segment — exposure → region(H/S/W/B) → WB → contrast — coverage ≡ 1.
- **Color engine** (at the tone-curve…local-adjust position): one pass per layer — first a global pseudo-layer applying the color segment (saturation → hue → vibrance → curve → HSL → grade → swatch), then each mask layer applying its FULL set exactly as today's local-adjust does.

The shader is `local_adjust.wgsl` grown with: a stage/coverage flag (global layers skip the mask fetch), a WB↔contrast **order flag** (global order is WB→contrast, the mask order is contrast→WB — they don't commute; each path keeps its historical order for parity), and Vibrance (new math, both scopes). The CPU reference `light_color_apply` grows in lockstep. Point-op count for a global-only edit drops 6 → 2; each mask stays one pass.

**Why parity is tolerance-based:** today every pass round-trips through an `rgba16float` texture; fusing removes intermediate quantization inside each segment, so outputs shift by ≲1 f16 LSB. Goldens compare within an explicit tolerance, never bit-exact.

**Tech Stack:** Rust, wgpu/WGSL, headless-GPU integration tests (pattern: `local_node.rs` tests), committed golden renders + benchmark baselines.

**Spec:** `docs/superpowers/specs/2026-07-28-unified-maskable-adjustments-design.md` §4 (engine), §6.1 (golden gate), §6.5 (perf gate: **no regression anywhere; ≥2× on the early-op drag case**).

## Global Constraints

- Branch: `feat/ui-v2-rewrite`. Never commit to `main`.
- **Golden gate:** Task 1's goldens are rendered by the CURRENT chain and committed BEFORE any engine code lands; Tasks 2-3 must reproduce them within `PARITY_TOL` (define once; start at `2e-3` absolute per channel in display-linear f32 — tighten if the diffs allow). Goldens are never regenerated to make a failure pass; a failure means the engine is wrong.
- **Order preservation:** global segment order exposure→region→WB→contrast and (color) sat→hue→vibrance→curve→HSL→grade→swatch; mask layers keep the EXACT existing local order (exposure→region→contrast→WB→sat→hue→curve→HSL→grade→swatch) + vibrance slotted after hue. The WB↔contrast divergence is deliberate and flag-selected (parity for both paths); document it where the flag is defined.
- **Node boundaries preserved:** dehaze-recovery keeps consuming the post-(light-segment) image; sharpen keeps consuming the color engine's output; the dehaze transmission dirty-routing in `set_stack` (amount-only changes must NOT dirty the transmission node) is behavior-preserved.
- **CPU/GPU lockstep:** every new shader step exists in `light_color_apply` first; the node-level parity tests enforce agreement.
- **Perf gate is an acceptance criterion:** Task 1 records baselines; Task 5 records afters; the plan FAILS (report BLOCKED, do not rationalize) if any measured case regresses beyond noise or the early-op case is <2×.
- Vibrance math (new, both scopes; define once, CPU + WGSL identical): in the shader's HSL space, `s' = s * (1.0 + vibrance * (1.0 - s))` with `s'` clamped to [0,1] — a saturation boost that fades as pixels approach full saturation; negative vibrance desaturates with the same weighting. Zero = identity.
- Export goldens: if `cargo test -p ferrolite-export` fails ONLY on ≲`PARITY_TOL`-scale diffs from removed f16 round-trips, widen that suite's tolerance to `PARITY_TOL` in ONE reviewed change with a comment citing this plan — never regenerate reference images silently.
- Subagents run the scoped gate named per task; coordinator runs the repo gate + owns the author's visual test plan.

---

### Task 1: Parity goldens + benchmark baselines (rendered by the CURRENT chain — must land before any engine code)

**Files:**
- Create: `ferrolite-pipeline/tests/layer_engine_parity.rs`
- Create: `ferrolite-pipeline/tests/golden/layer_engine/*.png` (committed reference renders)
- Create: `ferrolite-pipeline/tests/engine_bench.rs` (`#[ignore]`d timing harness, run explicitly)
- Create: `docs/benchmarks/2026-07-28-phase3-fused-engine.md` (baseline numbers; Task 5 appends afters)

**Interfaces:**
- Consumes: `EditPipeline::new/set_stack/evaluate` + the headless-GPU + readback patterns from `local_node.rs`'s tests (read them first and reuse their helpers' style; skip-if-no-adapter behavior included).
- Produces: `fn fixture_docs() -> Vec<(&'static str, OpStack)>` (name → doc) — the shared fixture set both the parity test and Task 3 consume; `const PARITY_TOL: f32 = 2e-3;` exported from the test file (Tasks 2-3 reference the same value — put it in a small `tests/common/` module if the harness splits).

**Fixture docs** (each also has a committed golden named after it; source = a deterministic synthetic image, e.g. a 512×512 HSV sweep gradient generated in-test — NO binary source asset):
1. `identity` — default doc.
2. `light_trio` — exposure +0.8 EV, contrast +0.35, temp +0.4/tint −0.2 (exercises the WB↔contrast order).
3. `curve_hsl_grade` — tone curve [(0,0.1),(0.5,0.55),(1,1)], HSL band 0 sat +0.4 / band 3 hue −0.3, grade shadows {hue:210, sat:0.5} + blending 0.7.
4. `full_global` — all of 2+3 combined + sharpen 0.8/r2 + dehaze 0.3/r8.
5. `one_mask` — global light_trio + one mask layer (full-coverage brush def or default mask covering all — reuse whatever `local_node.rs`'s tests construct) with exposure −1.0, contrast +0.3, temp −0.3, curve lift, HSL band sat, grade shadows.
6. `two_masks` — 5 plus a second layer with saturation +0.5, hue +0.2, swatch amount 0.4 rgb(1,0,0).
7. `mask_only` — identity global + one masked layer (isolates the local path).
8. `wb_contrast_both` — ONLY temp +0.5 and contrast +0.5 (the order-sensitivity sentinel: this golden CHANGES if anyone unifies the order later).

- [ ] **Step 1:** Write the parity test: for each fixture, build `EditPipeline` on the synthetic source, `set_stack(doc)`, evaluate, read back f32 pixels, and (a) on first run with env `UPDATE_GOLDENS=1`, write the 16-bit PNG golden; (b) normally, compare against the committed golden within `PARITY_TOL`, reporting max-diff + offending fixture on failure. Skip cleanly when no GPU adapter exists (same pattern as local_node tests).
- [ ] **Step 2:** Generate + commit the goldens (run once with `UPDATE_GOLDENS=1`), then run the test normally → PASS (self-consistency, current chain vs its own goldens).
- [ ] **Step 3:** Write `engine_bench.rs` (`#[ignore]`): on a synthetic 6000×4000 source, build the pipeline with `full_global`'s doc, evaluate once (warm), then time N=20 iterations of each: (a) exposure-dirty evaluate (`set_stack` with ev alternating ±0.01), (b) grade-dirty evaluate (grade lum alternating), (c) same as (a) with `two_masks`' layers added. Print median ms per case. Run it; record the three medians in `docs/benchmarks/2026-07-28-phase3-fused-engine.md` under "Baseline (pre-fusion, commit <hash>)" with the GPU name.
- [ ] **Step 4: Scoped gate + commit** (`cargo fmt -p ferrolite-pipeline -- --check && cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings && cargo test -p ferrolite-pipeline`), commit tests + goldens + baseline doc: `test(pipeline): layer-engine parity goldens + perf baselines (pre-fusion)`.

---

### Task 2: The engine node — stage split, order flag, coverage flag, vibrance

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs` (stage split + order/coverage flags + vibrance in `LocalAdjustUniform`/`local_adjust_uniform`/`light_color_apply`)
- Modify: `ferrolite-pipeline/src/shaders/local_adjust.wgsl` (order flag, coverage flag, vibrance)
- Modify: `ferrolite-pipeline/src/local_node.rs` (node gains `EngineStage` + the prepended global pseudo-layer)
- Modify: `ferrolite-pipeline/src/local.rs` (`AdjustmentSet::light_segment()` / `color_segment()`)
- Modify: `ferrolite-pipeline/src/lib.rs` (export what Task 3 needs)

**Interfaces:**
- Consumes: Phase 2b's extended uniform/LUT plumbing; `AdjustmentSet` (add nothing to it — vibrance field exists since Phase 1).
- Produces (Task 3 relies on these exact names):
  - `pub enum EngineStage { Light, Color }` (in `local_node.rs` or `local.rs` — implementer's call, export it).
  - `AdjustmentSet::light_segment(&self) -> AdjustmentSet` — copy with ONLY exposure/highlights/shadows/whites/blacks/temp/tint/contrast kept, everything else identity. `color_segment(&self) -> AdjustmentSet` — the complement (saturation, hue, vibrance, color swatch, tone_curve, hsl, color_grade kept). Unit-tested: `light_segment` ⊕ `color_segment` covers every field exactly once (a test iterates a fully-populated set and asserts each field appears in exactly one segment; sharpen/dehaze/NR/texture/clarity belong to NEITHER — assert they are identity in both).
  - `LocalAdjustUniform` gains, appended: `order_and_coverage: [f32; 4]` — x: 1.0 = global order (WB before contrast), 0.0 = mask order (contrast before WB); y: 1.0 = force full coverage (skip mask sample); z: vibrance amount; w: pad. (Vibrance rides in this vec4 — no second struct growth.)
  - `local_adjust_uniform(a, global_order: bool, full_coverage: bool)` — signature grows two flags (update the two existing call sites mechanically).
  - `light_color_apply(rgb, a, global_order: bool)` — CPU reference grows the order flag + vibrance step (after hue, before curve). WGSL mirrors: order flag swaps the WB/contrast application order; coverage flag makes `m = 1.0`; vibrance per the Global Constraints formula applied in the existing rgb2hsl/hsl2rgb space (reuse those fns — CPU port included).
  - `LocalAdjustmentsNode::new(ctx, layers, stage: EngineStage, global_set: Rc<RefCell<AdjustmentSet>>)` — or an equivalent constructor extension: the node now ALSO holds the global set. `evaluate` for `Stage::Light`: exactly one dispatch — `global_set.light_segment()`, global order, full coverage (no mask compositing at all). For `Stage::Color`: dispatch 1 = `global_set.color_segment()`, global order, full coverage; then the existing per-mask-layer dispatches unchanged (full sets, mask order, composited masks). Identity-set dispatches are SKIPPED entirely (a `is_identity()` check per pseudo-layer — a default global set must add zero passes, keeping `mask_only` parity trivially).
- [ ] **Step 1 (failing tests first):** in `uniforms.rs` tests — `light_and_color_segments_partition_the_set` (as described above); `cpu_reference_order_flag_swaps_wb_contrast` (with temp 0.5 + contrast 0.5 both set, `light_color_apply(c, &a, true) != light_color_apply(c, &a, false)`, and the `true` variant equals applying `c*mul` then pivot-contrast manually); `vibrance_boosts_low_sat_more_than_high_sat` (construct one low-sat and one high-sat rgb, vibrance 0.5: relative saturation gain of the low-sat pixel is strictly larger; vibrance 0 is identity — assert bit-equality).
- [ ] **Step 2:** run → fail; implement; run → pass.
- [ ] **Step 3:** node-level GPU tests (extend `local_node.rs`'s test module): a Light-stage node with the `light_trio` params matches CPU reference with global order; a Color-stage node with a color-segment global set + one mask layer matches the CPU composition (global color first, then layer, correct orders); a default global set adds no dispatch (assert via the existing rebuild-count/test hooks or output equality with the plain input).
- [ ] **Step 4: Scoped gate + commit** (`fmt`/`clippy`/`test -p ferrolite-pipeline`): `feat(pipeline): layer engine node — stage split, order+coverage flags, vibrance`.

---

### Task 3: The swap — both pipelines run the two-segment engine

**Files:**
- Modify: `ferrolite-pipeline/src/pipeline.rs` (delete exposure/wb/contrast/tone-curve/hsl/color-grade nodes + their Cells; Light engine at `vec![vignette_id]`, dehaze-recovery consumes it; Color engine replaces tone-curve…local-adjust, sharpen consumes it; `set_stack` rewritten: global-set + layers RefCells updated, engine nodes dirtied on change — dehaze/sharpen/geometry/transmission routing UNTOUCHED)
- Modify: `ferrolite-pipeline/src/tile_edit.rs` (same swap at `tile_edit.rs:200-284`; tile controls / `set_tile_transform` seam preserved on the Color engine node)
- Modify: `ferrolite-pipeline/src/lib.rs` (`prewarm_shaders`: drop the six retired shader entries — exposure/white_balance/contrast/tone_curve/hsl/color_grade — ONLY those with no remaining node referencing them; grep before deleting any `.wgsl` file; the pass-count doc comment updates)
- Possibly modify: `ferrolite-export` tolerance (per the Global Constraint, one reviewed change max)

**Interfaces:**
- Consumes: Tasks 1-2. `EditPipeline`/`TileEditPipeline` public APIs (`new`, `set_stack`, `evaluate`, `set_shared_transmission`, `transmission_texture`, tile controls) are UNCHANGED — `ferrolite-app` and `ferrolite-export` must compile without modification (any app-side change needed means the swap leaked — stop and reassess).
- Produces: the fused pipelines; Task 1's parity suite green against the pre-fusion goldens.

- [ ] **Step 1:** Preview pipeline swap. Dirty semantics: `set_stack` compares the new doc's `global` (segmented) + `layers` against the engines' current state; a light-segment change dirties the Light engine; a color-segment or layers change dirties the Color engine; unchanged fields dirty nothing (a grade-only drag must NOT re-run the Light engine or dehaze — assert via the graph's existing eval-count test hooks if present, else a new hook mirroring `local_rebuild_count`).
- [ ] **Step 2:** `cargo test -p ferrolite-pipeline` → the Task 1 parity suite is the gate. Debug any fixture exceeding `PARITY_TOL` (expected culprits: a missed order flag, region math applied in the wrong segment, double-applied vibrance).
- [ ] **Step 3:** Tile pipeline swap; the tile-seam golden + P2b's node parity tests + `cargo test -p ferrolite-export` gate it (tolerance handling per Global Constraints if needed).
- [ ] **Step 4:** Workspace check (`cargo check --workspace --all-targets`) — app/export compile untouched.
- [ ] **Step 5: Scoped gate + commit** (`fmt`/`clippy` for ferrolite-pipeline; `cargo test -p ferrolite-pipeline -p ferrolite-export`): `feat(pipeline): fused two-segment layer engine replaces the six point-op passes`.

---

### Task 4: Un-grey the Phase-3 controls

**Files:**
- Modify: `ferrolite-app/src/develop/adjustments.rs` (flip `global_ready: true` + empty the Phase-3 reasons for: highlights, shadows, whites, blacks, saturation, hue, color_amount; vibrance: `global_ready: true, mask_ready: true`, empty both reasons)
- Modify: `ferrolite-app/src/develop/base_tabs.rs` (the color-swatch picker's global arm: replace the disabled+reason wrapper with the live picker — same code path as the mask arm)
- Modify: registry tests asserting the old gating (`color_registry_rows_and_gating`, the Light-tab gating assertions) to the new truth.

**Interfaces:** consumes Task 3 (the engine actually applies these globally now). No new APIs.

- [ ] **Step 1:** flip flags + tests (update the gating tests FIRST, watch them fail against the old flags, then flip).
- [ ] **Step 2:** `cargo test -p ferrolite-app` full → green (registry invariants: empty reason is only legal when ready — the invariant test enforces the pairing automatically).
- [ ] **Step 3: Scoped gate + commit**: `feat(develop): global H/S/W/B, saturation/hue, swatch and vibrance go live (Phase 3 engine)`.

Note for the final review + author: global BASIC H/S/W/B (region math) now coexists with the parametric TONE CURVE H/S/W/B — different algorithms, Lightroom-style precedent (Basic panel vs point-curve regions). Deliberate; the author judges it in the visual pass.

---

### Task 5: After-benchmarks + the perf gate

**Files:**
- Modify: `docs/benchmarks/2026-07-28-phase3-fused-engine.md` (append "After (fused, commit <hash>)" numbers + the verdict table)

- [ ] **Step 1:** run `engine_bench.rs` (same machine, same synthetic sizes) → record the three medians.
- [ ] **Step 2:** verdict table: per case, baseline ms, after ms, ratio. GATE: every case ≤ baseline (within run-to-run noise, state the noise band from the 20-sample spread) AND case (a) early-op ≥2×. If the gate fails → status BLOCKED with the numbers (do NOT commit a rationalization).
- [ ] **Step 3:** commit: `perf(pipeline): fused-engine benchmark results — <headline ratio> on early-op drags`.

---

## Coordinator wrap-up (not a subagent task)

1. `rustup update stable`, full repo gate.
2. Visual test plan for the author:
   - **Perf feel (the point of this phase):** Exposure/Temp/Contrast drags at fit and 1:1 on a big RAW — before/after feel, plus the benchmark numbers from Task 5.
   - **Newly-live globals:** H/S/W/B in BASIC SLIDERS now work globally (region-based — compare character against the parametric TONE CURVE versions and judge the coexistence); Saturation/Hue/Vibrance + color swatch live in Adjust mode; Vibrance also live per-mask.
   - **Parity spot-checks:** a previously-edited image (global + masked edits) renders indistinguishably; one export re-run compared against a pre-phase export.
   - **Regression smoke:** dehaze drag (transmission caching intact — no new sluggishness), sharpen, crop/rotate at 1:1 (tile seams), mask create/paint/overlay, undo/redo, before/after split.
3. Wait for the author's verdict. Remaining phase afterward: Phase 4 (per-mask neighborhood passes: sharpen → NR → dehaze/clarity).
