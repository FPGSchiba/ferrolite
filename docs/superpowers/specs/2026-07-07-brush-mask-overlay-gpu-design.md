# ferrolite — Brush-mask overlay: GPU tint, no readback (design)

> **Status:** Design — pending user review, then writing-plans.
> **Date:** 2026-07-07
> **Branch:** `fix/brush-mask-perf` (off `main`).
> **Parent / builds on:** `2026-07-05-p1-masking-design.md` (§5.3 the `LocalAdjustmentsNode`,
> §6 two-tier recompute, §9.3 the canvas mask overlay). The P1 Plan-4 work shipped the overlay
> with a readback-based path and left an explicit perf note (see `app.rs`
> `rebuild_mask_overlay_if_needed`) that this design resolves.
> **Goal:** make Develop brush-mask painting smooth (no per-frame stall) by removing the overlay's
> synchronous GPU→CPU readback — the measured dominant cost — without changing what the overlay
> looks like or touching the edit pipeline.

---

## 1. Problem & measured diagnosis (measure-before-fix)

Painting a brush mask in Develop is too laggy for professional use. Before proposing anything, the
three suspect per-frame costs were instrumented (env-gated `FERROLITE_BRUSH_PROFILE`, zero-cost when
off) and measured on a live stroke in a **release** build:

| Cost (per dragged frame) | Range | Verdict |
|---|---|---|
| **`readback_wait`** — the `device.poll(Wait)` in `read_mask_r32f` | **1.5–14.8 ms** (median ~5–9, spikes to 15) | **DOMINANT** |
| `composite_submit` — encode + submit the mask composite | 0.3–1.8 ms | cheap, async |
| `rgba` — CPU RGBA-tint rebuild + `ColorImage` | 0.7–4.8 ms (trended up over the session) | secondary |
| `upload` — egui texture upload | 0.01 ms | negligible |
| `preview ep.evaluate()` — preview edit-pipeline recompute | 0.2–1.1 ms | negligible |

Node count climbed 1 → 44 over the stroke; **every** cost stayed flat across that range.

**Conclusions (evidence, not guesses):**

1. **The bottleneck is the overlay's synchronous GPU→CPU readback**, full stop. `readback_wait` is
   5–20× `composite_submit`, dominates the frame, is highly variable (→ the felt stutter), and the
   overlay total (`coverage` + `rgba`) routinely exceeds a 16 ms frame budget. Because the readback
   does a blocking `poll(Wait)`, it drains the *entire* frame's queued GPU work (including the
   preview evaluate, whose 0.3 ms is CPU-submit-only) — it is serializing all GPU work
   synchronously on the UI thread.
2. **The preview pipeline recompute is NOT the problem** (0.2–1.1 ms). `LocalAdjustmentsNode`,
   `set_preview_and_full`, and the OpStack clones are exonerated.
3. **The non-incremental O(all-dabs) re-raster is NOT the felt lag.** `composite_submit` was flat
   (<2 ms) from 1 → 44 nodes. The unused `StrokeCursor` / incremental-stamping machinery the P1
   design emphasized would optimize a cost that is not hurting at preview resolution.

The fix therefore targets **only** the overlay readback.

---

## 2. Root cause

`ferrolite-app::App::rebuild_mask_overlay_if_needed` (the UI-thread, per-frame overlay refresh) does,
on every dragged frame during a stroke (the rebuild key changes each dab):

```
MaskOverlayCompositor::coverage(def, input)
  → MaskCompositor::composite(..)         // GPU, async submit  (cheap)
  → read_mask_r32f(ctx, &buf)             // queue.submit + device.poll(Wait)  ← STALL
                                          //   + CPU copy/unpad of ~512² f32
overlay_rgba(&cov, 0.5)                    // fresh ~700 KB RGBA Vec, per-pixel convert
egui::ColorImage::from_rgba_unmultiplied   // + ctx.load_texture (fresh egui texture)
```

The coverage buffer is produced on the GPU and then round-tripped GPU→CPU→GPU purely to paint a red
tint over the image. The readback (`poll(Wait)`) is the stall; the CPU RGBA rebuild is the secondary
cost. Both are inherent to a readback-based overlay and cannot be tuned away — they must be removed.

---

## 3. Fix: tint on the GPU, hand egui the wgpu texture directly

Keep the composited mask on the GPU, tint it red **on the GPU**, and give egui the resulting wgpu
texture as a *native* texture. egui draws it with the same `ui.painter().image(..)` call as today, so
coordinate mapping (zoom/pan/crop) and alpha blending are byte-for-byte unchanged — but there is **no
readback, no CPU RGBA rebuild, and no per-frame `ColorImage` upload**.

**Hard constraint:** `egui_wgpu::Renderer::register_native_texture` requires the texture format be
`Rgba8UnormSrgb`, which is **not** storage-bindable. So the tint is produced by a **render pass**
(fullscreen triangle sampling the coverage), not a compute/storage write.

### 3.1 `ferrolite-pipeline` — `MaskOverlayCompositor` produces a GPU texture, not a CPU buffer

