# ferrolite — Plan 4: Viewer & VT Ladder (design)

> **Status:** Design — approved by user; pending writing-plans.
> **Date:** 2026-06-29
> **Parent:** `2026-06-28-ferrolite-speed-core-design.md` (Spec 1, §5 two-tier load + §6 VT ladder)
> and `2026-06-28-ferrolite-v1-architecture-map.md` (§4 decomposition, §5 cross-cutting
> contracts, §6 two-tier load path). Read those first for settled decisions and seams.
> **Proves:** G2 (load/preview speed) — specifically time-to-first-pixel, time-to-full-res-
> interactive, and 1:1 zoom/pan frame time (benchmark metrics M3/M4/M5).
> **Builds on:** Plans 1–3 (foundation, decode + catalog, jobs + library grid) — merged.
> **Out of scope:** watcher/reindex/tags (separate track); all edit ops, histogram, color
> management, export (Specs 2–3).

---

## 1. Goal

Stand up the single-image **viewer** and the **sparse virtual texture (VT)** that backs it,
completing the two-tier load path:

> open image → instant embedded-preview display → full `rawler` decode → display-linear RGB →
> sparse-virtual-texture full-res view with smooth GPU zoom/pan and a preview→full crossfade.

This is Plan 4 of 5 in Spec 1. Plans 1–3 delivered the job system, decode, catalog, and the
Library grid. Plan 4 delivers the two new engine-transferable crates (`ferrolite-gpu`,
`ferrolite-vt`), the photo-domain CFA→RGB display glue, and the app-side viewer.

**Definition of done (user-approved):** the **full 4-rung VT ladder** (single-texture →
static tiled+mip → residency+LRU → page-table + GPU feedback pass), the two-tier load wired
into the viewer, and the workspace gate green (`cargo fmt --check` + `cargo clippy --workspace
--all-targets -- -D warnings` + `cargo test --workspace`). The full sparse VT is the explicit
target here because engine-architecture learning is the author's stated priority; the parent
spec's rung-3 fallback remains available only if rung 4 proves intractable.

---

## 2. Scope

**In:**
- `ferrolite-image` — shared tile/float vocabulary additions (engine tier, permissive).
- `ferrolite-gpu` (**new**) — `GpuContext` + generic photo-agnostic retained-DAG executor.
- `ferrolite-vt` (**new**) — sparse virtual texture, rungs 1→4, source-agnostic tile streaming.
- `ferrolite-decode` — `DemosaicToRgb16f` trait + `QuadBin` impl (CFA→display-linear RGB).
- `ferrolite-app` — viewer surface, two-tier load, zoom/pan input, crossfade, navigation cancel.

**Out (later specs / separate track):** the Develop *module* and all edit ops, sparse-VT tile
overlap/halo for neighborhood ops, color management, export, histogram (Specs 2–3); library
watcher/reindex/tags (separate track). Spec 1 ships **display-only** tiling: **no tile
overlap/halo**.

---

## 3. Crate layout, licensing tiers & dependency graph

The architecture map's tier boundary is preserved: engine-transferable crates carry only
permissive deps and stay relicensable; photo-domain crates may pull LGPL/GPL and keep the
binary GPL-3.0.

```
ferrolite-image   (engine tier, permissive)   +tile/float vocabulary
   ▲
ferrolite-gpu     (engine tier: wgpu)          NEW — GpuContext + retained-DAG executor
   ▲                                           deps: wgpu, bytemuck, half, ferrolite-image
ferrolite-vt      (engine tier: wgpu)          NEW — sparse virtual texture (rungs 1–4)
   ▲                                           deps: ferrolite-gpu, ferrolite-jobs,
   │                                                 ferrolite-image, fast_image_resize, half, bytemuck
ferrolite-decode  (photo tier: rawler/LGPL)    +DemosaicToRgb16f trait + QuadBin
ferrolite-app     (GPL binary)                 viewer surface, two-tier load, input
```

New permissive deps for the engine tier: `bytemuck` (POD casts for GPU buffers), `half`
(`f16` for `Rgba16Float` uploads). Both are already resolved in `Cargo.lock` (transitive
today). `wgpu 22.1`, `egui/eframe 0.29.1`, `fast_image_resize 6.0` are the pinned versions.

**Device ownership.** eframe owns the `wgpu::Device`/`Queue`. `ferrolite-gpu::GpuContext`
holds cloned `Arc`-handles obtained from `egui_wgpu::RenderState` via
`GpuContext::from_render_state(&RenderState)` for the app path. For tests it offers
`GpuContext::headless() -> Option<GpuContext>`, which requests a fallback adapter and returns
`None` when none exists (headless CI). `ferrolite-gpu` never creates the app's device.

