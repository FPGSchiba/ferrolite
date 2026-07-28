# Dehaze Shared-Transmission (fix tiled full-res OOM on constrained GPUs) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Stop the full-res tiled dehaze from running the ~14-pass guided-filter transmission per tile (which OOMs a memory-constrained integrated GPU when streaming a large image) by computing the transmission **once** as a bounded whole-image map and having each tile's recovery **sample** it — turning tiled dehaze into a cheap per-pixel pass with no halo.

**Architecture:** The transmission map is a smooth whole-image property (like the atmospheric light `A`, already computed once). The whole-image `EditPipeline` (preview tier) already computes it once, bounded to ≤`DEHAZE_MAX_TRANSMISSION_DIM`, in **source space** (dehaze runs before the geometry node). Expose that texture and hand it to the tiled `TileEditPipeline`; drop the tiled tier's per-tile `DehazeTransmissionNode`; the tiled `DehazeRecoveryNode` samples the shared transmission at each output pixel's **source UV** (reusing the geometry transform the head already applies), so it is correct under crop/rotate. Export builds the same bounded transmission once from its source. With no per-tile transmission, tiled dehaze contributes **no halo** and ~1 cheap pass per tile.

**Tech Stack:** Rust, `wgpu` + WGSL compute, `bytemuck` Pod uniforms. Photo tier (`ferrolite-pipeline`) + app/export wiring. No new deps.

## Global Constraints

