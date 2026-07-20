# Develop warm-navigation cache design

**Date:** 2026-07-20
**Status:** Approved (brainstorm) — pending implementation plan
**Scope:** `ferrolite-app` (`develop/`, `viewer/`, `app.rs` reveal flow, `diag.rs`/`diag_mem.rs` budget), building on the shipped tiered-cache work (Phases 0/2/3, branch `feat/develop-fast-jpg-phases-2-3`).

## Problem / premise

The v2 UI (`docs/design/V2/`) makes Develop **filmstrip-driven**: a 96px strip of shoot thumbnails you click to load the big canvas. The quality-of-life expectation is **instant whole-shoot navigation** — clicking through a shoot reveals each image sharp, immediately, *with its edits* — the Lightroom "flip between develops" feel.

What already ships on this branch gets us part-way:

- **Tier-0** (thumbnail placeholder) makes *any* open show the picture instantly, but soft.
- **Tier-1** (2048px disk cache, both formats) makes the *sharp* re-open fast — but still a decode + reveal, not instant, and it is identity-only.
- Scroll memory is bounded (serialized prefetch + shared full-res `Arc`).

What is missing is a **warm in-RAM tier** that holds recent/neighbor images' *rendered* state resident so navigation is genuinely instant. This was the deferred Phase-1 `develop::cache` from the earlier tiered-cache spec, re-scoped here around the navigation goal rather than pure memory-bounding.

**This is a deliberate memory-for-speed trade.** Develop is structurally single-image today (`open_image_in_viewer` replaces one `Option<ViewerState>` and drops the prior pyramids; `ViewerGpu` is a single reused holder in `callback_resources`). This design intentionally retains N images' rendered state, governed by a budget, to buy instant navigation.

## Decisions (locked during brainstorm, 2026-07-20)

- **Hybrid two-level warm cache:** a light **display-tier** for a window of neighbors + the heavy **full-pipeline** for the 1–2 most-recent images.
- **Proactive, forward-biased prefetch** of filmstrip neighbors (the culling direction), not reactive-only.
- **Edited-aware warm previews:** a warmed neighbor honors its saved edits (keyed by op stack), not just as-shot.
- **Cache holds `Arc` handles, not the live `ViewerGpu` holder** — the single-holder architecture is untouched; swaps rebuild cheap wrappers from cached `Arc`s.
- **Adaptive budget becomes a real byte-accounted cap** (today it is only a diag figure), with its fraction and clamp bounds as **named, tunable constants**.
- **Build on current Develop** (this branch); the v2 UI filmstrip consumes it later — no v2-UI dependency.

## Architecture

### The two-level warm cache

A new `develop::cache` module. Both levels keyed by **`(image_id, op_stack_hash)`** so an edit naturally misses the stale entry and re-warms — no manual invalidation path.

**Display tier (the window — many).** Per resident image, an `Arc<wgpu::Texture>` of the **edited rung-1 display render** — the exact fit-view texture `apply_full_decoded` (RAW) / `reveal_srgb_preview` (Standard) already produce. Small VRAM each (a rung-1 texture, not a pyramid). On activation the module hands this `Arc<wgpu::Texture>` back; the reveal wraps it into the single `ViewerGpu` preview VT (`VirtualTexture::single_from_texture`, already used everywhere) = **instant fit-view sharp, with edits**. A 1:1 zoom re-streams the sparse full exactly as today.

**Full-pipeline tier (the recent — M = 1–2).** For the last 1–2 *actually-opened* images, additionally retain the `Arc<GpuPyramidSource>` + the tile-source `Arc<dyn TileSource>` + the op-stack/cam snapshot. On activation the reveal rebuilds the (`!Send`) `EditTileProducer` + sparse `VirtualTexture` from those `Arc`s — the identical work `apply_pyramid_ready` does today, minus the ~1.2 s pyramid build = **instant 1:1** on immediate back-and-forth.

Retaining `Arc`s (not `ViewerGpu`) is the key simplification: `ViewerGpu` stays a single holder; a swap rebuilds the cheap VT/producer wrappers from the cached `Arc`s, so no multi-image GPU-holder machinery is introduced.

### `develop::cache` module — API sketch

