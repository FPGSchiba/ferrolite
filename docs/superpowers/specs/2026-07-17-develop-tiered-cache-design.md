# Develop tiered image cache + memory-budget design

**Date:** 2026-07-17
**Status:** Approved (brainstorm) — pending implementation plan
**Scope:** `ferrolite-app` (`develop/`, `viewer/`, `diag.rs`, `app.rs`, `state.rs`), `ferrolite-previews`

## Problem

Two related pain points in the Develop view:

1. **Perceived speed.** Opening an image (RAW *or* JPG) shows no instant
   feedback and re-decodes from scratch. JPGs are worst off: the on-disk
   preview cache write-back is RAW-only (`should_write_back` requires
   `is_raw`), so a JPG gets **no cached reveal** — every open pays a full
   decode + pyramid build. JPG-straight-from-camera shooters edit these as
   first-class originals, not quick looks, so JPG and RAW must be treated
   equally.

2. **Unbounded memory growth while scrolling RAWs.** Memory climbs the longer
   the user scrolls through images in Develop. The cause is not yet confirmed
   by measurement (see Phase 0).

The two are deeply linked: any new caching adds memory pressure, so a memory
budget is the precondition that makes aggressive caching safe. This design
treats them as **one system**.

## Findings from code investigation

- **Structurally single-image.** `open_record` (`app.rs:2602`) cancels the old
  image's loads + VT tiles, then `open_image_in_viewer` (`state.rs:578`)
  replaces the single `Option<ViewerState>`, dropping the old pyramid/source
  `Arc`s. `ViewerGpu` is a single reused holder in `callback_resources`. There
  is **no accumulating per-image map** of live state.
- **wgpu is polled** every frame (`Maintain::Poll` at `app.rs:381,422`), so
  dropped GPU textures/buffers *are* reclaimed — argues against a classic GPU
  leak.
- **JPGs pay almost the full RAW path.** The pyramid job (`app.rs:1046–1086`)
  is **not** gated on RAW — the comment at `app.rs:1000` confirms
  Standard images still build both the CPU `PyramidTileSource` and the GPU
  `GpuPyramidSource` and install a sparse VT. A JPG only skips demosaic and the
  camera→working color step.
- **Large f32 buffers × in-flight multiplicity is the leading memory
  hypothesis.** Each open converts to a full-res **linear f32 RGBA** buffer
  (16 B/px — 24 MP ≈ 384 MB), then `Arc::new(image.clone())` at `app.rs:1047`
  **duplicates it** to hand to the pyramid job, plus a GPU pyramid (~1.33×
  full-res f32 in VRAM). Scrolling faster than jobs drain keeps several in
  flight → memory climbs, and should recede when idle. **To be confirmed by
  Phase 0**, not assumed.
- The only per-image-growing containers are `thumb_missing` / `thumb_pending`
  (sets of `i64` — negligible bytes), not the hundreds of MB observed.

## Decisions (locked during brainstorm)

- **One unified design**, not two separate efforts.
- **Adaptive memory budget** = `clamp(fraction × total_ram, floor, ceiling)`.
- **JPG stays on the unified pipeline** (Option B): equal first-class treatment;
  add caching + placeholder around it, no second pipeline.
- **Cache structured as a dedicated `develop::cache` module** (not a new crate
  yet): pure, headless-testable logic behind one API.
- **Measure before building**: confirm the memory-growth cause with
  instrumentation first.
- **Instrumentation lives in the existing `diag.rs` stack** as a permanent,
  reusable memory-profiling tool — not throwaway counters.
- **Drop the redundant `image.clone()`** at `app.rs:1047` (pass the shared
  `Arc`).

## Reveal model (what the user sees)

Uniform for RAW and JPG:

```
open → Tier 0 (upscaled thumbnail)   ~instant, already in RAM
     → Tier 1 (2048px disk preview)  fast decode, color-correct
     → Tier 2 (full pipeline reveal) current behavior
     → full VT streams in on zoom
```

- **Tier 0** reuses the exact thumbnail already decoded for the grid/filmstrip
  (`thumb_pixels` / texture cache), upscaled, shown immediately; crossfades out
  when the real reveal lands (reuses existing `crossfading` /
  `crossfade_elapsed`). Zero decode.
- **Tier 1** is the existing `ferrolite-previews` 2048px color-managed JPEG,
  with **write-back extended to JPG**.
- **Tier 2** is the unchanged unified full pipeline, minus the redundant clone.
- On a warm re-open (recent neighbor still resident in the RAM cache), Tier 2
  is served immediately at full quality, skipping Tiers 0/1.