- **Root cause (confirmed, do not re-diagnose):** on an Intel Iris Xe (Vulkan, `max_buffer_size` 256 MiB) the tiled `DehazeTransmissionNode::evaluate` inside `produce_tile` exhausts GPU memory when the drive loop streams full-res dehaze tiles across a 24 MP image (backtrace: `DehazeTransmissionNode::evaluate → produce_tile → produce_view → update`). The per-tile multi-pass transmission is the cost. The **preview** tier already computes the transmission once (whole-image, bounded) and is NOT the crash — do not change how the preview computes it.
- **The transmission is source-space (pre-geometry).** In `EditPipeline` the dehaze nodes run at `contrast_id`, before the `geometry` node (the output). So the preview transmission texture is in **source space**, at the preview source resolution capped to `DEHAZE_MAX_TRANSMISSION_DIM` (1536). The tiled tier applies geometry at the head (output space); its recovery therefore must sample the source-space transmission at each output pixel's **source coordinate** (via the geometry transform), NOT by output UV — output UV would be wrong under crop/rotate.
- **Correctness bar:** with identity geometry (no crop/rotate) the tiled dehaze result must match the whole-image render within the existing `SEAM_TOL` (the parity golden). Under crop/rotate the source-UV sampling keeps the transmission aligned to the source content (the same content the geometry head sampled), preserving the existing "accepted output-space difference" stance for the *recovery* only.
- **No per-tile transmission, no tiled dehaze halo:** after this change the tiled `dehaze_halo` contribution is 0 (recovery is per-pixel). `needs_full_rebuild` must NOT force a tiled rebuild on dehaze radius/amount changes; a transmission change is propagated by re-wiring the shared texture + bumping the opstack version (re-produce), not a producer rebuild.
- **Amount stays cheap:** an amount drag must not recompute the transmission (it doesn't affect it) — preserved because the preview transmission node already excludes amount, and the tiled recovery just samples.
- **Build-once GPU / no per-frame CPU** (CLAUDE.md). `#[repr(C)]` uniforms mirror WGSL. `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean.
- **Keep prior fixes:** the `edit_in_progress` drag-pause and the transmission working-res cap stay. The `[ferrolite gpu]` startup log added for diagnosis may remain (it is harmless) — do not remove it in this plan.

**Branch:** continue on `feat/p3-dehaze`.

**Gate (after each task; green except the 5 pre-existing `ferrolite-decode` fixture failures):**
```
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

---

## Design detail (the shared transmission + source-UV sampling)

The tiled `DehazeRecoveryNode` currently takes two graph inputs: `inputs[0]` = the haloed output-space image `I`, `inputs[1]` = the per-tile transmission (from the tiled `DehazeTransmissionNode`). It samples `trans` by the tile's local UV.

After this change, the tiled recovery instead holds an **externally-supplied shared transmission texture** `T` (source space, bounded) plus:
- the **geometry uniform** (`m`, `off`, `src_dims`) — identical to what `GeometryHeadNode` uses to map output→source, and
- the **tile frame** (`origin` = this haloed tile's output-space top-left, from the existing `TileFrame` the vignette node already consumes).

For each output pixel at haloed-local `(lx, ly)`, its output-space coordinate is `out = origin + (lx, ly)`; its source coordinate is `src = m · out + off` (exactly the geometry head's mapping); the transmission UV is `src / src_dims`. Sample `T` bilinearly there. This aligns the (source-space) transmission with the source content the head sampled to build this tile — correct under crop/rotate. Under identity geometry `m = I`, `off = 0`, so `src = out` and it reduces to the whole-image alignment the parity golden checks.

The shared `T` is:
- **In-app:** the preview `EditPipeline`'s transmission texture (`transmission_texture()`), computed once per edit, bounded. The app hands it to the tiled producer whenever it (re)evaluates the preview.
- **In export:** a one-shot bounded transmission the export builds from its CPU source (export has the `LinearRgbaF32` that built the pyramid).

When dehaze is inactive (amount 0 → no transmission), `T` is `None` and the recovery is a passthrough (amount 0 already passes through; belt-and-suspenders: skip sampling when `T` is absent).

---

## File Structure

- `ferrolite-pipeline/src/pipeline.rs` — `EditPipeline::transmission_texture()` accessor (+ the source dims it is in). *(Task 1)*
- `ferrolite-pipeline/src/dehaze_node.rs` — `DehazeRecoveryNode` reworked to sample an external shared transmission via source-UV (geometry uniform + tile frame); `set_shared_transmission` / frame / geometry setters. *(Task 2)*
- `ferrolite-pipeline/src/shaders/dehaze_recovery.wgsl` — source-UV sampling of the shared transmission. *(Task 2)*
- `ferrolite-pipeline/src/tile_edit.rs` — drop the per-tile `DehazeTransmissionNode`; wire the recovery to the shared transmission + geometry + frame; `set_shared_transmission`; remove dehaze from the halo. *(Task 3)*
- `ferrolite-pipeline/src/dehaze.rs` — `dehaze_halo` returns 0 for the tiled tier (recovery is per-pixel now); keep the transmission working-res + guided radius for the whole-image (preview/export) computation. *(Task 3)*
- `ferrolite-app/src/app.rs` + `viewer/edit_producer.rs` — after preview evaluate, hand the preview transmission to the tiled producer; re-wire on recompute; `needs_full_rebuild` no longer keys on dehaze. *(Task 4)*
- `ferrolite-export/src/render.rs` (+ `ferrolite-app/src/export/*`) — build the bounded transmission once from the source and set it on the producer. *(Task 5)*
- `ferrolite-pipeline/tests/golden.rs` — update the tiled-parity golden to the shared-transmission path; keep it seam-sensitive. *(Task 3)*

---

## Task 1: Expose the preview transmission from `EditPipeline`

**Files:** Modify `ferrolite-pipeline/src/pipeline.rs`.

**Interfaces:**
- Produces: `EditPipeline::transmission_texture(&self) -> Option<std::sync::Arc<wgpu::Texture>>` (the `DehazeTransmissionNode`'s current output texture, or `None` when dehaze is inactive) and `EditPipeline::transmission_src_dims(&self) -> [f32; 2]` (the source dims the transmission is aligned to = the preview source `(src_w, src_h)`).

- [ ] **Step 1: Write the failing test** (in `pipeline.rs` `#[cfg(test)] mod edit_pipeline_tests`)

```rust
    #[test]
    fn transmission_texture_present_only_when_dehaze_active() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let src = LinearRgbaF32::new(32, 24, vec![0.5; 32 * 24 * 4]).unwrap();
        // No dehaze → no transmission texture.
        let mut ep = EditPipeline::new(ctx.clone(), &src, OpStack::default(), IDENTITY);
        let _ = ep.evaluate();
        assert!(ep.transmission_texture().is_none());
        assert_eq!(ep.transmission_src_dims(), [32.0, 24.0]);
        // Dehaze active → a transmission texture exists.
        let stack = OpStack::default().set_op(crate::op::Op::Dehaze(crate::op::Dehaze {
            amount: 0.6,
            radius: 8,
        }));
        ep.set_stack(stack);
        let _ = ep.evaluate();
        assert!(ep.transmission_texture().is_some());
    }
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p ferrolite-pipeline --lib transmission_texture_present_only_when_dehaze_active` → FAIL (methods missing).

- [ ] **Step 3: Implement the accessors.** Retain an `Rc<DehazeTransmissionNode>` handle on `EditPipeline` (like `local_node`) if not already retained. Add:

```rust
    /// The current whole-image dehaze transmission texture (source space, bounded
    /// to `DEHAZE_MAX_TRANSMISSION_DIM`), or `None` when dehaze is inactive. Shared
    /// with the tiled producer so it does not recompute the transmission per tile.
    pub fn transmission_texture(&self) -> Option<std::sync::Arc<wgpu::Texture>> {
        self.dehaze_transmission_node.current_output_texture()
    }

    /// The source dims the transmission is aligned to (the preview source size).
    pub fn transmission_src_dims(&self) -> [f32; 2] {
        [self.src_w as f32, self.src_h as f32]
    }
```

Add `DehazeTransmissionNode::current_output_texture(&self) -> Option<Arc<wgpu::Texture>>` in `dehaze_node.rs`, returning the cached `out` texture's `Arc` when the last evaluate ran the passes (dehaze active) else `None`. (The node already caches its `out` in a `RefCell<Option<PipelineImage>>`; return `out.borrow().as_ref().map(|p| p.texture.clone())`, gated on the active flag so an inactive early-return reports `None`.)

- [ ] **Step 4: Run to verify it passes** — the new test → PASS (or headless-skip).

- [ ] **Step 5: Commit** — `git commit -am "feat(pipeline): expose EditPipeline whole-image dehaze transmission texture"`

---

## Task 2: `DehazeRecoveryNode` samples an external shared transmission via source-UV

**Files:** Modify `ferrolite-pipeline/src/dehaze_node.rs`, `ferrolite-pipeline/src/shaders/dehaze_recovery.wgsl`.

**Interfaces:**
- Consumes: `crate::uniforms::GeometryUniform` (fields `m: [f32;4]`, `off: [f32;2]`, `src_dims: [f32;2]`, `out_dims`, `out_origin`); `crate::nodes::TileFrame` (`origin: [f32;2]`, `full_dims: [f32;2]`).
- Produces: `DehazeRecoveryNode` gains `set_shared_transmission(&self, tex: Option<Arc<wgpu::Texture>>)`, `set_geometry(&self, GeometryUniform)`, and reads the shared `TileFrame` (via an `Rc<Cell<TileFrame>>` passed at construction, same one the vignette node uses). Its single input is now just `I` (`inputs[0]`); the transmission is an owned/set texture, not a graph input. A `RecoveryParams` field `has_transmission: u32` (1 when a shared transmission is bound, else 0) tells the shader whether to sample or pass through.

- [ ] **Step 1: Write the failing test** — extend `recovery_node_matches_dehaze_recover` (or add `recovery_samples_shared_transmission_identity_geometry`): build a recovery node with a small constant-`q` shared transmission texture + identity `GeometryUniform` + a `TileFrame { origin: [0,0], full_dims: [w,h] }`, evaluate on an image, and assert each pixel equals `dehaze_recover(px, (1-q)/DEHAZE_OMEGA, atmos, amount)` within `2e-3`. (Identity geometry → source UV == output UV == local UV, so a constant `q` yields the same result as before.)

- [ ] **Step 2: Run to verify it fails** — `cargo test -p ferrolite-pipeline --lib dehaze_node::recovery` → FAIL (new setters/signature).

- [ ] **Step 3: Implement.**
  - Shader `dehaze_recovery.wgsl`: bindings `0 = img`, `1 = shared transmission (source space)`, `2 = dst`, `3 = uniform P`, `4 = sampler`. Add to `struct P`: `geo_m: vec4<f32>`, `geo_off: vec2<f32>`, `src_dims: vec2<f32>`, `frame_origin: vec2<f32>`, `has_transmission: u32` (+ padding to 16B). Compute:
    ```wgsl
    let out_xy = p.frame_origin + vec2<f32>(f32(gid.x), f32(gid.y));
    let src = vec2<f32>(
        p.geo_m.x * out_xy.x + p.geo_m.y * out_xy.y + p.geo_off.x,
        p.geo_m.z * out_xy.x + p.geo_m.w * out_xy.y + p.geo_off.y,
    );
    let uv = src / p.src_dims;
    let t = select(1.0, clamp(textureSampleLevel(trans, samp, uv, 0.0).r, 0.0, 1.0), p.has_transmission == 1u);
    ```
    When `has_transmission == 0u` (or `amount == 0`), pass `I` through unchanged. Otherwise apply the existing recovery blend with `t`.
  - Node: replace the graph-input transmission with an internally-held `Arc<Texture>` (default: a 1×1 neutral texture so a bind group always validates) + `has_transmission` flag; add `set_shared_transmission`, `set_geometry` (writes `geo_*`/`src_dims` into `RecoveryParams`), and construct with the shared `Rc<Cell<TileFrame>>` (read into `frame_origin` each evaluate). `evaluate` takes only `inputs[0]` = `I`.
  - Keep the pure `dehaze_recover` reference unchanged.

- [ ] **Step 4: Run to verify it passes** — the recovery tests → PASS on GPU.

- [ ] **Step 5: Commit** — `git commit -am "feat(pipeline): DehazeRecoveryNode samples an external shared transmission via source-UV"`

---

## Task 3: `TileEditPipeline` uses the shared transmission (drop per-tile transmission + halo) + parity golden

**Files:** Modify `ferrolite-pipeline/src/tile_edit.rs`, `ferrolite-pipeline/src/dehaze.rs`, `ferrolite-pipeline/tests/golden.rs`.

**Interfaces:**
- Produces: `TileEditPipeline` no longer builds a `DehazeTransmissionNode`; the `DehazeRecoveryNode` (input `contrast_id`) samples a shared transmission set via `TileEditPipeline::set_shared_transmission(&mut self, tex: Option<Arc<wgpu::Texture>>)`; the recovery is wired the geometry uniform (from the stack's geometry) + the head's `TileFrame`. `dehaze_halo` returns 0.

- [ ] **Step 1: Update the parity golden first.** `dehaze_tiled_matches_whole_image`: build the whole-image `EditPipeline` (which computes the transmission), pass `whole.transmission_texture()` into the tiled `TileEditPipeline::set_shared_transmission(...)`, and assert tiled == whole within `SEAM_TOL` at identity geometry (the shared-transmission path must match the whole-image render). Keep the sawtooth fixture + amount so it is seam-sensitive. (No fold-in halo to remove now; instead the guard is that a WRONG source-UV mapping would break the match — verify by temporarily zeroing `frame_origin`/geometry and confirming the test then fails.)

- [ ] **Step 2: Run to verify it fails** — `cargo test -p ferrolite-pipeline --test golden dehaze_tiled_matches_whole_image` → FAIL (`set_shared_transmission` missing; tiled still builds its own transmission).

- [ ] **Step 3: Implement.**
  - `dehaze.rs`: `dehaze_halo(_) -> 0` (recovery is per-pixel; document that the whole-image transmission carries the neighbourhood, computed off the tiled path). Keep `transmission_working_dims`, `guided_radius`, `transmission_map`, and the `DehazeTransmissionNode` (still used by the whole-image `EditPipeline`/export).
  - `tile_edit.rs`: remove the `DehazeTransmissionNode` construction + its `dehaze` param cell; the `DehazeRecoveryNode` input becomes `vec![contrast_id]`; construct the recovery with the head's shared `TileFrame` `Rc<Cell<>>`; call `recovery.set_geometry(geometry_uniform_for(stack.geometry(), src_w, src_h))` at build + in `set_stack`; add `set_shared_transmission` delegating to the recovery node; the `halo` line drops the `dehaze_halo` term (now 0). `set_dehaze_atmos` still sets the recovery's `atmos`.
  - Note: the recovery's `has_transmission` is set by `set_shared_transmission(Some/None)`.

- [ ] **Step 4: Run to verify it passes** — parity golden PASS; do the RED check (zero the geometry/frame → fails) then restore. Record numbers.

- [ ] **Step 5: Commit** — `git commit -am "feat(pipeline): tiled dehaze samples shared transmission (no per-tile transmission, halo=0); parity holds"`

---

## Task 4: App wiring — hand the preview transmission to the tiled producer

**Files:** Modify `ferrolite-app/src/viewer/edit_producer.rs`, `ferrolite-app/src/app.rs`, `ferrolite-app/src/develop/ops_edit.rs`.

**Interfaces:**
- Produces: `EditTileProducer::set_shared_transmission(&mut self, tex: Option<Arc<wgpu::Texture>>)` delegating to `TileEditPipeline`. In `set_preview_and_full`, after the preview `EditPipeline` evaluates, call `producer.set_shared_transmission(preview_ep.transmission_texture())` so the tiled tier samples the same map. `needs_full_rebuild` drops its `dehaze_halo` clause (dehaze no longer changes the tiled halo/geometry; radius/amount changes flow through the re-wired transmission + version bump).

- [ ] **Step 1:** Add `EditTileProducer::set_shared_transmission` (delegate). Build `-p ferrolite-app`.
- [ ] **Step 2:** In `set_preview_and_full`, in the full-res branch (produce_full), after the preview `ep.evaluate()` (which recomputes the transmission), fetch `ep.transmission_texture()` and call `producer.set_shared_transmission(tex)` on the (rebuilt-or-existing) producer, before `set_opstack_version`. (The preview is the source of the shared transmission; it is evaluated in the same method just above.)
- [ ] **Step 3:** Remove the `dehaze_halo(old.dehaze()) != dehaze_halo(new.dehaze())` clause from `needs_full_rebuild` (it is now always equal — `dehaze_halo` is 0). Update `needs_full_rebuild_on_dehaze_halo_change` to assert dehaze changes no longer force a rebuild (amount/radius are color-like now). 
- [ ] **Step 4:** `cargo build -p ferrolite-app && cargo test -p ferrolite-app` → PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(app): share the preview dehaze transmission with the tiled producer"`

---

## Task 5: Export — build the bounded transmission once from the source

**Files:** Modify `ferrolite-export/src/render.rs`, `ferrolite-export/src/job.rs`, `ferrolite-app/src/export/{mod.rs,batch.rs}`.

**Interfaces:**
- Produces: `render_tiled` builds a bounded whole-image transmission once from the source (via a short-lived `EditPipeline` over the CPU source, or a direct `DehazeTransmissionNode` over a bounded source) and calls `pipeline.set_shared_transmission(tex)` before the tile loop. `render_tiled` already receives `atmospheric_light`; it needs the CPU source (or a bounded EditPipeline) to compute the transmission — thread the CPU `LinearRgbaF32` in (the export callers have it; add a `&LinearRgbaF32` param to `render_tiled`, or build the transmission in `job.rs`/`export/*` where the source is in scope and pass the `Arc<Texture>` in).

- [ ] **Step 1:** In the export path where the CPU `LinearRgbaF32` + `ctx` are available (`export/mod.rs` / `batch.rs` build the pyramid from it), build a bounded whole-image transmission: `let ep = EditPipeline::new(ctx, &source, stack, camera_to_working); ep.evaluate(); let t = ep.transmission_texture();` and pass `t` down to `render_tiled` (new param `shared_transmission: Option<Arc<wgpu::Texture>>`).
- [ ] **Step 2:** `render_tiled` calls `pipeline.set_shared_transmission(shared_transmission)` after building the `TileEditPipeline`, before the tile loop. Update the render-golden calls to pass `None` (their stacks have no dehaze → identity).
- [ ] **Step 3:** `cargo test -p ferrolite-export` → PASS (goldens byte-identical: no dehaze → `None` → passthrough).
- [ ] **Step 4: Commit** — `git commit -am "feat(export): build the dehaze transmission once and share it to the tiled render"`

---

## Self-Review

- **OOM root cause** (per-tile tiled transmission) → removed in Task 3 (no per-tile transmission node; recovery samples a shared map; halo 0). Memory + per-tile cost collapse to a per-pixel sample.
- **Correctness under geometry** → Task 2 source-UV sampling (geometry uniform + tile frame), verified by the identity-geometry parity golden (Task 3) and the source-UV mapping mirroring the geometry head.
- **Amount stays cheap / no tiled rebuild on dehaze** → Task 4 (`needs_full_rebuild` drops dehaze; transmission re-wired + version bump).
- **Export** → Task 5 builds the transmission once from the source.
- **Prior fixes kept** → drag-pause (`edit_in_progress`), transmission working-res cap, GPU log all untouched.
- **Placeholder scan** → the reused pieces (transmission passes, geometry mapping) point at existing code (`DehazeTransmissionNode`, `GeometryHeadNode`'s `m·out+off`); the new code (recovery source-UV shader, accessors, wiring) is given.
- **Type consistency** → `transmission_texture() -> Option<Arc<wgpu::Texture>>`, `set_shared_transmission(Option<Arc<wgpu::Texture>>)`, `set_geometry(GeometryUniform)` used identically across Tasks 1–5.

## Open design note for the author's review

**Space of the shared transmission.** It is computed **source-space** (the preview computes dehaze before geometry) and the tiled recovery samples it at each output pixel's source coordinate. This is exact for identity geometry and correct-by-construction under crop/rotate (it samples the transmission at the same source location the geometry head sampled). The alternative — recomputing the transmission in the tiled tier's *output* space — would need a bounded whole-output render inside the producer (more code) and is not worth it given the source-UV mapping is correct. Flagging so you can confirm the source-space choice before execution.