---

## 4. `ferrolite-image` — shared vocabulary additions

Zero photo/GPU coupling; pure data + math, fully unit-tested without a GPU.

- `const TILE_SIZE: u32 = 256;`
- `struct TileCoord { lod: u32, x: u32, y: u32 }` — a virtual tile address.
- `struct LinearRgbaF32 { width: u32, height: u32, pixels: Vec<f32> }` — the CPU-side
  display-linear RGBA image the LOD pyramid is built from. `pixels.len() == width*height*4`,
  validated by a constructor mirroring the existing `ImageBuffer::new` contract. Kept `f32`
  on the CPU; converted to `f16` only at GPU upload (in `ferrolite-gpu`/`ferrolite-vt`).
- Pure helpers (tested): `pyramid_level_count(width, height)` (levels until the top fits one
  tile), `tiles_per_level(width, height, lod)`, `tile_pixel_origin(coord)`,
  `pixel_to_tile(x, y, lod)`. These are the math the VT and pyramid rely on; isolating them
  here keeps residency logic testable without GPU or photo concepts.

---

## 5. `ferrolite-gpu` (new, engine tier)

### 5.1 `GpuContext`
A thin wrapper over the wgpu device: `Arc<Device>`, `Arc<Queue>`, adapter info, plus small
conveniences (`create_render_pass`, `create_compute_pass`, `upload_texture`, an offscreen
render-target helper used by golden tests). Constructed from `RenderState` (app) or
`headless()` (tests). No retained state beyond the handles.

### 5.2 Generic retained-DAG executor (honors cross-cutting contract §4)
Photo-agnostic. Knows nothing about images, tiles, or wgpu — it is plain graph machinery so
it is understandable and testable in isolation, and Spec 2's photo edit nodes slot in without
reaching into internals.

- `trait Node { fn evaluate(&self, inputs: &[&NodeOutput]) -> NodeOutput; }` in an
  object-safe (`dyn`) form, with an associated generic output type parameter on the graph so
  the executor itself carries no photo vocabulary.
- `struct Graph { add_node(node) -> NodeId, connect(src, dst), mark_dirty(NodeId),
  evaluate(NodeId) -> &Output }`.
- **Dirty-flag invalidation + cached outputs:** a clean node returns its cached output; a
  dirty node re-evaluates, re-caches, and propagates dirtiness to dependents. Evaluation is a
  topological walk of the requested node's transitive inputs.
- **Tested with toy arithmetic nodes only** (e.g. constant/add/multiply): correct topo order,
  cache reuse on a clean re-evaluate, dirty propagation to dependents, diamond dependencies.
  No GPU, no photo concepts. Spec 1's display path does not require the executor in its hot
  loop; building it now proves contract §4 and de-risks Spec 2.

---

## 6. `ferrolite-vt` (new, engine tier) — sparse virtual texture

Source-agnostic per cross-cutting contract §5: it streams tiles for *any* large RGBA image
source and never knows what produced the pixels. Built as the four de-risking rungs; each
rung is independently demoable.

### 6.1 Tile source seam
```
trait TileSource {
    fn level_count(&self) -> u32;
    fn level_size(&self, lod: u32) -> (u32, u32);
    fn tile(&self, coord: TileCoord) -> LinearRgbaF32;   // 256² , edge-clamped at borders
}
```
The app supplies `PyramidTileSource`, built from one demosaiced `LinearRgbaF32` full image by
`fast_image_resize` (downsample per LOD). The VT depends only on the trait.

### 6.2 Pure CPU residency core (tested without a GPU)
Extracted as free functions / a plain struct so the streaming logic is verifiable headless:
- `needed_tiles(viewport, zoom, source_dims, budget) -> Vec<TileCoord>` — which virtual tiles
  + LODs the current view requires (LOD chosen from on-screen texel density).
- `ResidencySet` with LRU ordering under a tile-count budget derived from the VRAM budget:
  `touch`, `insert -> Option<evicted>`, `diff(needed) -> (to_load, to_evict)`.

### 6.3 Rungs
1. **Single-texture viewer.** Whole image uploaded as one `Rgba16Float` texture; a sampling
   shader does zoom/pan. Proves the egui↔wgpu canvas + interaction. **Also the fallback path**
   (tiny-preview cameras, or images small enough to skip tiling).
2. **Static tile grid + mip pyramid (all resident).** Tiled sampling with per-fragment LOD
   selection from screen-space derivatives; smooth zoom/pan. **← G2 validated here.**