## Architecture

### The `develop::cache` module

Owns four things behind one API; all *logic* is pure and headless-testable
(mirrors how `ferrolite_vt::ResidencySet` and `preview_cache::prefetch_targets`
are tested):

- In-RAM **byte-accounted LRU** keyed by `(image_id, Tier)` → `Arc<buffer>`.
- The **adaptive budget**.
- An **in-flight registry** capping concurrent heavy decodes so transient
  memory can't overshoot budget.
- Coordination with the on-disk `ferrolite-previews` Tier-1 store (which keeps
  its own disk LRU via `evict_to`).

API sketch:

```rust
enum Tier { Preview /* 2048px */, Full }

enum CacheState { Resident(Arc<LinearRgbaF32>), Loading, Absent }

fn get(&self, id: i64, tier: Tier) -> CacheState;
fn insert(&mut self, id: i64, tier: Tier, buf: Arc<LinearRgbaF32>); // evicts to budget
fn inflight_begin(&self, bytes: u64) -> InflightGuard;             // drop = end (saturating)
fn set_budget(&mut self, bytes: u64);
fn resident_bytes(&self) -> u64;
```

Eviction: LRU by last-touch; **never** evict the currently-open image's live
tiers.

### Adaptive budget

`budget = clamp(fraction × total_ram, floor, ceiling)`. Starting constants
(tunable): `fraction ≈ 0.15`, `floor ≈ 512 MB`, `ceiling ≈ 4 GB`. Total RAM
detected once at startup via the same platform layer as RSS (Phase 0).

**Scope:** governs the new develop RAM cache only. The existing thumb caches
(texture cache 512-cap, pixel cache ~256 MB) stay separate and unchanged — they
are bounded and not implicated — but the memory overlay shows them so we can
revisit. **GPU side:** VT tile pools are already fixed-budget; we bound
*pyramid count* (≈ 1 live + the in-flight cap) rather than byte-budgeting VRAM
portably.

### The clone removal

Pass the `Arc` the reveal already holds into the pyramid job instead of
`Arc::new(image.clone())` (`app.rs:1047`). Implementation must verify the
reveal retains the *full-res* buffer as an `Arc` (today `preview_source` /
`raw_preview_source` are `Arc`s; the full `image` may need its `Arc` hoisted
earlier so reveal and job share one allocation).

### JPG Tier-1 write-back

Relax the `is_raw` gate in `preview_cache::should_write_back` (and the read /
prefetch key paths) to include `FileKind::Standard`. The `PreviewKey` already
hashes file identity + op stack + color profile; the encoded payload for a
Standard image is its color-managed identity render, same shape as RAW.
Implementation must confirm the Standard color path produces an equivalent
`display_matrix` input to `encode_srgb_jpeg`.

## Phase 0 — Memory-profiling overlay (the debug half, built first)

A **dedicated second overlay** (own toggle, e.g. F10), separate from the
existing text panel, purpose-built to show *what* grows and *when*. Lives
entirely in `diag.rs`, behind `FERROLITE_DIAG` (zero cost when off), reusing the
module's gate / tick / log-sink plumbing and the
`record_* → Gauges → Snapshot → format_*_line` pattern.

### 1. Precise per-category attribution

A `MemCategory` breakdown where each owner self-reports its bytes (buffers know
their size: `LinearRgbaF32` = `w*h*16`):

| Category | Source |
|---|---|
| `viewer_full_linear` | open viewer's full-res f32 image |
| `viewer_preview_src` | `preview_source` / `raw_preview_source` `Arc`s |
| `cpu_pyramid` | `PyramidTileSource` buffers |
| `gpu_pyramid` | `GpuPyramidSource` (VRAM) |
| `vt_pools` | preview + full VT tile pools (capacity × tile bytes) |
| `present_buffers` | front / back present textures |
| `ram_cache` | new develop cache, subtotaled per tier |
| `disk_preview` | on-disk preview store size |
| `thumb_tex` / `thumb_pix` | existing thumb caches |
| `inflight_decode` / `inflight_pyramid` | bytes held by active jobs (push atomics) |
| `rss` | process resident set — ground truth |
| **`unattributed`** | **`rss − Σ(known CPU categories)`** |

`unattributed` is the debugging linchpin: if it climbs while modeled categories
stay flat, an *unmodeled* allocation is leaking and we know to hunt there. The
overlay renders a **category / current / peak** table.

