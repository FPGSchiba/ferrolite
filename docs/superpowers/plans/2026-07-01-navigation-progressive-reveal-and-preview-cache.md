# Navigation: Progressive Reveal, Preview Cache, Parallel Demosaic

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or
> superpowers:subagent-driven-development) to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax. This spec is self-contained — you do NOT
> need the originating chat. Read the "Background" section first.

**Goal:** Make Develop navigation feel right at scale. Eliminate the
preview→full **color/tone shift** on RAW open, keep first-pixel fast as the
editing DAG deepens and megapixels grow, and remove the last synchronous cost
from the decode path.

**Three improvements, in priority order:**
1. **Coarse-real progressive reveal** (near-term, the one that matters now) —
   stop displaying the camera JPEG for RAW; show the *real* render (color-managed
   raw pipeline) progressively instead, so there is no color shift.
2. **Persistent pipeline-rendered preview cache** (the high-MP answer) — cache
   ferrolite-rendered previews on disk so browsing is O(preview) regardless of
   MP/DAG. This is why Lightroom has a preview cache.
3. **Parallelize the demosaic** (small) — keep the decode term low as MP grows.

**Tech stack:** Rust 2021, egui/eframe 0.29, wgpu 22, rayon (already a workspace dep).

---

## Background (read this first)

### The two-tier viewer today

Opening an image in Develop runs a two-tier load (`ferrolite-app/src/viewer/load.rs`):

- **Tier 1 (preview):** `spawn_preview` decodes the RAW's **embedded JPEG**
  (`decode_preview_raw`) → `AppEvent::PreviewReady`. `apply_preview_ready`
  (`app.rs`) builds a rung-1 single-texture `VirtualTexture` from it
  (`color_convert` sRGB→working), sets `loaded = true` (canvas paints instead of
  the spinner), and fits the view. This is the fast first pixel.
- **Tier 2 (full):** after a debounce (`FULL_DECODE_DEBOUNCE`), `spawn_full`
  runs `decode_full` → `apply_orientation_linear` → `QuadBin` (half-res demosaic)
  → `AppEvent::FullDecoded`. `apply_full_decoded` builds a rung-4 **sparse**
  `VirtualTexture` (feedback-driven, tiled) from the demosaiced image, attaches a
  `TileEditPipeline` producer (camera→working + op stack), and begins a crossfade
  from the preview to the full.

`drive_viewer` (`app.rs`) advances the crossfade and flips `show_full` once the
full is ready and its tiles are produced; `ViewerCallback` (`viewer/callback.rs`)
draws `full` when `show_full` else `preview`. `ViewerGpu` holds
`{ preview, full, preview_before }`.

### Why the shift happens (root cause — NOT a bug)

The embedded JPEG carries the camera's color science + picture style (Sony
Creative Style: contrast, saturation, tone). The full tier is ferrolite's
**neutral raw** render (`camera_to_working` → sRGB OETF). These are two genuinely
different renderings; the raw looks flatter, so the swap reads as a color/tone
shift. Making them match — not hiding the swap behind a spinner — is the fix.

### Scaling analysis (why "just wait for full" was rejected)

- **DAG depth axis:** DAG cost lands on the tiled producer, which is already
  bounded per frame (`MAX_PRODUCE_PER_FRAME`, CLAUDE.md rule 2). Deeper DAG →
  longer time-to-fully-sharp, streamed; never a synchronous stall. Well-handled.
- **Megapixel axis:** first-pixel of any *color-correct* render is bounded below
  by decode+unpack (I/O + decompress), which scales ~linearly with MP and cannot
  be cheaply subsampled for compressed RAW. Measured on a 24 MP Sony ARW:
  `decode ≈ 5 ms, demosaic ≈ 25 ms` (warm). Est. ~150–250 ms at 100 MP. It stays
  sub-second and runs on the job thread (never freezes UI), but it grows.
- "Wait for full" recouples first-pixel latency to full-render latency, throwing
  away the progressive architecture's main benefit. Fine at 24 MP, degrades with
  MP. Rejected.

**Conclusion:** keep the progressive architecture. Fix the *tier-1 source*
(JPEG → real render), and for high-MP browsing add a persistent cache. That is
what Improvements 1 and 2 do.

### Session context (state of the tree when this spec was written)

The following were just implemented (verify they are present; they may or may not
be committed yet). Do NOT re-do them:

- **RAW EXIF orientation:** `RawDecoded.orientation` +
  `ferrolite_decode::apply_orientation_linear`, applied in `spawn_full`; view
  refit to full dims in `apply_full_decoded`.
