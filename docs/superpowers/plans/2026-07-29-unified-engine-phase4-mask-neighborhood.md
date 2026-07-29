# Unified Maskable Adjustments — Phase 4: Per-Mask Neighborhood Ops (Sharpen + Dehaze) & Deferred Perf

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Per-mask Dehaze (amount) and per-mask Sharpen go live — the last greyed controls with real engines — while landing the two profiled perf follow-ups (separable sharpen ~−12 ms, recovery-into-engine fusion ~−5 ms). NR and clarity/texture stay greyed (no algorithm exists in ANY scope; explicitly out of scope — a future effort).

**Architecture:**
1. **Dehaze recovery fuses into the Color engine.** Recovery `(I−A)/t′ + A` with `t′ = 1 − amount·(1−t)` is per-pixel once the shared whole-image transmission `t` exists (it stays its own heavy node). The standalone `DehazeRecoveryNode` is deleted from both pipelines; the Color engine gains a transmission binding (+ source-UV mapping — the tile path reuses the node's existing `TileFrame` controls, the same mapping the recovery node used) and applies recovery as the FIRST step of the global color pseudo-layer (preserving today's post-contrast/pre-curve order). Each mask layer's loop dispatch gains the same step driven by `layer.adjustments.dehaze.amount`, computed from the ORIGINAL engine input `I` (bind it alongside `current`) and blended by the mask. Per-mask dehaze RADIUS stays global-only — the radius shapes the shared transmission map.
2. **Sharpen becomes separable + layered.** The O(r²) box loop becomes two O(r) passes (H then V — same box mean) into a blur texture (the shared heavy map), then apply dispatches: global `c + a·(c − blur)`, then per layer `c += m·aᵢ·(c_in − blurᵣᵢ)` sequentially. Layers may use distinct radii: one separable blur per DISTINCT radius among {global ∪ layers with amount≠0} (2 passes each, cheap now). The sharpen node consumes the Color engine's already-composited `MaskBuffer`s via a shared handle — no second compositing, and mask semantics stay consistent with the engine's (range masks keyed off post-global-color content). `sharpen_halo`/`needs_full_rebuild` grow to the MAX radius across global+layers.
3. **UI:** mask-scope `sharpen_amount`, `sharpen_radius`, `dehaze_amount` flip `mask_ready: true`; `dehaze_radius` keeps `mask_ready: false` with a NEW accurate reason ("Radius shapes the shared whole-image transmission — global only").

**Parity policy (settled precedent):** the parity suite pins the fused engine at `PARITY_TOL = 2e-3`. Fusing recovery removes one more f16 round-trip — dehaze-bearing fixtures may drift ≲1–2 f16 ULP (amplified regionally by divide-by-t, the documented mechanism). If a fixture exceeds tolerance ONLY by that mechanism (prove it: identity-dehaze fixtures must stay green; the drift must scale with dehaze amount), regenerate that golden with a documented note — same adjudication class the author approved 2026-07-29. Separable sharpen: identical mean modulo float order — `full_global` may drift ≤~1e-3; same policy. Any drift NOT explained by these two mechanisms is a bug.

**Tech Stack:** Rust, wgpu/WGSL; existing `LocalAdjustmentsNode` engine (Color stage), `DehazeTransmissionNode`, `CachedMasks`, `engine_bench` harness, parity goldens.

**Spec:** design doc §4 (phased neighborhood wiring), §1.3; benchmark doc's deferred-items list (author-approved 2026-07-29).

## Global Constraints

- Branch: `feat/ui-v2-rewrite`. Never commit to `main`.
- CPU/GPU lockstep: every new engine step lands in `light_color_apply` (CPU reference) and the WGSL together; node-level parity tests extend accordingly. The sharpen node has no CPU twin today — its correctness gates are the parity goldens + a new separable-equals-2D-box unit test.
- Behavior preservation: global dehaze output unchanged within the parity policy above; global sharpen mean identical (separable == 2D box). Existing masked-edit output unchanged (new per-layer steps are zero-identity).
- Dirty semantics: per-layer dehaze/sharpen amount changes are cheap uniform updates (Color-engine/sharpen-node dirty only); global dehaze radius keeps dirtying the transmission node; `needs_full_rebuild`'s sharpen-halo key now reflects the max radius (a LAYER radius change must trigger the full-tier rebuild exactly like the global radius does today — trace `ferrolite-app/src/develop/ops_edit.rs` `needs_full_rebuild` and its `full_stack` comparison).
- Mask sharing: the engine's composited masks are exposed via a narrow handle (e.g. `Rc<RefCell<CompositedMasks>>` of the buffers + layer indices), consumed by the sharpen node in BOTH pipelines; tiles composite per tile as today.
- Per-control reset, greyed-with-reason, keybind conventions: unchanged (CLAUDE.md).
- Subagents run scoped gates; coordinator runs the repo gate.