Gathering: mostly **pull** at tick time (live `ViewerState` buffers, cache
resident bytes, thumb-cache sizes, VT pool capacities) plus **push** atomics for
in-flight job bytes (bumped on capture, dropped saturating on completion via an
`InflightGuard`).

### 2. Growth over time

A ring buffer (~300 samples) of RSS + key categories, drawn as a
sparkline / line graph in the overlay so the user watches the curve live and
sees plateau (policy) vs. forever-ramp (leak). **Painter polyline by default**
(a graph is data-viz, not an icon — outside the no-hand-drawn-icons rule; no new
dependency); `egui_plot` an optional upgrade.

### 3. Logging rich enough to diagnose from the file alone

- ~1/sec structured line:
  `[mem] t+12s rss=2.1G live=380M inflight=760M gpu=512M cache=0 unattrib=90M`.
- **Event-anchored** deltas on open / close / nav:
  `[mem] open #123 RAW: live +384M inflight +760M rss=2.1G` — the log alone
  reconstructs what each navigation added.
- A **dump hotkey** (e.g. Shift+F10) writing a full categorized snapshot on
  demand.

### Classify the growth

- Recedes when idle → **policy/timing** (in-flight f32 buffers piling up) → the
  budget + in-flight cap (Phase 1) is the fix.
- Never recedes → **real leak** → stop and hunt it with `systematic-debugging`
  before layering the cache on top; `unattributed` points at where.

### Platform layer

RSS + total RAM need a small layer not currently in the tree. Default:
`memory-stats` (tiny, cross-platform) for RSS; total RAM via a minimal
`cfg`-gated read (Linux `/proc/meminfo`, macOS `sysctl hw.memsize`, Windows
`GlobalMemoryStatusEx`) or `sysinfo` if a heavier dep is acceptable. Decided at
implementation; dev-only so a small crate is fine.

## Rollout

Each phase is independently shippable and testable; **Phase 0 gates the rest.**

1. **Phase 0** — memory-profiling overlay + RSS/total-RAM layer + per-category
   byte accounting → *measure, confirm cause*.
2. **Phase 1** — `develop::cache` module (pure logic + tests), adaptive-budget
   wiring, in-flight cap, clone removal → *bounds memory*.
3. **Phase 2** — Tier-0 thumbnail placeholder reveal (reuses existing crossfade)
   → *instant open feel*.
4. **Phase 3** — JPG Tier-1 write-back + warm-RAM neighbor reuse under budget
   → *fast re-opens, both formats*.

## Testing

Headless unit tests (matching existing repo patterns): budget clamp, LRU
eviction order, byte accounting, in-flight cap, `should_write_back` for
`Standard`, and every new pure diag formatter (`format_mem_line`, the breakdown
formatter, event-anchored delta formatter). Scoped gate per crate during work;
repo gate once at the end.

## Visual test plan (per CLAUDE.md, for the author)

Automated green is necessary but not sufficient; hand-test after the repo gate:

1. **Memory overlay (Phase 0).** Run with `FERROLITE_DIAG=overlay`, toggle the
   memory overlay (F10). Open a RAW folder, scroll fast: watch the growth graph
   and category table. Expected: modeled categories dominate; `unattributed`
   stays small. Failure signature: `unattributed` climbs steadily (unmodeled
   leak) or total never recedes when you stop scrolling.
2. **Memory bound (Phase 1).** Scroll fast through many RAWs; confirm RSS
   plateaus near the budget rather than climbing without limit. Confirm the
   `cache` category respects the budget (evictions logged).
3. **Instant reveal (Phase 2).** Open a RAW and a JPG cold: a placeholder
   (upscaled thumbnail) appears immediately, then crossfades to sharp with no
   jarring pop or obvious color jump.
4. **Warm re-open (Phase 3).** Open image A, move to B, back to A: A reveals
   immediately at full quality (served from RAM cache). Open the *same* JPG
   twice: second open is fast (Tier-1 disk hit), not a full re-decode.
5. **No regressions.** Per-control reset, edits, crop/mask overlays, and split
   compare still behave; no freeze on open or navigation (frame-time line in the
   text overlay stays within budget).

## Non-goals / YAGNI

- No JPG fast-path / second pipeline (explicitly rejected — JPGs are
  first-class originals on the unified pipeline).
- No byte-level VRAM budgeting (bound pyramid *count* instead).
- No changes to the existing thumb caches (bounded; not implicated).
- No new crate — a module suffices until a second consumer exists.