3. **Residency + LRU eviction under VRAM budget.** A physical tile pool (2-D atlas of
   `Rgba16Float` slots; configurable budget ~1–2 GB of a 6–8 GB GPU); CPU residency set; tile
   loads submitted as **`ferrolite-jobs` jobs at Visible priority**; LRU eviction; the display
   shader **falls back to a coarser resident LOD** for missing tiles (blurry→sharp, never
   blocks). Handles images larger than the budget.
4. **Page-table indirection + GPU feedback pass (full engine-style sparse VT).** An
   indirection texture (`Rg32Uint`: physical slot + flags, per virtual tile per LOD); the
   display shader marks needed `(lod, tile)` into a **GPU feedback buffer**; the CPU reads it
   back **asynchronously** (`map_async`, one frame latent), diffs against the resident set,
   enqueues missing-tile load jobs, and evicts LRU under budget. This is the engine-learning
   payload.

### 6.4 Public API
`request_view(viewport, zoom) -> ()` updates residency + issues load jobs; a paint entry
binds the page table + physical pool and records the display draw into the egui-provided
`wgpu::RenderPass`. Tile uploads land on the queue between frames.

---

## 7. Demosaic seam — `ferrolite-decode` (photo tier)

`decode_full` currently returns raw CFA/Bayer `u16` samples (`cpp = 1`) and discards the
white-balance / black-level context. CFA→RGB is photo-domain (it needs rawler's WB coeffs and
black/white levels), so it lives here, **not** in the engine-tier crates.

- Extend the full-decode product (or add a sibling) to also surface **CFA pattern**, **black
  & white levels**, and **camera WB coefficients** from rawler.
- `trait DemosaicToRgb16f { fn to_linear_rgba_f32(&self, raw: &RawDecoded, params: &DemosaicParams) -> LinearRgbaF32; }`
- **`QuadBin`** (Plan 4 default): each 2×2 RGGB quad → one pixel `(R, (G1+G2)/2, B)`; apply
  black-level subtraction → multiply by WB coeffs → normalize to display-linear `[0,1]`.
  Halves each dimension (24MP → ~6MP), produces **zero demosaic artifacts**, is trivially fast.
  Output is **display-linear** — the sRGB OETF (gamma) is applied in the display shader, not
  baked, so Spec 3 can substitute a color-managed path. Documented as a Spec-3 placeholder.
- A full-resolution **`Bilinear`** impl is a future drop-in behind the same trait with no
  call-site change.

A non-RGGB CFA pattern falls back to rung 1 with a coarse/quarter decode if QuadBin's RGGB
assumption does not hold (logged, never panics).

---

## 8. Two-tier load + viewer — `ferrolite-app`

### 8.1 Viewer surface
- Add `viewer: Option<ViewerState>` to `AppState`. **Double-click** a grid cell (or **Enter**
  on the selected cell) opens it; **Esc** returns to the grid. While `Some`, the central panel
  renders the wgpu viewer instead of the grid. This stays within the **Library module**; the
  Develop *module* remains a Spec-2 stub (the existing gradient canvas).
- `ViewerState` holds: the open image id/path, the tier-1 preview texture, the VT (once built),
  the current zoom/pan transform, the crossfade animation factor, and the live decode/tile job
  handles for cancellation.

### 8.2 Two-tier load (drives G2)
1. **Tier 1 — preview.** Submit an **Interactive** `decode_preview` job → upload the upright
   preview as a texture → display fit-to-screen. First-pixel target **< ~150 ms**.
2. **Tier 2 — full.** In parallel submit an **Interactive** `decode_full` job → `QuadBin` →
   `LinearRgbaF32` → `PyramidTileSource` → feed `ferrolite-vt`.
3. **Crossfade** preview→full (~150 ms) once the **current view's** tiles are resident — no pop.
4. Zoom/pan thereafter drives the VT (§6).
- **Fallback:** a camera that embeds only a tiny thumbnail → tier 1 uses a fast half/quarter
  `decode_full` rendered through rung 1 instead of the embedded preview.

### 8.3 Navigation cancels superseded work (cross-cutting contract §1)
Opening image B, or closing the viewer, **cancels** image A's pending decode + tile-load jobs
via their `CancelToken`s. The current-image decode and current-view tiles always preempt
background thumbnail backfill (Interactive/Visible > Background).

### 8.4 Input
- Scroll = zoom about the cursor; left-drag = pan; double-click = toggle fit ↔ 1:1.
- The pan/zoom transform math (clamp to bounds, zoom-about-point, fit computation) is a **pure
  tested unit**, independent of egui.

