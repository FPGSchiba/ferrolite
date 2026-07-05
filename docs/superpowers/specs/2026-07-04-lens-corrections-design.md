# ferrolite — Spec 4.4: Lensfun lens corrections (design)

> **Status:** Design — pending user review (2026-07-04); then writing-plans.
> **Date:** 2026-07-04
> **Parent map:** `2026-07-03-spec4-secondary-and-polish-map.md` (§4 **Spec 4.4**, §3 licensing
> tiers, §5 cross-cutting contracts — read first for the settled seams).
> **Predecessor:** `2026-06-30-spec2-editing-design.md` (the edit DAG, `OpStack` sidecar
> persistence, the `Geometry` resampling op, and the **VT tile halo + GPU tile producer** — the
> neighbourhood/geometry infrastructure this feature reuses).
> **Proves:** an image opened with EXIF lens metadata auto-matches a Lensfun lens; the user opts
> into **distortion**, **transverse CA**, and **vignetting** corrections (each with an Amount
> control); the corrections apply on the GPU at preview- and full-res-tiled tiers, persist to the
> `.xmp` sidecar, and reload on the next open. Never panics.
> **UI target:** the **Develop module** 296px right adjustment panel — a new "Lens Corrections"
> section (`docs/design/ferrolite-design-system.md` §6).
> **Branch:** `feat/lens-corrections` (off `main`).

---

## 1. Goal & validation

Add a new **geometry-class correction stage** driven by the **pure-Rust `lensfun` crate** and the
image's lens metadata:

> open an image → its EXIF (make/model/lens/focal/aperture) **auto-matches** a Lensfun lens and the
> matched name is shown, but corrections stay **off** → the user opts into **Distortion**,
> **Transverse CA**, and/or **Vignetting** (each an on/off toggle **plus** an Amount slider) → the
> corrections apply on the GPU, fused into the existing geometry resample (single resample) at
> preview-res interactively and full-res via the tiled VT producer → a searchable **camera + lens
> picker** overrides a failed or wrong auto-match → the selection + toggles persist in the `.xmp`
> sidecar and reload on the next open → any failure (no EXIF, no match, unsupported model, DB load
> failure) leaves the image unchanged and is logged, never panics.

Image quality remains **secondary** to speed/architecture (map §3.3). The deliverable is the
*architecture*: an **off-thread bake** (Lensfun `Modifier` → a coarse GPU-ready warp grid +
vignetting gain map) feeding a **single fused GPU resample**, non-destructive and sidecar-persisted
— **not** a lens-science parity effort. This is *correction* driven by Lensfun's database, not lens
simulation and not our own lens models (map §4.4 "Out").

---

## 2. Scope

**In:**
- **New crate `ferrolite-lens`** (photo tier) — wraps the pinned `lensfun` crate behind our own
  adapter (`LensDb` load + match; `bake_geometry` / `bake_vignetting` / `lens_halo`). Pure
  (no GPU/UI); the only place that touches the pre-alpha `lensfun` API.
- **`ferrolite-decode`** (photo tier) — surface lens/camera metadata as an **additive** decode
  product (contract §3). `Metadata` already carries `make`/`model`/`lens`/`focal_length`/`aperture`
  (`ferrolite-decode/src/metadata.rs`); add `crop_factor: Option<f32>` (and derive an effective
  crop factor when absent). No other decode product changes.
- **`ferrolite-pipeline`** (photo tier) — a new `Op::LensCorrection` variant + params + accessors +
  per-op reset; the **fused warp** into the geometry resample (`geometry.wgsl` + `GeometryHeadNode`);
  a small **vignetting gain** pass; the `LensCorrection`-aware halo; the DAG wiring (a full rebuild
  on lens/enable/context change, uniform-only on Amount change).
- **`ferrolite-app`** (photo tier) — a Develop "Lens Corrections" panel section (matched-lens label,
  searchable camera+lens picker, three toggle+Amount rows with per-control resets, advanced
  focal/aperture override); the off-thread **bake job** on `ferrolite-jobs` + event delivery + GPU
  upload; wiring slider/toggle → `OpStack` → DAG → repaint.