- Add a **build-once** tint render pipeline (`Rgba8UnormSrgb` color target, fullscreen-triangle vertex
  shader + fragment shader). The fragment shader samples the R32F coverage (nearest) at its UV and
  outputs **premultiplied** red:

  ```
  cov = textureSample(coverage, samp, uv).r        // [0,1]
  a   = clamp(cov, 0, 1) * STRENGTH                 // STRENGTH = 0.5 (matches today's 50% tint)
  out = vec4(a, 0.0, 0.0, a)                        // premultiplied: rgb = red * a
  ```

  Premultiplied because egui's image pipeline blends premultiplied alpha; this reproduces today's
  `overlay_rgba(cov, 0.5)` + `from_rgba_unmultiplied` result.

- Replace `coverage(ctx, def, input) -> (u32, u32, Vec<f32>)` with
  `overlay_texture(&self, def, input) -> OverlayTexture` where `OverlayTexture` wraps an
  `Arc<wgpu::Texture>` (+ its `TextureView`, dims), format `Rgba8UnormSrgb`. Body: composite the
  `MaskDefinition` (existing `MaskCompositor`, already on GPU) → run the tint pass into the sRGB
  target → return the texture. **No `read_mask_r32f`, no `Vec<f32>`.** The pipeline is built once in
  `new()` and reused (CLAUDE.md §2).
- The tint pass reuses the same bounded input the app already supplies (≤ `OVERLAY_MAX_EDGE`, ≤512²),
  so the output texture is ≤512²; egui upscales it with a LINEAR sampler when drawn over the image
  rect — identical to today's `TextureOptions::LINEAR` on the 512² `ColorImage`.
- `read_mask_r32f` stays in `ferrolite-mask` (still used by unit/golden tests and available for A2's
  future needs); only the overlay stops calling it.

### 3.2 `ferrolite-app` — register/update a native egui texture

- `ViewerState` gains `mask_overlay_native: Option<egui::TextureId>` (replacing
  `mask_overlay_tex: Option<TextureHandle>`).
- `rebuild_mask_overlay_if_needed` keeps its **exact** rebuild-key logic (hash of committed def +
  `preview_component` + `opstack_version`) and its **exact** gates (mask selected, tool active). On a
  rebuild:
  1. `overlay_texture(..)` → the sRGB wgpu texture (kept alive on `ViewerState`, e.g.
     `mask_overlay_gpu: Option<OverlayTexture>`, so its `Arc<wgpu::Texture>` outlives the frame).
  2. `rs.renderer.write()`:
     - first build (or after `free`): `register_native_texture(device, &view, FilterMode::Linear)`
       → store the `TextureId`.
     - subsequent rebuilds: `update_egui_texture_from_wgpu_texture(device, &view, Linear, id)` —
       **reuse** the same `TextureId`. Registering per frame would leak `TextureId`s.
  3. Store the `TextureId` in `mask_overlay_native`.
- **Lifecycle / freeing:** call `renderer.free_texture(&id)` and clear `mask_overlay_native` +
  `mask_overlay_gpu` when the selected mask is cleared (the existing early-return branch that today
  sets `mask_overlay_tex = None`) and on image close/replace. `image_id`-guarded like the other
  viewer GPU holders.
- The mask composite + tint still run on the UI thread via the established
  `GpuContext::from_render_state(rs)` ad-hoc-context pattern, but both are **async submits with no
  wait** (~1–2 ms total), which honors CLAUDE.md §1 (nothing that blocks the UI thread).

### 3.3 `ferrolite-app::develop::mask_overlay::show` — draw the native texture

- The fill branch changes from a borrowed `TextureHandle` to the native `TextureId`:
  `ui.painter().image(tex_id, image_rect, Rect(0..1), Color32::WHITE)`. The `overlay_on && !adjusting`
  gate, the selection gates, and every tool-affordance route are **unchanged**.
- `show`'s `overlay_tex` parameter type changes from `Option<&egui::TextureHandle>` to
  `Option<egui::TextureId>` (a `Copy` id); the call site in `app.rs` passes `v.mask_overlay_native`.

---

## 4. What is explicitly preserved

- **Live prospective-component preview** (the Components window Add section tuning a Luma/Color
  component) — still composites the prospective def; only the transport to screen changed.
- **`adjusting` suppression** (overlay hidden while a Light+Color slider of the mask is dragged).
- **Source-anchored alignment** across crop/rotate/aspect — the overlay is still drawn stretched over
  `image_rect` with uv 0..1, exactly as before.
- **Preview-tier only** overlay (unchanged; full-res tiled tier untouched).
- **Device-loss recovery** — the tint pipeline is build-once and rebuilt on `GpuContext` recreation
  like the other pipelines (never per edit).
- **Undo/redo, persistence, mask semantics** — untouched (this is a presentation-path change only).

## 5. Non-goals (data-justified)

- **Preview pipeline / `LocalAdjustmentsNode` / OpStack clones** — measured negligible; not touched.
- **Incremental `StrokeCursor` stamping** — `composite_submit` flat across 1→44 nodes; out of scope.
  Noted as a *possible future* full-res-tier optimization only (where dab counts and mask resolution
  are far larger).