### 8.5 Status bar
The existing "GPU: idle/busy" already binds to job-system activity; VT tile loads are jobs, so
the indicator lights during streaming with no extra wiring. "N indexed" / EXIF bindings are
unchanged.

---

## 9. Error handling

- **GPU device/surface loss** → wgpu error scopes → recreate `GpuContext` + VT pools, then
  re-request the visible tiles.
- **Tile-upload OOM** → shrink the pool budget + apply backpressure (defer pending loads).
- **No usable embedded preview** → tier-1 fallback to a fast half/quarter `decode_full` via
  rung 1.
- **Corrupt / failed full decode** → the viewer keeps showing the tier-1 preview; a status
  note; never panics. Decode and tile jobs run under the existing panic-isolated worker
  boundary, so one bad file never downs the pool.
- **Unexpected CFA pattern** (non-RGGB) → rung-1 coarse-decode fallback, logged.

---

## 10. Testing

Mirrors the parent spec §10 (golden-image GPU diffs + pure CPU logic), adapted to the CI
constraint below.

- **Pure CPU (run on every OS in CI — the 80%-coverage target):**
  - VT residency: `needed_tiles` for fixed viewport+zoom; LRU `diff`/eviction order.
  - Pyramid + tile math in `ferrolite-image` (level count, tiles-per-level, pixel↔tile).
  - `QuadBin`: synthetic Bayer input → expected RGB (black-level, WB, binning).
  - Retained-DAG executor: topo order, cache reuse, dirty propagation, diamonds.
  - Pan/zoom transform math (zoom-about-point, fit, clamping).
  - `LinearRgbaF32` length validation.
- **Golden-image GPU diffs:** a headless `GpuContext` renders fixed `(image, viewport, zoom)`
  cases to an offscreen `Rgba8` target → compared to committed reference PNGs with a small
  per-pixel tolerance (absorbs driver float diffs). Covers rungs 1–4 display output and LOD
  fallback.
- **CI constraint (hard):** the CI workflow runs `cargo test --all` on
  ubuntu/macos/windows **with no GPU or software rasterizer installed**. Therefore every GPU
  test **must skip gracefully when `GpuContext::headless()` returns `None`** (log + return
  `Ok`), so `cargo test --workspace` stays green in CI. Golden references are authored and
  verified locally on the dev GPU (RTX 3060/3070 class).
- **Coverage:** 80%+ on non-GPU logic; GPU passes covered by golden diffs rather than line
  coverage.

---

## 11. Build order & rung gates (TDD throughout)

1. `ferrolite-image` vocabulary additions (pure; tests first).
2. `ferrolite-gpu`: `GpuContext` + retained-DAG executor (executor fully tested now).
3. `ferrolite-decode`: `DemosaicToRgb16f` + `QuadBin` (synthetic-Bayer tests).
4. `ferrolite-vt` rung 1 → rung 2 (**G2 gate: smooth tiled zoom/pan**) → rung 3 → rung 4. Each
   rung is demoable; CPU residency logic tested per rung; golden diffs added from rung 2.
5. `ferrolite-app`: viewer surface + input → two-tier load wiring + crossfade + navigation
   cancel.
6. **Workspace gate green:** `cargo fmt --check` + `cargo clippy --workspace --all-targets
   -- -D warnings` + `cargo test --workspace`. Then finish the branch.

---

## 12. Decisions recorded (resolved during brainstorming, 2026-06-29)

| Question | Decision | Rationale |
|---|---|---|
| VT ladder scope for Plan 4 | **All 4 rungs** (full sparse VT incl. GPU feedback pass) | Engine-architecture learning is the author's explicit priority; rung-3 fallback retained only if rung 4 proves intractable. |
| CFA→RGB display conversion | **`QuadBin` now, behind a `DemosaicToRgb16f` trait; `Bilinear` later** | Lowest risk now (clean, fast, no artifacts at half-res); no call-site rework when fidelity is raised in Spec 2/3. |
| GPU executor scope | **Working minimal generic retained-DAG executor** | Honors contract §4 concretely and gives Spec 2 a warm start; cheap to build and fully testable headless. |
| Demosaic crate placement | **`ferrolite-decode`** (photo tier) | Needs rawler WB/black-level context; must not pollute the relicensable engine tier. |
| Golden GPU tests in CI | **Auto-skip when no adapter** | CI has no GPU; `cargo test --workspace` must stay green. Goldens verified on the dev GPU. |