Pure LRU logic (headless-testable, mirroring `ferrolite_vt::ResidencySet` / `preview_cache::prefetch_targets`). GPU `Arc`s are opaque handles it stores, byte-accounts, and evicts — no GPU/egui in the pure core.

```rust
struct CacheKey { image_id: i64, op_stack_hash: u64 }

struct DisplayEntry { tex: Arc<wgpu::Texture>, dims: (u32, u32), bytes: u64 }
struct FullEntry {
    pyramid: Arc<GpuPyramidSource>,
    tile_source: Arc<dyn TileSource + Send + Sync>,
    op_stack: OpStack, cam: [[f32; 3]; 3], bytes: u64,
}

enum WarmHit { Full(FullEntry, DisplayEntry), Display(DisplayEntry), Miss }

fn get(&mut self, key: CacheKey) -> WarmHit;          // marks last-touch
fn insert_display(&mut self, key: CacheKey, e: DisplayEntry);   // evicts to budget
fn insert_full(&mut self, key: CacheKey, e: FullEntry);         // caps to M, evicts to budget
fn set_open(&mut self, key: Option<CacheKey>);        // never-evict guard
fn set_budget(&mut self, bytes: u64);
fn resident_bytes(&self) -> u64;                      // feeds the F10 ram_cache category
```

Eviction: LRU by last-touch; **never** evict the open image or the current forward-prefetch core; the full tier is additionally count-capped at M.

### Prefetch & threading (edited-aware, forward-biased)

On settle, warm **±N filmstrip neighbors**, biased forward. Per neighbor:

1. **Decode the source off-thread** — the heavy part (RAW demosaic / JPEG decode) on a `ferrolite-jobs` `Background` job, reusing the existing serialized single-job discipline so the transient peak is one source buffer at a time (the bounded-concurrency contract from `spawn_prefetch`).
2. **Load its edit sidecar off-thread** (op stack) in the same walk.
3. **Render the rung-1 edited display texture on the render thread, bounded to one neighbor per frame**, and `insert_display` it.

Rationale for the split: the multi-ms work (decode) stays off the UI thread (CLAUDE.md rule 1); the edit eval is a *fast GPU pass* — the same rung-1 evaluation every open already runs on the render thread — so doing at most one neighbor's warm-render per frame keeps GPU work bounded (rule 2) and never blocks a frame. This subsumes today's disk-2048 prefetch for the warm case, but the disk write-back is still produced for cold persistence across sessions.

The `!Send` edit-pipeline constraint (the tile producer is `Rc`-based, UI-thread only) is exactly why the warm-render step is render-thread-bound rather than fully off-thread; the design accommodates it instead of fighting it.

### Budget & eviction (tunable)

The adaptive budget becomes the **real byte-accounted cap** for the warm cache, tracking retained CPU bytes plus an estimate of retained GPU bytes (rung-1 textures + the M pyramids). Its three parameters become **named `pub const`s in `diag_mem`** (the existing home of `adaptive_budget` from Phase 0) — the single place to tune if more headroom is wanted later:

```rust
// in diag_mem.rs — hoisted from Phase-0's inline `/ 100 * 15` + function-local FLOOR/CEILING.
/// Fraction of total system RAM the warm cache may use, before clamping.
pub const BUDGET_FRACTION_PERCENT: u64 = 15;
/// Lower clamp: never below this even on small-RAM hosts.
pub const BUDGET_FLOOR_BYTES: u64 = 512 * 1024 * 1024;   // 512 MiB
/// Upper clamp: never above this even on large-RAM hosts.
pub const BUDGET_CEILING_BYTES: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB

pub fn adaptive_budget(total_ram: u64) -> u64 {
    (total_ram / 100 * BUDGET_FRACTION_PERCENT)
        .clamp(BUDGET_FLOOR_BYTES, BUDGET_CEILING_BYTES)
}
```

`adaptive_budget` and these constants stay in `diag_mem` (one source of truth); the wiring layer calls `cache.set_budget(diag_mem::adaptive_budget(total_ram))`, so the pure `develop::cache` core takes the budget as a plain number and never re-derives it.

Primary bounds remain the **counts**, also named `pub const`s (co-located in `develop::cache`): the display window **`WARM_WINDOW_FORWARD = 4`** and **`WARM_WINDOW_BACK = 2`** neighbors, and the full-pipeline count **`WARM_FULL_COUNT = 2`**. The byte budget is the safety ceiling that evicts when full-res retention would overshoot those counts on a small-RAM host. All five values (three budget + two window/count groups) are tunable in one glance.