- **`ferrolite-catalog`** — **no change**: the new op variant serializes in the existing `frl:ops`
  JSON payload via serde automatically (`ferrolite-pipeline/src/serialize.rs`).

**Out (non-goals / later):**
- **Creative / simulated lens effects** — this is *correction* only (map §4.4 "Out").
- **Building our own lens models** — Lensfun's DB is the sole source; unmatched lenses get no
  correction (identity), never a guessed model.
- **Manual defringe / manual-CA sliders** beyond the DB-driven transverse-CA correction.
- **Perspective / keystone / upright auto-level** — not a Lensfun-DB correction; out of scope.
- **A network lens-DB updater / user-supplied calibration** — v1 ships the **bundled** DB only
  (§4.5); updatability is noted as future work, not built.
- **Distortion-aware crop auto-expand** (auto-fill the black corners barrel correction can expose) —
  the corrected image may show edge padding; a v1 acceptable tradeoff (§7), not auto-cropped.

---

## 3. Architecture of the slice

```
ferrolite-app  (Develop module, photo tier)
  "Lens Corrections" section: matched-lens label · camera+lens picker (override) ·
     Distortion [x]+Amount · Transverse CA [x]+Amount · Vignetting [x]+Amount · focal/aperture (adv)
  slider/toggle → new OpStack (Op::LensCorrection) → mark node dirty / request rebuild → repaint
   │                                       │
   │  lens/enable/context changed:         │  Amount changed only:
   │  ferrolite-jobs bake task (contract §1)│  uniform update, NO re-bake, NO rebuild
   ▼                                       ▼
ferrolite-lens  (photo tier, pure / testable — no GPU/UI)
  LensDb::load_bundled()  ·  match(make,model,lens,focal,aperture,crop) -> Option<LensMatch>
  bake_geometry(&match, focal, crop, grid_n) -> WarpGrid { n, per-channel (u,v) source coords }
  bake_vignetting(&match, focal, aperture)   -> VignetteMap { radial gain }
  lens_halo(&WarpGrid) -> u32   (max source displacement, capped)
     (all via a wrapped lensfun::Modifier: apply_geometry_distortion + subpixel + color_modification)
   │  { WarpGrid, VignetteMap, halo, resolved_name }  OR  None (logged),  over the app event channel
   ▼  on receipt: upload warp-grid + vignette textures; set halo; rebuild the tile producer; repaint
ferrolite-pipeline  (photo tier) — Approach A: FUSED single resample
  geometry resample (geometry.wgsl / GeometryHeadNode): compose crop/rotate → undistorted coord
     → sample WarpGrid (per channel = TCA) → ONE bilinear fetch per channel   ← single resample
  vignetting gain pass: per-pixel radial-gain multiply in scene-linear (near the head, pre-exposure)
  Amount = shader lerp uniform (identity↔full warp; gain↔1.0), applied without re-bake/rebuild
   │  produces PipelineImage on the UNCHANGED Graph<PipelineImage> (contract §4)
   ├── ferrolite-vt   — reuses the source-agnostic tile halo + GPU tile producer (contract §5);
   │                    the producer stays a SINGLE resampling head; halo = lens_halo (+ rotate)
   └── ferrolite-gpu  — Graph<O> executor UNCHANGED; pipelines built once + pre-warmed (CLAUDE.md)
```