- **Histogram render:** `develop/histogram_widget.rs` draws per-bin bars (was a
  broken `convex_polygon`).
- **Drive-loop convergence:** `VirtualTexture::produce_pending()` +
  `needed_established()`; `drive_viewer` gates `idle` on producer convergence and
  keeps repainting until converged (tiles now stream without manual pan/zoom).
- **Double-white-balance fix (the red cast):** `ferrolite_color::normalize_neutral`
  row-normalizes the RAW `camera_to_working` (the demosaic already applied the
  as-shot WB gains). Applied ONLY in the app's RAW `camera_to_working()`, not the
  sRGB `preview_to_working()`.

---

## Global constraints

- `cargo fmt --all` clean; `cargo clippy --workspace --all-targets -- -D warnings`
  exit 0; `cargo test --workspace` green after each improvement. Conventional
  commits, no attribution footer.
- **Responsiveness rules (CLAUDE.md, load-bearing):** never block the UI/update
  thread with decode/demosaic/pyramid work — it stays on `ferrolite-jobs`; GPU
  work stays on the render thread and bounded per frame; build pipelines once and
  reuse. Do not regress these.
- **Finish rule (CLAUDE.md):** a green workspace gate is necessary but not
  sufficient — STOP and wait for the author's visual test before finishing each
  improvement.
- Standard (JPG/PNG) images are unaffected by all three: their tier-1 preview IS
  the full render (no tier-2), so they keep today's behavior (spinner → single
  render). Guard every RAW-specific change on `kind == FileKind::Raw`.

---

# Improvement 1 — Coarse-real progressive reveal

**Outcome:** On RAW open, the canvas shows a spinner until the *real* render is
ready, then reveals the color-managed raw render (complete at low resolution) and
sharpens in progressively. The embedded JPEG is never displayed in Develop, so
there is no color/tone shift. Same pipeline throughout → the only visible change
is resolution, not color.

**Design decision — one color path.** Replace the JPEG-sourced rung-1 preview
with a raw-pipeline render, so the displayed image, the histogram source, and the
before/after "before" all come from the same color-managed pipeline. Concretely:

- Do NOT display the embedded JPEG for RAW. (Keep it only as a *fallback* if the
  full decode fails, and for the Library grid thumbnails, which use a separate
  path — do not touch those.)
- The demosaic output is already half-res (`QuadBin`, e.g. 3024×2012 for a 24 MP
  sensor ≈ 6 MP). Build the rung-1 single-texture "preview" from *that* (one
  pipeline pass through `camera→working` + op stack), and keep the sparse full VT
  for zoom/pan/streaming detail. The single render is the color-correct first
  pixel; the sparse VT sharpens/enables zoom.

There are two viable implementations; **Approach A is recommended** (simpler,
reuses the rung-1 single-texture + crossfade + histogram machinery). Approach B
is noted as an alternative.

### Approach A (recommended): raw-derived rung-1 preview built at full-decode

The rung-1 `preview` VT is currently built from the JPEG in `apply_preview_ready`.
Move its construction to `apply_full_decoded` and source it from the demosaiced
RAW run through the pipeline. Keep the existing crossfade→sparse for detail; since
both tiers are now the same pipeline, the crossfade becomes a pure sharpness ramp
(no color shift).

**Files:**
- Modify `ferrolite-app/src/viewer/load.rs` — for RAW, skip `spawn_preview`'s
  *display* role (see Task 1; keep it available for the fallback).
- Modify `ferrolite-app/src/app.rs` — `apply_preview_ready`, `apply_full_decoded`,
  `drive_viewer`, `FullFailed` handler, `FULL_DECODE_DEBOUNCE`.
- Possibly add a helper in `ferrolite-pipeline` to render the half-res demosaic
  through the op stack into a single `Rgba16Float` texture (an `EditPipeline`
  evaluate already does this — reuse it; see Task 2).

- [ ] **Step 1 — Gate the RAW display off the JPEG.**
  In `apply_preview_ready`, for `kind == Raw` do NOT set `loaded = true` and do
  NOT build/insert the displayed `ViewerGpu.preview` from the JPEG. Keep the
  spinner up. (Standard images keep the current path: build preview, `loaded =
  true`, `idle = true`.) You may keep decoding the JPEG so it is available for the
  failure fallback (Step 5), but it must not be shown.
  - Decide whether to keep `spawn_preview` for RAW at all. Recommended: keep it
    (cheap, ~ms) purely as the fallback source; store its linear buffer on
    `ViewerState` (e.g. reuse `preview_source`) but do not build a displayed VT
    from it.