Eviction is LRU by last-touch, never evicting the open image or the forward-prefetch core. An edit to image X inserts `(X, new_hash)` and the stale `(X, old_hash)` ages out by LRU.

### Reveal integration

- On open, consult the warm cache **first**. A **full-pipeline hit** rebuilds producer + sparse VT from the cached `Arc`s → instant fit + 1:1. A **display hit** wraps the cached texture into the preview VT → instant fit; the sparse full then streams as today. A **miss** falls through to the shipped ladder unchanged: Tier-0 thumbnail → disk-2048 → decode.
- The current image's rendered state is *inserted* into the warm cache on reveal (so back-navigation is instant), and marked never-evict while open.

### Diagnostics

The F10 memory overlay's `ram_cache` category (already modeled, currently 0) now reports the warm cache's real `resident_bytes()`, subtotaled per tier, so the memory-for-speed trade is observable live and the budget's effect (evictions) is visible.

## Module boundaries

- **`ferrolite-app/src/develop/cache.rs`** — pure LRU + byte accounting + eviction + the window/count constants (`WARM_WINDOW_FORWARD`/`WARM_WINDOW_BACK`/`WARM_FULL_COUNT`); headless unit tests. Opaque GPU `Arc` handles; takes the budget as a plain number via `set_budget`.
- **`viewer` / `app.rs` reveal flow** — warm-hit short-circuit + insert-on-reveal + the bounded one-per-frame warm-render step.
- **`develop::preview_cache::spawn_prefetch`** — extended to also deliver decoded sources (+ loaded op stacks) for warm-rendering, keeping its serialized single-job discipline.
- **`diag_mem`** — owns the budget constants (`BUDGET_FRACTION_PERCENT`/`BUDGET_FLOOR_BYTES`/`BUDGET_CEILING_BYTES`) and `adaptive_budget` (hoisted to named consts); `ram_cache` gather wired to `cache.resident_bytes()`.

## Testing

Headless unit tests (repo pattern): keying + `op_stack_hash` miss-on-edit, byte accounting, LRU eviction order, never-evict-open, full-tier count cap (M), budget clamp at floor/ceiling/mid using the named constants, forward-biased neighbor selection. GPU/reveal wiring and the one-per-frame warm-render are visual (author test). No golden-image work.

## Visual test plan (for the author, per CLAUDE.md)

Run `FERROLITE_DIAG=1 cargo run --release`; use a shoot with a mix of **edited and unedited** RAW + JPG.

1. **Instant forward culling.** Open an image, then arrow/click forward through the filmstrip. → each next image reveals **sharp almost immediately** (warm display hit), not just Tier-0-soft-then-decode. Failure: a soft thumbnail that visibly resolves to sharp on every step.
2. **Instant back-and-forth (full tier).** Open A, open B, back to A. → A is instant at **fit AND 1:1 zoom** (full-pipeline warm), no pyramid rebuild pause.
3. **Edited neighbor warms with edits.** Edit image X, move away, come back (or prefetch reaches it). → X reveals **with its edits** instantly, no unedited flash.
4. **Edit invalidates.** Edit the open image; move away and back. → the *new* edit shows (old warm entry did not resurrect a stale render).
5. **Budget bound (F10).** Scroll a large shoot fast; watch `ram_cache` in the memory overlay. → it rises to the budget and **plateaus** (evictions), `rss`/`unattributed` bounded, `pyr#` small (≈ open + M + prefetch-in-flight). Failure: monotonic climb past budget.
6. **No regressions.** Tier-0 placeholder, disk fast re-open, edits/crop/mask/split, per-control reset, no freeze on open/navigation all still behave.

## Non-goals / YAGNI

- No change to the `ViewerGpu` single-holder architecture (cache holds `Arc`s; swaps rebuild wrappers).
- No byte-level VRAM budgeting API — GPU is bounded by counts (N display textures + M pyramids) with an estimate folded into the byte cap.
- No f16 buffer trim (separate effort).
- No new crate — `develop::cache` module until a second consumer exists.
- No v2-UI work here — this is the engine the v2 filmstrip will consume.