**Licensing tiers (map §3.1) preserved.** All new logic is **photo tier**. `lensfun` is
**LGPL-3.0-or-later** (pure Rust, no C toolchain) + its bundled DB (a database under Lensfun's
data licence) — the binary is GPL-3.0 anyway, so this is fine, and it lives strictly in
`ferrolite-lens`. Engine crates (`ferrolite-gpu`/`ferrolite-vt`/`ferrolite-image`) are touched
**only** for a **generic warp-grid sample** capability if any is needed — no photo concepts, no
copyleft; the resampling shader itself lives in `ferrolite-pipeline` (photo tier) as it does today.
**No C toolchain is introduced → no build-gating decision is required** (contrast 4.2's libjxl).
The generic `Graph<PipelineImage>` executor is **not modified** (contract §4).

---

## 4. `ferrolite-lens` — adapter over the pure-Rust `lensfun` crate

A **new crate** whose sole job is to quarantine the **pre-alpha** `lensfun` crate behind a stable,
pure, testable surface. Pinned exact version; a thin trait so a future swap (C bindings /
hand-rolled) never touches `ferrolite-pipeline` or `ferrolite-app`.

### 4.1 Why a wrapper crate (not folded into `ferrolite-pipeline`)
- The `lensfun` crate is **pre-alpha ("API may still shift")** — isolating it means an upstream
  break is a one-crate fix, and our pipeline/UI depend only on *our* types (`WarpGrid`,
  `VignetteMap`, `LensMatch`), never on `lensfun`'s.
- Keeps `ferrolite-pipeline` free of DB/matching concerns (it consumes baked grids, nothing more).
- Mirrors the repo's existing seam discipline (`ferrolite-color` owns color math; the pipeline
  consumes its products).

### 4.2 Types (pure data; `Clone`; no GPU/UI/`unsafe`)
```rust
pub struct LensMatch { pub lens_id: String, pub display_name: String, pub crop_factor: f32 }

pub struct WarpGrid {           // coarse; sampled bilinearly on the GPU
    pub n: u32,                 // grid resolution (e.g. 129) — a tunable const
    pub coords: Vec<[f32; 6]>,  // per node: (u,v) source coord for R, G, B channels (TCA)
    pub max_disp: f32,          // max |source - dest| in pixels, over the grid → halo
}

pub struct VignetteMap { pub radial: Vec<f32> }  // 1D radial gain LUT (len = a tunable const)
```
- `WarpGrid.coords` stores **source** (sample) coordinates in normalized image space for each of
  R/G/B, so distortion (all channels equal) and transverse CA (channels differ) are one product.
- **Amount is NOT baked in.** The grid is the *full* correction; the shader lerps
  `mix(identity_coord, grid_coord, amount)` and `mix(1.0, gain, amount)`. So dragging an Amount
  slider is a uniform update — **no re-bake, no rebuild** (CLAUDE.md GPU rule; instant response).

### 4.3 API
```rust
pub trait LensDb {
    fn match_lens(&self, q: &LensQuery) -> Option<LensMatch>;   // EXIF → lens (auto)
    fn find_lenses(&self, camera: &str, needle: &str) -> Vec<LensMatch>;  // picker search
    fn bake_geometry(&self, m: &LensMatch, focal: f32, crop: f32, n: u32) -> Option<WarpGrid>;
    fn bake_vignetting(&self, m: &LensMatch, focal: f32, aperture: f32, len: u32)
        -> Option<VignetteMap>;
}
pub fn load_bundled() -> Result<impl LensDb, LensError>;        // Database::load_bundled()
pub fn lens_halo(g: &WarpGrid) -> u32;                          // ceil(max_disp), capped
```
- The concrete impl constructs a `lensfun::Modifier` at the given focal/crop/aperture and image
  grid dims, calls `enable_distortion_correction` / `enable_tca_correction` /
  `enable_vignetting_correction`, then `apply_geometry_distortion` (+ subpixel) over a **coarse
  `n×n` grid** (not full-res — the Modifier is built at grid resolution) to fill `coords`, and
  `apply_color_modification` sampled radially for `radial`.
- *Implementation detail deferred to the plan (not the spec):* the exact `lensfun` 0.7 call
  sequence and buffer layout — the crate mirrors the C++ `ApplyGeometryDistortion` /
  `ApplySubpixelDistortion` / `ApplyColorModification`, so this is feasible; the plan pins the
  precise API against the pinned version.

### 4.4 Matching
- `LensQuery { make, model, lens, focal, aperture, crop }` from `Metadata`.
- Auto-match: find the camera (make/model) then the lens (lens string) in the DB; on ambiguity,
  prefer an exact lens-name hit, else `None` (never a wrong guess). `crop_factor` comes from the
  matched camera when EXIF lacks it.
- Manual override: `find_lenses(camera, needle)` powers the picker's live search.