- [ ] **Step 2 — Build the color-correct rung-1 render at full-decode.**
  In `apply_full_decoded`, after the demosaiced+oriented `image` arrives and the
  camera→working matrix (`cam`, already `normalize_neutral`-ized via the app's
  `camera_to_working()`) is known, render `image` through the op stack into a
  single `Rgba16Float` texture and wrap it as the rung-1 `ViewerGpu.preview`
  (`VirtualTexture::single_from_texture`). Reuse `EditPipeline` (the same type the
  preview edit tier uses) or a one-shot pipeline pass — build the pipeline once
  and reuse it (do not compile per open). This is the first-reveal image.
  - Also build the sparse full VT as today (unchanged) for zoom/streaming.
  - Set `image_dims` + refit the view to the full dims (already done today).

- [ ] **Step 3 — Reveal + progressive sharpen, no color shift.**
  Set `loaded = true` here (reveal). Two sub-options for how the sparse full takes
  over:
  - **3a (simplest):** keep the existing crossfade (`begin_crossfade`) from the
    rung-1 render to the sparse full. Because both are the same pipeline, this is
    now a sharpness-only ramp. `drive_viewer` already gates `show_full` on
    `full_converged` (session work) so it swaps when the full is actually
    produced.
  - **3b:** skip `begin_crossfade`; show the rung-1 render until the sparse full
    has produced the visible tiles (reuse `produce_pending()/needed_established()`),
    then swap. Avoids a redundant ramp. Either is acceptable; prefer 3a for
    minimal change.

- [ ] **Step 4 — Point the histogram at the real render.**
  `maybe_update_histogram` reads `g.preview.single_texture_arc()`. With Step 2 the
  rung-1 preview IS the raw render, so the histogram is now color-consistent with
  what is displayed — no code change beyond confirming it dispatches after the
  rung-1 render exists (mark dirty in `apply_full_decoded`, which already happens).
  Add a test asserting the histogram source is the raw-render texture, not a JPEG
  buffer, if practical.

- [ ] **Step 5 — Failure fallback.**
  In the `FullFailed` handler, if `kind == Raw` and we never revealed
  (`!loaded`), build and show the embedded-JPEG rung-1 preview as a fallback so an
  undecodable-full still shows *something*, then set `loaded = true` and `idle`.
  This is the one place the JPEG may still reach the screen; log it.

- [ ] **Step 6 — Snappier settle.**
  Lower `FULL_DECODE_DEBOUNCE` (0.15 → ~0.05 s). Decode is ~30 ms, so the debounce
  is now the dominant settle delay; trimming it makes reveal feel near-instant
  while still coalescing very fast scrubbing. Keep the job cancellation on
  navigation (`cancel_loads` / `cancel_sparse`) so scrubbed-past decodes stop.

- [ ] **Step 7 — before/after "before".**
  `ensure_before_view` builds `preview_before` from `preview_source` via
  `color_convert` (sRGB→working). With the unified path, build the "before" from
  the raw demosaic + identity op stack + `camera_to_working` instead, so the
  before/after split compares like-with-like. Guard on RAW.

**Approach B (alternative): warm the sparse coarse LODs, no rung-1 render.**
Add `VirtualTexture::warm_coarse(&mut self, ctx, producer, max_levels) -> usize`
to the sparse path: enumerate every tile of the coarsest `max_levels` LOD levels
(from the internal `LevelLayout` — `level_count()`, `dims(lod)`) and produce them
through the existing `produce_view` path so the shader's coarse-LOD fallback has a
resident tile for the whole image on the first paint (no gray flash, no feedback
bootstrap). Then reveal (`loaded = true`) showing the sparse full directly (skip
`begin_crossfade`). Downside vs A: the histogram + before/after still need a
single-texture source, so you keep a second (JPEG or separately-rendered) path —
i.e. it does NOT unify the color path. Prefer A unless the rung-1 render pass is
undesirable.

**Improvement 1 acceptance criteria:**
- Opening a RAW shows spinner → color-correct raw render (no JPEG look, no
  color/tone shift), sharpening in progressively.
- Histogram matches the displayed image.
- Fast filmstrip scrubbing coalesces (spinners during scrub, image on settle
  within ~debounce + decode); no piled-up decodes.
- Standard JPG/PNG unchanged.
- Full-decode failure still shows the embedded JPEG as a fallback.
- Workspace gate green; author visual test passes.

---

# Improvement 2 — Persistent pipeline-rendered preview cache

**Outcome:** Browsing is O(preview) regardless of MP or DAG depth. First visit to
an image renders + caches a ferrolite preview; every subsequent visit (and fast
scrubbing) loads the cached preview instantly and color-correct. This is the
decoupling that makes 100 MP libraries browse smoothly, and the reason Lightroom
maintains a preview cache.