---

### Task 1: Separable sharpen (global only — perf + identical output)

**Files:** `ferrolite-pipeline/src/shaders/sharpen_box_h.wgsl` (new), `sharpen_box_v.wgsl` (new), `sharpen_apply.wgsl` (new: `c + a·(c − blur)` reading src+blur), retire the fused `sharpen.wgsl` loop (file stays in-tree as reference); `ferrolite-pipeline/src/nodes.rs` or a new `sharpen_node.rs` (multi-pass node with an intermediate + blur texture, `ensure_*` dims-keyed allocation mirroring `dehaze_node.rs`'s pattern); both pipelines swap the node in place (same graph position, same inputs); `lib.rs` prewarm entries.

- [ ] Step 1 (failing test): unit test `separable_box_equals_2d_box` — CPU-compute both on a small deterministic image (e.g. 16×16 gradient+noise, radius 3, clamped edges — mirror the WGSL's `clamp` edge handling exactly) and assert per-pixel equality within 1e-6; plus a GPU node test: new sharpen node output vs the OLD 2D formula computed CPU-side, within 2e-3 (f16 storage).
- [ ] Step 2: implement; parity suite — `full_global` is the only sharpen-bearing fixture; expect ≤~1e-3 drift (float order); if regenerated, document per the parity policy.
- [ ] Step 3: quick bench spot-check (`engine_bench` case (a), 3 runs) — record the sharpen improvement in the benchmark doc ("Phase 4 increments" section, cool machine note).
- [ ] Step 4: scoped gate (`fmt`/`clippy`/`test -p ferrolite-pipeline`; `test -p ferrolite-export`), commit: `perf(pipeline): separable sharpen — O(r) two-pass box blur + apply`.

### Task 2: Recovery fuses into the Color engine (global path)

**Files:** `ferrolite-pipeline/src/shaders/local_adjust.wgsl` (transmission binding @binding(5) + `dehaze_recover()` step, ported EXACTLY from `dehaze_recovery.wgsl`'s per-pixel math incl. its source-UV sampling and `t` floor clamp); `ferrolite-pipeline/src/local_node.rs` (binding, uniform fields: dehaze amount + atmos in a new vec4; the engine's existing `TileFrame`/tile controls drive the UV mapping — mirror how `DehazeRecoveryNode` consumed them); `ferrolite-pipeline/src/uniforms.rs` (`light_color_apply` gains the recovery step FIRST in the color segment, flag-gated; CPU transmission lookup = a closure/param since CPU tests need an injectable `t`); `pipeline.rs`/`tile_edit.rs` (delete the recovery node; transmission node output handed to the Color engine; dirty routing: dehaze amount now dirties the Color engine, radius still dirties transmission); `dehaze_node.rs` (`DehazeRecoveryNode` deleted; `CachedBinds` pattern stays for transmission).

- [ ] Step 1 (failing test): node-level GPU test — Color engine with a synthetic transmission texture (e.g. constant 0.5) + global dehaze amount 0.4 matches the CPU reference (recovery step with injected t) within existing tolerance; identity amount ⇒ bit-identical to pre-change engine output (zero extra work — flag-gated).
- [ ] Step 2: implement; run parity suite — `full_global` + range-mask fixtures may drift per the parity policy (prove the mechanism: rerun with dehaze zeroed → must be green); regenerate w/ documentation only if proven.
- [ ] Step 3: dirty-routing regression tests: dehaze-amount-only change does NOT re-run the transmission node (extend the existing eval-count hooks); radius change does not re-run the Light engine.
- [ ] Step 4: scoped gate + `cargo check --workspace --all-targets` (app must compile untouched), commit: `perf(pipeline): dehaze recovery fused into the color engine (one less full-res pass)`.

### Task 3: Per-mask dehaze amount

**Files:** `local_adjust.wgsl` + `uniforms.rs` (per-layer uniform gains the layer's dehaze amount — reuse the recovery step in the LAYER loop, computed from the ORIGINAL engine input `I` (bind it) and blended `mix(cur, recovered, m)` BEFORE the layer's point ops, mirroring the global relative order); `local_node.rs` (per-layer uniform fill from `layer.adjustments.dehaze.amount`); `ferrolite-app/src/develop/adjustments.rs` (`dehaze_amount.mask_ready = true`, reason emptied; `dehaze_radius.mask_reason` = "Radius shapes the shared whole-image transmission — global only"); gating tests updated (TDD: assertions first).

- [ ] Step 1 (failing tests): CPU+GPU — a mask layer with dehaze amount 0.5 over a synthetic transmission changes ONLY masked pixels (unmasked bit-identical); layer amount 0 adds zero cost (flag/identity gate); UI gating test updated.
- [ ] Step 2: implement (lockstep); parity suite green (all existing fixtures have zero layer-dehaze).
- [ ] Step 3: scoped gates (`ferrolite-pipeline`, `ferrolite-app`), commit: `feat(pipeline): per-mask dehaze amount via the shared transmission`.

### Task 4: Per-mask sharpen

**Files:** `local_node.rs` (expose composited masks: `pub(crate) fn composited_masks_handle(&self) -> Rc<RefCell<...>>` — design the narrow type; populated during `evaluate_color`, cleared/refreshed per evaluate; tile path populates per tile); sharpen node (consume the handle: per-layer apply dispatches after the global apply, one blur per distinct radius, layer uniform = amount + mask binding); `sharpen_apply.wgsl` grows a masked variant (or a mask binding + full-coverage flag, mirroring the engine's pattern); `uniforms.rs` `sharpen_halo` → max radius over global+visible layers (new signature taking the doc or the layer list — update `needs_full_rebuild`'s call in `ferrolite-app/src/develop/ops_edit.rs` accordingly — this IS an app-crate touch, keep it mechanical); `adjustments.rs` (`sharpen_amount`/`sharpen_radius` mask_ready = true, reasons emptied); gating tests.

- [ ] Step 1 (failing tests): GPU node test — one full-coverage mask layer with sharpen amount 1.0/r2 over a synthetic image equals global sharpen 1.0/r2 (full coverage ⇒ same result); a half-coverage mask sharpens only masked pixels; distinct global/layer radii produce two blurs (assert via an eval-count/blur-count hook); halo test: `sharpen_halo` returns the max.
- [ ] Step 2: implement; parity green (existing fixtures have zero layer-sharpen); UI flips + tests.
- [ ] Step 3: scoped gates both crates + `cargo check --workspace`, commit: `feat(pipeline): per-mask sharpen — shared separable blur, per-layer masked apply`.

### Task 5: Benchmarks + docs + fixture coverage

- [ ] New parity fixtures + goldens (new-engine renders, tight tol): `mask_dehaze` (one mask, dehaze amount 0.5, global dehaze 0.3/r8) and `mask_sharpen` (one mask, sharpen 1.0/r2, global sharpen 0.8/r2, distinct radius variant).
- [ ] Full `engine_bench` re-run (5 runs, cool machine if possible): record the Phase 4 deltas in the benchmark doc (expected: case (a) drops by roughly the sharpen+recovery savings; state reality honestly — no gate this time, the re-based gate is "no regression + explanation").
- [ ] Update the spec's §4 chain diagram note + the V2 design README's remaining-greyed list (NR/clarity only). Scoped gate, commit: `test(pipeline): phase-4 parity fixtures + benchmark update`.

## Coordinator wrap-up

1. Repo gate on latest stable.
2. Visual test plan for the author: per-mask dehaze on a hazy shot (amount live in Mask scope, radius greyed with the new reason — hover it); per-mask sharpen incl. a two-mask different-radius case at 1:1; global sharpen feel (should be snappier — separable) and unchanged look; dehaze drag perf (one less pass); regression smoke: global dehaze/sharpen at 1:1 tile seams, export of a dehazed+masked edit, undo/redo.
3. After the author's verdict: the spec's phases are COMPLETE → the UX feedback pass (workstream 2, spec §8) is next, then finishing-a-development-branch for the whole feat/ui-v2-rewrite branch.