### 4.5 Database shipping (v1: bundled)
- v1 uses `lensfun`'s **bundled** DB (`load_bundled()`) — no external download, no updater. The DB
  is loaded **once** at startup (or first Develop entry) behind a `ferrolite-jobs` task if it proves
  non-trivial, cached for the session (contract §1). Binary-size cost (a few MB of lens data) is
  accepted. A network/user-supplied DB updater is explicit future work (§2 Out).

### 4.6 Tests (pure CPU, every OS in CI — the 80%+ target)
- `match_lens`: a known fixture EXIF (make/model/lens) resolves to the expected lens id; unknown →
  `None`; ambiguous → `None` (no wrong guess).
- `bake_geometry`: for a bundled fixture lens, corner/edge nodes match a direct `lensfun` CPU
  reference within tolerance; identity when the lens has no distortion model; `max_disp` sane.
- `bake_vignetting`: monotone radial falloff for a known fixture; identity when no model.
- `lens_halo`: `ceil(max_disp)` and capped at the shared max (mirrors `MAX_SHARPEN_RADIUS`).
- Amount-lerp helpers (`mix` toward identity / gain 1.0) if factored CPU-side.

---

## 5. `Op::LensCorrection` — model & op order (`ferrolite-pipeline`)

### 5.1 The op
A new variant on the existing `Op` enum (`ferrolite-pipeline/src/op.rs`) + a new `OpKind`
discriminant, pure param data, serde-serializable (round-trips in `frl:ops` with no
`ferrolite-catalog` change):
```rust
pub struct Correction { pub enabled: bool, pub amount: f32 }   // default { false, 1.0 }

pub struct LensCorrection {
    pub lens_id: Option<String>,   // resolved Lensfun key; None = unmatched (identity)
    pub focal_len: f32,            // capture context used for the bake (EXIF, user-overridable)
    pub aperture: f32,
    pub crop_factor: f32,
    pub distortion: Correction,
    pub tca: Correction,
    pub vignetting: Correction,
}
```
- **Absent op = identity** (unedited), exactly like every other op.
- `OpStack` gains a `lens_correction()` accessor, `set_op`/`reset` handle it like the others
  (`ferrolite-pipeline/src/op.rs` canonical-order machinery), and a **per-op reset** clears it.
- **Per-control reset (CLAUDE.md, load-bearing):** each of the three `amount` sliders carries its
  own `draw_reset_arrow` (reset → 1.0); each toggle defaults off; the section header has a section
  reset; the global "Reset all" already clears the whole stack.

### 5.2 Op order — **before `Geometry`**
Canonical apply order becomes:
`Exposure → WhiteBalance → Contrast → ToneCurve → HSL → Sharpen → LensCorrection → Geometry`.

Lens correction logically precedes the **user's** crop/rotate (you correct the lens, then compose).
But per **Approach A** the *geometric* part of `LensCorrection` is not a standalone resample — it is
**fused into the `Geometry` resample** (§6). The op-order slot exists so the param/reset/serde
model is uniform and so vignetting (a gain, §6.2) has a defined place; the geometric params are
consumed by the geometry stage. (Rationale recorded in §11.)

---

## 6. GPU application — Approach A (fused single resample), both tiers

### 6.1 Geometric corrections fused into the geometry resample
The existing geometry resample is the single place the image is spatially resampled — the
`GeometryHeadNode` root of the tiled full-res producer (`ferrolite-pipeline/src/nodes.rs`) and the
geometry node of the preview pipeline, both driven by `geometry.wgsl` +
`GeometryUniform`/`geometry_tile_uniform` (`ferrolite-pipeline/src/uniforms.rs`). It gains a
**warp-grid sampler**:
- For each output texel: apply the user crop/rotate transform (existing 2×2 + offset) to get the
  **undistorted** normalized coordinate, then **sample the `WarpGrid`** (bilinear) to get the
  **source** coordinate — **per channel** (R/G/B), which is exactly transverse-CA correction — and
  do **one bilinear source fetch per channel**. Distortion off ⇒ all channels share the grid's
  geometric warp; TCA off ⇒ the three channels collapse to one coordinate.