**Design:**
- **What is cached:** a downscaled, color-managed render of the image at the
  *current* op stack + working space + camera profile — i.e. the output of the
  same pipeline as tier-1 in Improvement 1, at a fixed "standard preview" long
  edge (e.g. 2048 px). Store as a compact GPU-uploadable format (e.g.
  `Rgba16Float` raw, or a lossy-but-wide encoding; measure size vs. quality).
- **Where:** a cache directory alongside the catalog DB (reuse
  `ferrolite-catalog` conventions). Key by `image_id` +
  a **content/render hash**: `(file mtime/size, op_stack_version, working_space,
  color_profile, preview_long_edge, pipeline_schema_version)`. Any change to those
  invalidates the entry.
- **Read path:** on Develop open (and on filmstrip prefetch), look up the cache
  key. Hit → upload the cached preview as the rung-1 render and reveal
  immediately (no decode needed); still kick the sparse full for zoom detail.
  Miss → fall back to Improvement 1's decode+render path, and write the result to
  the cache off-thread.
- **Invalidation:** on edit commit, working-space change, or profile change, the
  `op_stack_version`/inputs change → key changes → old entry is stale (lazily
  overwritten). Add a bounded LRU eviction (size cap) and a "rebuild previews"
  maintenance action.
- **Threading:** all cache I/O and rendering on `ferrolite-jobs`; never on the UI
  thread. Prefetch neighbors of the current filmstrip selection at low priority.

**Files (new work, sketch):**
- New module, likely `ferrolite-catalog` or a new `ferrolite-previews` crate:
  cache store (path layout, key hashing, read/write, LRU eviction).
- `ferrolite-app`: cache lookup on open + filmstrip prefetch; write-back after a
  miss render; wire invalidation to edit/WS/profile changes
  (`opstack_version`, `apply_working_space`).
- Schema/version constant so a pipeline change bumps `pipeline_schema_version` and
  invalidates all previews.

**Tasks:**
- [ ] Define the cache key + on-disk layout + a `PreviewCache` API
  (`get(key) -> Option<Preview>`, `put(key, preview)`, `evict_to(size)`).
- [ ] Render-to-cache path (reuse Improvement 1's pipeline render at the standard
  long edge), off-thread.
- [ ] Cache read on open + neighbor prefetch (low priority jobs).
- [ ] Invalidation wiring (edit commit, working space, profile,
  `pipeline_schema_version`).
- [ ] LRU eviction + size cap + a "purge/rebuild previews" action.
- [ ] Tests: key stability, invalidation on each input, LRU eviction, round-trip.

**Acceptance:** second and later visits to an image (and fast scrubbing across a
folder of large RAWs) show the color-correct preview with no visible decode wait;
edits/WS/profile changes correctly invalidate; cache respects its size cap.

---

# Improvement 3 — Parallelize the demosaic

**Outcome:** The decode term (the MP-bound part of first-pixel) stays small as
sensors grow. `QuadBin::to_linear_rgba_f32` is currently a single-threaded
per-output-pixel loop; parallelize it across rows with rayon.

**Files:**
- `ferrolite-decode/src/demosaic.rs` — `QuadBin::to_linear_rgba_f32`.
- `ferrolite-decode/Cargo.toml` — add `rayon` (workspace dep) if not present.

**Tasks:**
- [ ] Rewrite the output loop as a parallel row iterator
  (`par_chunks_mut(out_w*4)` over the output buffer, or
  `into_par_iter()` over output rows), keeping the exact same per-pixel math
  (black level, WB gains, normalize, CFA sampling) so results are bit-identical to
  the serial version.
- [ ] Keep the existing unit tests green (they assert specific binned values);
  add one asserting the parallel output equals a serial reference on a small
  fixture (guard against ordering bugs).
- [ ] Confirm the work still runs on a `ferrolite-jobs` worker (it does — called
  from `spawn_full`); rayon parallelism nests fine but verify it does not starve
  the job pool for small images (consider a min-size threshold before going
  parallel).

**Acceptance:** demosaic wall time drops ~linearly with cores on large frames;
output identical to serial; workspace gate green.

---

## Suggested order & stopping point

Implement **Improvement 1 first** — it removes the color shift and, with the
session's convergence + orientation + WB fixes, puts navigation "in a good spot"
(the stated near-term goal). Ship it, get the author's visual test, commit.

**Improvements 2 and 3 are the scale insurance** — pull them in when higher-MP
libraries or heavier DAGs make first-pixel latency or browse thrash noticeable.
They are independent of each other and of Improvement 1.