- **The double `serde_json` hash in the rebuild key** — mildly wasteful (O(nodes)) but not the
  bottleneck; left as-is to keep the change focused. Optional micro-opt for a later pass.
- **Moving the composite off the UI thread** — unnecessary: the submits are async and ~1–2 ms.

## 6. Error handling

- **No render state / no GPU context** (`frame.wgpu_render_state()` is `None`) → early return, no
  overlay this frame (as today).
- **No overlay input yet** (`preview_source` absent) → early return (as today).
- **Tint-pipeline / device loss** → wgpu error scopes recreate `GpuContext` + pipelines once on
  recovery (reuses the Spec 1/2 recovery path); the overlay rebuilds on the next frame.
- **Mask deselect / image close** → free the native `TextureId`, drop the GPU texture; never leak
  `TextureId`s or dangle a texture from a closed viewer (`image_id` guard).
- **Empty / degenerate mask** → the compositor already yields an identity/zeroed coverage; the tint
  pass outputs `a = 0` (fully transparent) → nothing painted. Never panics.

## 7. Testing (TDD; CLAUDE.md gate, then hold for the author's visual test)

**Pure CPU logic:**
- The tint mapping `coverage → premultiplied sRGB red` as a pure function (mirrors the WGSL): `cov=0
  → (0,0,0,0)`; `cov=1 → (0.5,0,0,0.5)`; monotonic; clamped. Keeps the shader honest against a CPU
  reference, matching the existing `dab_alpha` / `composite_dabs` mirroring discipline.

**Golden-image GPU diff (auto-skip when `GpuContext::headless()` is `None`):**
- The tint render pass output for a known coverage buffer (e.g. a linear-gradient coverage) vs a
  committed reference `Rgba8UnormSrgb` PNG — proves premultiplied red + alpha are correct.

**egui / integration:** `cargo build` + clippy; no golden test for egui rendering.

**Measure-after (the real proof of the fix):** re-run with `FERROLITE_BRUSH_PROFILE=1` on a live
stroke and confirm the `readback` and `rgba` probe lines are **gone** and per-frame overlay cost
collapses to the ~1–2 ms async-submit range (well inside a 16 ms frame budget). Then remove the
temporary instrumentation (or leave it gated behind the env flag if useful for regression watch —
decided in the plan).

**Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace` green → **then STOP and hold for the author's (Jann's) hands-on visual
test**: paint several long strokes at small and large brush sizes and confirm smooth, stall-free
painting with the red overlay tracking the brush live; verify the overlay still hides while dragging
a mask adjustment slider, still shows the live prospective-component preview, and still aligns under
crop/rotate/zoom (CLAUDE.md "Finishing a branch" rule).

## 8. Decomposition into implementation plans

One branch (`fix/brush-mask-perf`), a single writing-plans → TDD cycle (the change is small and
cohesive):

1. **Tint pass in `ferrolite-pipeline`.** The build-once `Rgba8UnormSrgb` tint render pipeline +
   shader; `MaskOverlayCompositor::overlay_texture` replacing `coverage`; the pure tint-mapping unit
   test + the golden GPU diff. (`read_mask_r32f` retained in `ferrolite-mask`.)
2. **App wiring.** `ViewerState` fields (`mask_overlay_native` / `mask_overlay_gpu`);
   `rebuild_mask_overlay_if_needed` register/update-in-place + free-on-deselect/close;
   `mask_overlay::show` native-`TextureId` draw + the `show` signature change.
3. **Verify + cleanup.** Measure-after with the probe; workspace gate green; remove/settle the
   temporary `FERROLITE_BRUSH_PROFILE` instrumentation; hand over the visual test plan and HOLD.

---

## 9. Decisions recorded (2026-07-07)

| Question | Decision | Rationale |
|---|---|---|
| Which cost to fix | **The overlay GPU→CPU readback only** | Measured dominant (1.5–15 ms/frame); preview recompute (0.3 ms) and re-raster growth (flat) exonerated by the probe. |
| How to remove it | **Tint on GPU + hand egui the wgpu texture** (`register/update_native_texture`) | Removes readback (dominant) *and* CPU RGBA rebuild (secondary) *and* per-frame upload; reuses egui's image draw so coordinates/blend are unchanged. The option deferred at P1 plan time. |
| Tint pass kind | **Render pass into `Rgba8UnormSrgb`** (not compute/storage) | egui native textures must be `Rgba8UnormSrgb`, which is not storage-bindable. |
| Native texture lifecycle | **Register once, `update_..` in place, `free_texture` on deselect/close** | Registering per frame leaks `TextureId`s; reuse keeps it bounded. |
| Incremental stamping | **Out of scope** | `composite_submit` flat 1→44 nodes; not the felt lag. Future full-res-tier item only. |
| Composite thread | **Stays on UI thread (async submit, ~1–2 ms)** | No wait → no block; honors CLAUDE.md §1 without extra machinery. |