- **Amount** is applied in-shader as `mix(dest_coord, grid_coord, distortion.amount)` (and the TCA
  channel split scaled by `tca.amount`) — a uniform, so Amount drags never re-bake or rebuild.
- **Single resample:** distortion + TCA + user rotate/crop compose into one sampling transform. No
  double interpolation; the tiled producer stays a **single resampling head** (the key Approach-A
  win for the load-bearing tiled path).

### 6.2 Vignetting — a per-pixel gain pass
Vignetting correction is a **radial gain**, not a warp, and belongs in **scene-linear** light. It is
a small dedicated compute pass placed **near the head, before exposure** (so the gain acts on linear
scene values): `out = in * mix(1.0, vignette_gain(radius), vignetting.amount)`, sampling the baked
`VignetteMap` radial LUT. Cheap, point-wise, **no halo**.

**Logical vs physical placement (intentional, not a contradiction):** `LensCorrection` sits *before
`Geometry`* in the OpStack's canonical order (§5.2 — the unit of params/reset/serde), but its two
physical passes sit elsewhere in the DAG: the **vignetting gain runs early** (scene-linear, before
exposure) and the **geometric warp is fused into the geometry resample** (the tail/head resample).
This logical/physical split mirrors Spec 2, where `Geometry` is logically last yet is physically the
*resampling head* of the tiled producer. The canonical-order slot governs serialization and reset
grouping; it does not dictate the physical pass location.

### 6.3 Halo & rebuild discipline
- The tile halo for the geometric stage = `lens_halo(&WarpGrid)` (max source displacement) composed
  with the rotate footprint, extending the existing `sharpen_halo`-style function
  (`ferrolite-pipeline/src/uniforms.rs`) and capped like `MAX_SHARPEN_RADIUS`. This makes the
  full-res tiled corrected view seamless (the tile-seam golden, §8).
- **Rebuild only when geometry/halo changes** (lens_id, enable flags, focal/aperture/crop, or the
  grid) — reuses/extends `needs_full_rebuild` (`ferrolite-app/src/develop/ops_edit.rs`) which
  already triggers on geometry + halo change. **Amount-only** changes are uniform updates: no
  rebuild, no re-bake.
- **Pipelines built once + pre-warmed** (CLAUDE.md GPU rule): the vignetting pipeline joins the
  startup pre-warm (`ferrolite-pipeline/src/lib.rs`); the geometry pipeline is unchanged in count
  (it gains bindings, not a new pipeline). The warp-grid + vignette textures are cached,
  image/edit-independent GPU resources, re-created only when a new bake arrives — never per frame.

### 6.4 Two tiers (Spec 2 §6, unchanged shape)
- **Preview tier (interactive):** the warp + gain run on the single fit/preview texture; slider/
  toggle response stays sub-frame (Amount is uniform-only; enable/lens change re-bakes off-thread
  then swaps in).
- **Full-res tier (1:1):** the VT streams **corrected** tiles via the GPU tile producer with the
  lens halo (contract §5), coarse-LOD fallback inherited. Identical warp math at both tiers.

### 6.5 Recovery
On GPU device-loss, the warp-grid + vignette textures and the vignetting pipeline are rebuilt with
the rest (reuses Spec 1/2 recovery); the last bake is re-uploaded **once**, not per edit.

---

## 7. App — matching, off-thread bake, UI, persistence (`ferrolite-app`)

### 7.1 On open / match
`Metadata` → `LensQuery` → `ferrolite-lens::match_lens`. A match sets `resolved_name` and a
candidate `lens_id`/`crop_factor` **without enabling any correction** (opt-in). No match → the
section shows "No lens matched" and offers the picker only. Identity either way until the user acts.

### 7.2 Off-thread bake (`ferrolite-jobs`, contract §1)
When `lens_id`/enable/focal/aperture/crop change, a **cancellable job** runs
`bake_geometry` + `bake_vignetting` + `lens_halo` and delivers `{ WarpGrid, VignetteMap, halo,
resolved_name }` (or `None`, logged) over the **app event channel**. On receipt: upload the two
textures, set the halo, rebuild the tile producer, `request_repaint()`. Superseded bakes (rapid
lens/context changes) are **cancelled**; the latest wins. **No DB load, matching, or bake ever runs
on the UI thread** (CLAUDE.md §1). Amount changes bypass the job entirely (uniform update).

### 7.3 Develop "Lens Corrections" section (design-system §6, 296px panel)
A restyled `CollapsingHeader` (`ferrolite-app/src/develop/adjustment_panel.rs`), placed with the
other sections:
- **Matched-lens label** + a **searchable camera+lens picker** (live `find_lenses`) to override /
  manually select; a "clear match" affordance returns to identity.
- **Three rows** — Distortion / Transverse CA / Vignetting — each a **toggle** + an Amount
  `EguiSlider` (0–100%+, default 100%) with its **own reset arrow** (`draw_reset_arrow`, the shared
  `EguiSlider` reset column, `ferrolite-app/src/widgets`).
- **Advanced:** editable focal length / aperture when the auto values are wrong (collapsed by
  default).
- A **section reset** clears the whole `LensCorrection` op; the global "Reset all" already does too.

### 7.4 Persistence
The op serializes in the existing `frl:ops` JSON (`ferrolite-pipeline/src/serialize.rs`), written
off-thread and merge-preserving via the existing path (`ferrolite-catalog/src/xmp.rs`,
`ferrolite-app/src/develop/ops_persist.rs`) — **no catalog change**. Reopen re-hydrates the op and
**re-bakes** from the persisted `lens_id` + context (the grid/map are derived cache products, not
persisted). Catalog stays a cache (contract §2): losing the DB never loses the persisted selection.

---

## 8. Error handling (never panics; always a defined image)

- **No EXIF / no auto-match** → section shows "No lens matched"; picker available; **identity**.
- **Unsupported / missing correction model** for the matched lens (e.g. lens has distortion but no
  vignetting data) → that correction's toggle is disabled with a note; the others still work.
- **Bake failure** (`bake_*` returns `None`) → the correction is treated as identity, logged; the
  UI reflects it. Never a panic, never a partial/garbage warp.
- **DB load failure** → the whole section is unavailable (label explains), corrections identity,
  logged. The rest of Develop is unaffected.
- **Fallback (rung-1 / non-RGGB) images** → corrections apply at **preview-res only** (the full-res
  tiled path needs the pyramid source), as Spec 2; logged, never panics.
- **GPU pass / device-loss** → the existing wgpu error-scope recovery (§6.5).
- **Sidecar write failure** → status-bar warning; the in-memory op is kept (as Spec 2).
- **Job panics** are caught at the existing worker boundary; one bad bake/tile never downs the pool.

---

## 9. Testing (TDD; CLAUDE.md gate, then hold for the author's visual test)

**Pure CPU (every OS in CI — the 80%+ target):**
- `ferrolite-lens`: matching truth table; `bake_geometry` corners vs a `lensfun` CPU reference;
  `bake_vignetting` monotone falloff; identity when a model is absent; `lens_halo` cap (§4.6).
- `Op::LensCorrection` model: `set_op` immutability, canonical order (before `Geometry`), per-op
  reset, per-control (Amount → 1.0) reset defaults.
- Serialization: `frl:ops` round-trip incl. the new variant; version tolerance (older/absent →
  default, unedited); merge preserves foreign nodes + `xmp:Rating` (extends existing `xmp.rs` tests).
- Metadata: `crop_factor` surfaced/derived; `LensQuery` built from `Metadata`.
- Amount-lerp + halo-from-grid math.

**Golden-image GPU diffs (auto-skip when `GpuContext::headless()` is `None`, per Spec 1):**
- A **fixture-lens corrected render** (distortion + TCA + vignetting at fixed params) vs a committed
  reference authored on the dev GPU.
- A **tile-seam golden**: the corrected image via the per-tile **haloed** producer matches the
  whole-image result within tolerance at tile borders — the halo-correctness proof (mirrors Spec 2's
  sharpen tile-seam golden).
- Amount = 0 (or all-disabled) ≡ the unedited geometry result (regression: corrections off changes
  nothing).

**egui UI** (the Lens Corrections section, picker, toggles, Amount sliders + resets): `cargo build`
+ clippy + the author's hands-on visual test. No golden tests for egui rendering.

**Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace` green → **then STOP and hold for the author's (Jann's) hands-on visual
test of the running app** (real RAW files with known lenses) before finishing the branch (CLAUDE.md
"Finishing a branch" rule).

---

## 10. §5 contracts honored

1. **Job submission is universal** — DB load (if non-trivial) and every bake is a `ferrolite-jobs`
   task with cancellation + progress; superseded bakes are cancelled, navigation cancels tiles.
2. **Catalog is a cache** — only the `lens_id` + toggles + context persist (in the sidecar
   `frl:ops`); the warp grid / vignette map are re-derivable bake products, never stored. Losing
   the DB or catalog never loses the selection.
3. **Decode products additive** — lens/camera metadata (+ `crop_factor`) is an **additive**
   `Metadata` extension; no existing decode product changes.
4. **GPU executor is photo-agnostic** — the correction is `ferrolite-pipeline` **nodes** (the fused
   geometry warp + the vignetting gain pass) on the **unchanged** `Graph<PipelineImage>`; the
   executor is not touched.
5. **VT is source-agnostic** — the full-res path reuses the existing **source-agnostic** tile halo +
   GPU tile producer; any engine-crate touch is a **generic** warp-grid sample (no photo concepts,
   no copyleft) (§3.1).

**No C toolchain is introduced → no build-gating decision is required** (contrast 4.2's libjxl;
matches 4.3).

---

## 11. Decisions recorded (resolved during brainstorming, 2026-07-04)

| Question | Decision | Rationale |
|---|---|---|
| Lensfun integration / build story | **Pure-Rust `lensfun` crate v0.7**, pinned, wrapped behind a `ferrolite-lens` adapter | No C toolchain → **no build-gating decision** (like 4.3); keeps the whole workspace pure-Rust + WGSL; bundles the DB; LGPL is fine (binary is GPL-3.0). Pre-alpha risk isolated to one crate behind our own trait (swap to C bindings / hand-rolled later without touching pipeline/UI). |
| Adapter placement | **A new `ferrolite-lens` crate** (not folded into `ferrolite-pipeline`) | Quarantines the pre-alpha dep; pipeline/UI depend only on our `WarpGrid`/`VignetteMap`/`LensMatch`; mirrors the repo's seam discipline (`ferrolite-color` owns math, pipeline consumes products). |
| Match & enable behaviour | **Auto-match from EXIF, manual override picker, corrections opt-in (off by default)** | Non-destructive & least surprising; nothing resamples an image on open unless the user asks; the picker recovers a failed/wrong match. |
| Corrections & granularity | **All three (distortion, TCA, vignetting), each a toggle + Amount slider** | Matches the map's full scope; each Amount slider carries its own reset arrow (CLAUDE.md per-control reset, richly); Amount lets a user dial back an over-correction. |
| Pipeline architecture | **Approach A — fuse geometric corrections into the existing geometry resample; vignetting a separate early gain pass** | Single resample (best quality); reuses the halo path; keeps the load-bearing tiled producer a **single resampling head** (vs Approach B's two stacked resampling heads + double-resample blur). |
| CPU vs GPU / how | **CPU bakes a coarse warp grid + vignette map (off-thread); GPU samples them** | The `lensfun` crate exposes a per-pixel `Modifier` (coordinate remaps), **not** raw coefficients; baking a grid decouples the shader from lens math and reuses the GPU resampling infra. |
| Amount application | **Shader lerp uniform (not baked)** | Amount drags become uniform-only updates — instant, no re-bake, no rebuild (CLAUDE.md GPU/responsiveness rules). |
| Op order | **`LensCorrection` before `Geometry`** | Lens correction logically precedes the user's crop/rotate; the geometric part is consumed by (fused into) the geometry resample, the op slot keeps the model/reset/serde uniform and gives vignetting a defined place. |
| Lens DB shipping | **Bundled DB (`load_bundled()`); no updater in v1** | Simplest correct v1; a network/user-supplied updater is explicit future work. |
| New deps | `lensfun` (LGPL, pure Rust) in `ferrolite-lens` only | Permissive of the GPL binary; no C toolchain; isolated. |
| Scope | **One spec, 4 implementation plans, one branch** | Mirrors Spec 2/3 decomposition; keeps each plan reviewable. |

---

## 12. Decomposition into implementation plans

One branch `feat/lens-corrections` off `main`; each plan is its own writing-plans → TDD cycle, in
dependency order.

1. **`ferrolite-lens` foundation.** New crate wrapping the pinned `lensfun`: `load_bundled`,
   `match_lens`/`find_lenses`, `bake_geometry`/`bake_vignetting`/`lens_halo`, our `WarpGrid`/
   `VignetteMap`/`LensMatch` types. Full CPU tests + a bundled fixture lens. No GPU/app.
2. **`Op::LensCorrection` + metadata.** The op variant + `OpKind` slot (before `Geometry`) +
   accessor + per-op reset + serde round-trip tests; `ferrolite-decode` `crop_factor` surfacing +
   `LensQuery`. No GPU yet.
3. **GPU application.** Fuse the warp-grid sample (per-channel = TCA) into `geometry.wgsl` /
   `GeometryHeadNode` + `GeometryUniform`; the vignetting gain pass (pre-warmed); the `lens_halo`
   wiring + `needs_full_rebuild` extension; the Amount lerp uniforms. Fixture-lens + tile-seam +
   corrections-off goldens (auto-skip headless).
4. **Develop UI + bake job + persistence.** The "Lens Corrections" section (matched label, searchable
   picker, three toggle+Amount rows with per-control resets, advanced focal/aperture), the
   off-thread bake job + event delivery + texture upload + producer rebuild, and the `frl:ops`
   persist/re-bake-on-open wiring.

---

## 13. Reference

- **Spec 4 map:** `2026-07-03-spec4-secondary-and-polish-map.md` — §3 tiers, §4.4 entry, §5 contracts.
- **Spec 2 (Editing):** `2026-06-30-spec2-editing-design.md` — the edit DAG, `OpStack` sidecar
  persistence, the `Geometry` resample, and the **VT tile halo + GPU tile producer** this reuses.
- **v1 architecture map:** `2026-06-28-ferrolite-v1-architecture-map.md` — §3 licensing tiers,
  §5 cross-cutting contracts; the "Lensfun bindings (pragmatic, deferrable)" decision.
- **`lensfun` crate:** https://docs.rs/lensfun (v0.7, pure-Rust, LGPL-3.0-or-later, `load_bundled()`,
  `Modifier` per-pixel remap API — mirrors the C++ `ApplyGeometryDistortion`/`ApplySubpixelDistortion`/
  `ApplyColorModification`).
- **Lensfun calibration format** (models: `poly3`/`poly5`/`ptlens` distortion, `pa` vignetting):
  https://lensfun.github.io/manual/latest/elem_calibration.html
- **Design system:** `../../design/ferrolite-design-system.md` — the Develop 296px panel grammar for
  the Lens Corrections section + the shared reset affordance.
- **Code touch-points:** `ferrolite-pipeline/src/op.rs` (`Op`/`OpKind`/`OpStack`),
  `ferrolite-pipeline/src/pipeline.rs` + `nodes.rs` (`GeometryHeadNode`, node chain),
  `ferrolite-pipeline/src/uniforms.rs` (`GeometryUniform`, `sharpen_halo`),
  `ferrolite-pipeline/src/shaders/geometry.wgsl`, `ferrolite-pipeline/src/serialize.rs`,
  `ferrolite-decode/src/metadata.rs` + `lib.rs` (metadata extraction),
  `ferrolite-app/src/develop/adjustment_panel.rs` + `ops_edit.rs` + `ops_persist.rs`,
  `ferrolite-app/src/widgets` (`draw_reset_arrow` / `EguiSlider` reset column),
  `ferrolite-catalog/src/xmp.rs` (`read_ops`/`write_ops` — unchanged).
