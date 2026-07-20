# Develop Warm-Navigation Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make filmstrip navigation in Develop instant — clicking a recent/neighbor image reveals it sharp, immediately, with its edits — via a two-level in-RAM warm cache (display-tier window + full-pipeline for the 1–2 most-recent), fed by proactive forward-biased, edited-aware prefetch, under a tunable byte budget.

**Architecture:** A pure, headless-testable `develop::cache` module holds `Arc` handles (never the live `ViewerGpu`), keyed by `(image_id, op_stack_hash)`. On open the reveal flow consults the cache first: a full-pipeline hit rebuilds the producer + sparse VT from cached `Arc`s (instant 1:1); a display hit wraps a cached `Arc<wgpu::Texture>` into the preview VT (instant fit); a miss falls through the shipped Tier-0 → disk-2048 → decode ladder. Prefetch decodes neighbor sources off-thread and their edited display texture is rendered on the render thread bounded to one-per-frame (the edit pipeline is `Rc`-based / `!Send`). The single-`ViewerGpu`-holder architecture is untouched — swaps rebuild cheap wrappers.

**Tech Stack:** Rust, egui/eframe + wgpu, `ferrolite-jobs`, `ferrolite-previews::hash_serde`, `ferrolite_vt::VirtualTexture::{single_from_texture,single_texture_arc,sparse}`, `ferrolite_pipeline::GpuPyramidSource`. Spec: `docs/superpowers/specs/2026-07-20-develop-warm-navigation-cache-design.md`.

## Global Constraints

- **Scope:** all changes in crate `ferrolite-app` (`develop/`, `viewer/`, `app.rs`, `diag_mem.rs`). **Scoped gate = `ferrolite-app` only:** `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`. The coordinator runs the repo gate once at end of branch.
- **Toolchain:** coordinator runs `rustup update stable` before the repo gate (note if sandbox-blocked); fix code forward-compatibly, never pin.
- **Newest-stable float-literal lint:** suffix any new egui/GPU float literal in f32 context `_f32` or `float-literal-f32-fallback` reddens CI.
- **Threading (load-bearing, CLAUDE.md rule 1):** no decode/encode/IO or multi-ms CPU on the UI thread — neighbor decode + sidecar read go to `ferrolite-jobs`. The warm-render (edit-pipeline eval) runs on the render thread but is **bounded to one neighbor per frame**.
- **GPU (load-bearing, CLAUDE.md rule 2):** build pipelines once (reuse the pre-warmed `ViewerPipelines`); no per-frame pipeline rebuild; the warm cache holds `Arc` handles, adds no new pipeline.
- **Single-holder invariant:** do NOT turn `ViewerGpu` into a multi-image map. The cache stores `Arc<wgpu::Texture>` / `Arc<GpuPyramidSource>` / `Arc<dyn TileSource>`; the single holder is rebuilt from them on swap.
- **Tunable constants:** budget knobs (`BUDGET_FRACTION_PERCENT=15`, `BUDGET_FLOOR_BYTES=512 MiB`, `BUDGET_CEILING_BYTES=4 GiB`) live in `diag_mem`; window/count knobs (`WARM_WINDOW_FORWARD=4`, `WARM_WINDOW_BACK=2`, `WARM_FULL_COUNT=2`) live in `develop::cache`. All as named `pub const`.
- **Diagnostics zero-cost when off:** gather behind `crate::diag::enabled()` as existing recorders do.
- Line width 100, rustfmt defaults, 4-space indent, `-D warnings`.

## File Structure

- **Create `ferrolite-app/src/develop/cache.rs`** — the pure warm-cache: `CacheKey`, `DisplayEntry`, `FullEntry`, `WarmHit`, `WarmCache` (LRU + byte accounting + eviction + never-evict-open + full-tier count cap), window/count constants, and the pure forward-biased `warm_window` neighbor selector. No egui/GPU logic — GPU `Arc`s are opaque.
- **Modify `ferrolite-app/src/develop/mod.rs`** — declare `pub mod cache;`.
- **Modify `ferrolite-app/src/diag_mem.rs`** — hoist budget constants to named `pub const`; `adaptive_budget` uses them.
- **Modify `ferrolite-app/src/state.rs`** — hold the `WarmCache` on `AppState` and a small warm-render request queue.
- **Modify `ferrolite-app/src/viewer/mod.rs`** — `ViewerState` carries its `op_stack_hash` accessor use; no structural change beyond what tasks specify.
- **Modify `ferrolite-app/src/app.rs`** — reveal-flow warm-hit short-circuit + insert-on-reveal (display + full tiers), the bounded one-per-frame warm-render drain, prefetch extension, and the `ram_cache` diag gather.

---

## Phase A — Foundation + display-tier warm (reactive back-navigation)

## Task 1: Hoist budget constants in `diag_mem`

**Files:**
- Modify: `ferrolite-app/src/diag_mem.rs`

**Interfaces:**
- Produces: `pub const BUDGET_FRACTION_PERCENT: u64`, `pub const BUDGET_FLOOR_BYTES: u64`, `pub const BUDGET_CEILING_BYTES: u64`; `adaptive_budget(total_ram: u64) -> u64` unchanged in behavior, now referencing them.

- [ ] **Step 1: Update the existing test to pin the named constants**

In `diag_mem.rs`'s `#[cfg(test)] mod tests`, the test `adaptive_budget_clamps_to_floor_and_ceiling` currently uses local `FLOOR`/`CEIL`. Replace those locals with the new public constants:

```rust
    #[test]
    fn adaptive_budget_clamps_to_floor_and_ceiling() {
        // Tiny RAM -> floor.
        assert_eq!(adaptive_budget(1024 * 1024 * 1024), BUDGET_FLOOR_BYTES);
        // Huge RAM -> ceiling.
        assert_eq!(adaptive_budget(128 * 1024 * 1024 * 1024), BUDGET_CEILING_BYTES);
        // Mid RAM -> fraction of it (overflow-safe divide-then-multiply).
        let mid = 16u64 * 1024 * 1024 * 1024;
        assert_eq!(adaptive_budget(mid), mid / 100 * BUDGET_FRACTION_PERCENT);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app diag_mem::tests::adaptive_budget 2>&1 | tail -20`
Expected: FAIL — `BUDGET_FLOOR_BYTES` etc. not found.

- [ ] **Step 3: Hoist the constants + rewrite `adaptive_budget`**

Replace the existing `adaptive_budget` fn (and its function-local `FLOOR`/`CEILING`) with:

```rust
/// Fraction of total system RAM the develop warm cache may use, before clamping.
/// Tunable: raise for more warm-navigation headroom on RAM-rich hosts.
pub const BUDGET_FRACTION_PERCENT: u64 = 15;
/// Lower clamp for the warm-cache budget — never below this on small-RAM hosts.
pub const BUDGET_FLOOR_BYTES: u64 = 512 * 1024 * 1024; // 512 MiB
/// Upper clamp for the warm-cache budget — never above this on large-RAM hosts.
pub const BUDGET_CEILING_BYTES: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB

/// Adaptive warm-cache byte budget = clamp(fraction × total RAM, floor, ceiling).
/// Divide-then-multiply avoids `u64` overflow on large-RAM hosts.
pub fn adaptive_budget(total_ram: u64) -> u64 {
    (total_ram / 100 * BUDGET_FRACTION_PERCENT).clamp(BUDGET_FLOOR_BYTES, BUDGET_CEILING_BYTES)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app diag_mem:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app diag_mem::
git add ferrolite-app/src/diag_mem.rs
git commit -m "refactor(diag): hoist warm-cache budget knobs to named tunable consts"
```

---

## Task 2: `develop::cache` core — keys, display entries, get/insert, byte accounting

**Files:**
- Create: `ferrolite-app/src/develop/cache.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (declare `pub mod cache;`)

**Interfaces:**
- Produces:
  - `pub const WARM_WINDOW_FORWARD: usize = 4`, `pub const WARM_WINDOW_BACK: usize = 2`, `pub const WARM_FULL_COUNT: usize = 2`.
  - `struct CacheKey { image_id: i64, op_stack_hash: u64 }` (Copy, Eq, Hash).
  - `struct DisplayEntry { tex: Arc<wgpu::Texture>, dims: (u32, u32), bytes: u64 }` (Clone).
  - `enum WarmHit { Full { full: FullEntry, display: DisplayEntry }, Display(DisplayEntry), Miss }` (Task 5 adds the `Full` payload; here define `Display`/`Miss` and a placeholder `FullEntry` unit is NOT created yet — see note).
  - `struct WarmCache` with `new(budget: u64) -> Self`, `get(&mut self, CacheKey) -> WarmHit`, `insert_display(&mut self, CacheKey, DisplayEntry)`, `resident_bytes(&self) -> u64`, `set_budget(&mut self, u64)`, `set_open(&mut self, Option<CacheKey>)`, `len_display(&self) -> usize`.
- Test-only: a helper to fabricate a `DisplayEntry` without a real GPU texture — see Step 1.

> **GPU-in-pure-tests note:** `DisplayEntry.tex` is `Arc<wgpu::Texture>`, which cannot be constructed headlessly. To keep the cache logic pure and testable, `WarmCache` must depend ONLY on `CacheKey` + `bytes` + last-touch for all logic (LRU, budget, eviction) and treat `tex`/`dims` as opaque payload it never inspects. Tests construct entries via a `#[cfg(test)]` constructor `DisplayEntry::for_test(bytes: u64)` that stores a `None` texture behind an `Option`, OR — simpler and preferred — the cache stores entries in a generic inner map and tests exercise a `WarmCache` whose payload type is swapped for `u64`. Use the concrete approach in Step 3 (an `Option<Arc<wgpu::Texture>>` in `DisplayEntry`, `None` in tests) so no generics leak into the public API.

- [ ] **Step 1: Declare the module + write failing tests**

In `ferrolite-app/src/develop/mod.rs` add `pub mod cache;` (alphabetical among the `pub mod` lines).

Create `ferrolite-app/src/develop/cache.rs` with the test module:

```rust
//! Develop warm-navigation cache: a two-level in-RAM cache of recently-shown
//! render state, keyed by `(image_id, op_stack_hash)`, so filmstrip navigation
//! reveals instantly. All LRU / budget / eviction logic is pure and headless-
//! tested; GPU `Arc` handles are opaque payload the cache never inspects. See
//! docs/superpowers/specs/2026-07-20-develop-warm-navigation-cache-design.md.

#[cfg(test)]
mod tests {
    use super::*;

    fn disp(bytes: u64) -> DisplayEntry {
        DisplayEntry { tex: None, dims: (0, 0), bytes }
    }
    fn key(id: i64, hash: u64) -> CacheKey {
        CacheKey { image_id: id, op_stack_hash: hash }
    }

    #[test]
    fn display_hit_after_insert_and_miss_otherwise() {
        let mut c = WarmCache::new(1 << 30);
        assert!(matches!(c.get(key(1, 0)), WarmHit::Miss));
        c.insert_display(key(1, 0), disp(100));
        assert!(matches!(c.get(key(1, 0)), WarmHit::Display(_)));
        // Different op_stack_hash for the same image is a distinct key (edit miss).
        assert!(matches!(c.get(key(1, 9)), WarmHit::Miss));
    }

    #[test]
    fn resident_bytes_sums_display_entries() {
        let mut c = WarmCache::new(1 << 30);
        c.insert_display(key(1, 0), disp(100));
        c.insert_display(key(2, 0), disp(250));
        assert_eq!(c.resident_bytes(), 350);
        // Re-inserting the same key replaces (does not double-count).
        c.insert_display(key(1, 0), disp(400));
        assert_eq!(c.resident_bytes(), 650);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-app develop::cache::tests 2>&1 | tail -20`
Expected: FAIL — types not defined.

- [ ] **Step 3: Implement the core (display tier only; eviction is Task 3)**

Prepend to `cache.rs` (above the tests):

```rust
use std::collections::HashMap;
use std::sync::Arc;

/// Neighbors warmed AHEAD of the current image (the culling direction).
pub const WARM_WINDOW_FORWARD: usize = 4;
/// Neighbors warmed BEHIND the current image.
pub const WARM_WINDOW_BACK: usize = 2;
/// How many most-recent images also retain the full pipeline (instant 1:1).
pub const WARM_FULL_COUNT: usize = 2;

/// Identity of a cached render: an edit (new op stack) yields a new hash, so the
/// stale entry is a natural miss and ages out by LRU — no manual invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub image_id: i64,
    pub op_stack_hash: u64,
}

/// Display-tier payload: the edited rung-1 display texture (fit-view sharp).
/// `tex` is `None` only in headless tests; production always stores `Some`.
#[derive(Clone)]
pub struct DisplayEntry {
    pub tex: Option<Arc<wgpu::Texture>>,
    pub dims: (u32, u32),
    pub bytes: u64,
}

/// Result of consulting the cache for a key. `Full` is populated by Task 5.
pub enum WarmHit {
    Full { full: FullEntry, display: DisplayEntry },
    Display(DisplayEntry),
    Miss,
}

/// A monotonically increasing tick used as the LRU last-touch stamp.
type Touch = u64;

struct Slot<E> {
    entry: E,
    touched: Touch,
}

pub struct WarmCache {
    budget: u64,
    clock: Touch,
    open: Option<CacheKey>,
    display: HashMap<CacheKey, Slot<DisplayEntry>>,
    // Full tier added in Task 5.
    full: HashMap<CacheKey, Slot<FullEntry>>,
}

impl WarmCache {
    pub fn new(budget: u64) -> Self {
        Self {
            budget,
            clock: 0,
            open: None,
            display: HashMap::new(),
            full: HashMap::new(),
        }
    }

    fn tick(&mut self) -> Touch {
        self.clock += 1;
        self.clock
    }

    pub fn set_budget(&mut self, budget: u64) {
        self.budget = budget;
        self.evict_to_budget();
    }

    /// Mark which key is the currently-open image — it is never evicted.
    pub fn set_open(&mut self, open: Option<CacheKey>) {
        self.open = open;
    }

    pub fn get(&mut self, key: CacheKey) -> WarmHit {
        let now = self.tick();
        if let Some(f) = self.full.get_mut(&key) {
            f.touched = now;
            let full = f.entry.clone();
            if let Some(d) = self.display.get_mut(&key) {
                d.touched = now;
                return WarmHit::Full { full, display: d.entry.clone() };
            }
        }
        if let Some(d) = self.display.get_mut(&key) {
            d.touched = now;
            return WarmHit::Display(d.entry.clone());
        }
        WarmHit::Miss
    }

    pub fn insert_display(&mut self, key: CacheKey, entry: DisplayEntry) {
        let now = self.tick();
        self.display.insert(key, Slot { entry, touched: now });
        self.evict_to_budget();
    }

    /// Total resident bytes across both tiers (feeds the F10 `ram_cache` gauge).
    pub fn resident_bytes(&self) -> u64 {
        let d: u64 = self.display.values().map(|s| s.entry.bytes).sum();
        let f: u64 = self.full.values().map(|s| s.entry.bytes).sum();
        d + f
    }

    pub fn len_display(&self) -> usize {
        self.display.len()
    }
}
```

Add a minimal `FullEntry` stub so the types compile now; Task 5 fills its fields:

```rust
/// Full-pipeline payload (Task 5 populates the GPU `Arc`s). Kept minimal here so
/// the `WarmHit::Full` variant and the `full` map type-check from Task 2.
#[derive(Clone)]
pub struct FullEntry {
    pub bytes: u64,
}
```

Add a no-op `evict_to_budget` for now (real logic in Task 3):

```rust
impl WarmCache {
    fn evict_to_budget(&mut self) {
        // Implemented in Task 3.
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-app develop::cache::tests 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app develop::cache
git add ferrolite-app/src/develop/cache.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(develop): warm-cache core — keys, display entries, get/insert, byte accounting"
```

> Note: `WarmCache.full` + `FullEntry` are unused until Task 5/6; add `#[allow(dead_code)]` on `FullEntry`, the `full` field, and `WarmHit::Full` with a `// wired by Task 5/6` comment to satisfy `-D warnings`, and REMOVE those allows in Task 5.

---

## Task 3: `develop::cache` eviction — LRU, never-evict-open, budget

**Files:**
- Modify: `ferrolite-app/src/develop/cache.rs`

**Interfaces:**
- Produces: real `evict_to_budget` behavior; eviction never removes `self.open`; evicts least-recently-touched display entries until `resident_bytes() <= budget`.

- [ ] **Step 1: Write failing tests**

Add to the `tests` module:

```rust
    #[test]
    fn evicts_least_recently_touched_over_budget() {
        let mut c = WarmCache::new(300);
        c.insert_display(key(1, 0), disp(100)); // touch 1
        c.insert_display(key(2, 0), disp(100)); // touch 2
        c.insert_display(key(3, 0), disp(100)); // touch 3 -> 300, at budget
        assert_eq!(c.resident_bytes(), 300);
        // Touch #1 so it is now most-recent.
        assert!(matches!(c.get(key(1, 0)), WarmHit::Display(_)));
        // Insert #4 -> over budget by 100 -> evict the LRU, which is now #2.
        c.insert_display(key(4, 0), disp(100));
        assert_eq!(c.resident_bytes(), 300);
        assert!(matches!(c.get(key(2, 0)), WarmHit::Miss), "LRU #2 evicted");
        assert!(matches!(c.get(key(1, 0)), WarmHit::Display(_)), "#1 kept (touched)");
    }

    #[test]
    fn never_evicts_the_open_image() {
        let mut c = WarmCache::new(150);
        c.insert_display(key(1, 0), disp(100)); // oldest
        c.set_open(Some(key(1, 0)));
        // Insert #2 -> 200 > 150; #1 is oldest but open, so #2... must free space
        // WITHOUT touching #1. With only #1 and #2 present and #1 protected, the
        // cache evicts #2's competitors — here nothing else — so it may exceed
        // budget rather than drop the open image. Assert #1 survives.
        c.insert_display(key(2, 0), disp(100));
        assert!(matches!(c.get(key(1, 0)), WarmHit::Display(_)), "open image never evicted");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-app develop::cache::tests::evict 2>&1 | tail; cargo test -p ferrolite-app develop::cache::tests::never 2>&1 | tail`
Expected: FAIL — no eviction yet (`resident_bytes` stays 400 / assertions fail).

- [ ] **Step 3: Implement eviction**

Replace the stub `evict_to_budget`:

```rust
    /// Evict least-recently-touched DISPLAY entries until within budget. The open
    /// image is never evicted (skipped as a candidate). The full tier is bounded
    /// by count (Task 5), not evicted here. If the only remaining candidates are
    /// protected, the cache may briefly exceed budget rather than drop the open
    /// image — a bounded overshoot of at most the open image's own bytes.
    fn evict_to_budget(&mut self) {
        while self.resident_bytes() > self.budget {
            // Pick the LRU display entry that is not the open image.
            let victim = self
                .display
                .iter()
                .filter(|(k, _)| Some(**k) != self.open)
                .min_by_key(|(_, s)| s.touched)
                .map(|(k, _)| *k);
            match victim {
                Some(k) => {
                    self.display.remove(&k);
                }
                None => break, // only protected entries remain
            }
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-app develop::cache 2>&1 | tail -20`
Expected: PASS (all cache tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app develop::cache
git add ferrolite-app/src/develop/cache.rs
git commit -m "feat(develop): warm-cache LRU eviction with never-evict-open guard"
```

---

## Task 4: Display-tier reveal integration (reactive instant back-navigation)

**Files:**
- Modify: `ferrolite-app/src/state.rs` (hold `WarmCache`)
- Modify: `ferrolite-app/src/viewer/mod.rs` (an `op_stack_hash()` helper on `ViewerState`)
- Modify: `ferrolite-app/src/app.rs` (consult on open; insert on reveal)

**Interfaces:**
- Consumes: `develop::cache::{WarmCache, CacheKey, DisplayEntry, WarmHit}`, `diag_mem::adaptive_budget`, `mem_probe::total_ram_bytes`, `VirtualTexture::{single_from_texture, single_texture_arc, single_dims}`.
- Produces: `AppState.warm_cache: develop::cache::WarmCache`; `ViewerState::op_stack_hash(&self) -> u64`; warm display-hit short-circuit + insert-on-reveal.

- [ ] **Step 1: Add `op_stack_hash` to `ViewerState` + test**

In `viewer/mod.rs`, add a method (near `op_stack` usage):

```rust
    /// Stable hash of this viewer's current op stack, for warm-cache keying.
    /// Uses the same `hash_serde` the disk preview cache keys with, so identical
    /// stacks collide intentionally and any edit changes the hash.
    pub fn op_stack_hash(&self) -> u64 {
        ferrolite_previews::hash_serde(&self.op_stack)
    }
```

Add to the `viewer` tests:

```rust
    #[test]
    fn op_stack_hash_changes_with_edits() {
        let mut v = ViewerState::open(1, std::path::PathBuf::from("x.raw"), FileKind::Raw);
        let h0 = v.op_stack_hash();
        v.op_stack = v.op_stack.set_op(ferrolite_pipeline::Op::Exposure(
            ferrolite_pipeline::Exposure { ev: 1.0 },
        ));
        assert_ne!(h0, v.op_stack_hash(), "an edit must change the warm-cache hash");
    }
```

Run: `cargo test -p ferrolite-app viewer::tests::op_stack_hash 2>&1 | tail` → RED then GREEN.

- [ ] **Step 2: Hold the `WarmCache` on `AppState`**

In `state.rs`, add a field to `struct AppState` (near `thumb_pixels`):

```rust
    /// Develop warm-navigation cache (two-level: display + full pipeline).
    pub warm_cache: crate::develop::cache::WarmCache,
```

Initialize it in BOTH `AppState` constructors (search `thumb_pixels: crate::library::thumb_pixel_cache::ThumbPixelCache::new(` — there are two, prod + test):

```rust
            warm_cache: crate::develop::cache::WarmCache::new(
                crate::diag_mem::adaptive_budget(crate::mem_probe::total_ram_bytes()),
            ),
```

- [ ] **Step 3: Consult the warm cache on open (display hit)**

In `app.rs` `open_record` (after `self.state.open_image_in_viewer(rec);`), add a warm-hit attempt. If the just-opened viewer's key is a display hit, reveal it immediately by wrapping the cached texture into a preview `ViewerGpu` holder — mirroring `reveal_srgb_preview`'s holder install, but from the cached texture instead of a fresh color pass. Because this needs the render state, factor it into a helper `try_warm_reveal(&mut self, frame, image_id) -> bool` called from the open flow / a suitable frame point where `frame.wgpu_render_state()` is available (the same place `reveal_srgb_preview` is reachable). Add:

```rust
    /// If the open viewer's `(image_id, op_stack_hash)` is warm in the cache,
    /// install its cached render immediately and return true (skipping Tier-0/1/
    /// decode). A `Full` hit installs the full pipeline (instant 1:1); a `Display`
    /// hit installs the rung-1 preview VT (instant fit; sparse full streams as
    /// today). A miss returns false and the normal ladder runs.
    fn try_warm_reveal(&mut self, frame: &eframe::Frame, image_id: i64) -> bool {
        let Some(rs) = frame.wgpu_render_state() else {
            return false;
        };
        let key = match self.state.viewer.as_ref() {
            Some(v) if v.image_id == image_id => crate::develop::cache::CacheKey {
                image_id,
                op_stack_hash: v.op_stack_hash(),
            },
            _ => return false,
        };
        let hit = self.state.warm_cache.get(key);
        let display = match hit {
            crate::develop::cache::WarmHit::Miss => return false,
            crate::develop::cache::WarmHit::Display(d) => d,
            // Full-tier install is Task 6; for Phase A treat Full as its display.
            crate::develop::cache::WarmHit::Full { display, .. } => display,
        };
        let Some(tex) = display.tex.clone() else {
            return false;
        };
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
        let vt = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            self.apply_display_tail(&gpu, vp);
            ferrolite_vt::VirtualTexture::single_from_texture(
                &gpu,
                tex,
                display.dims,
                &vp.pipelines,
            )
        };
        self.install_preview_holder(frame, image_id, vt, display.dims);
        true
    }
```

> Implementer notes:
> - `install_preview_holder` is the shared holder-install + fit + `loaded=true` currently inlined at the tail of `reveal_srgb_preview` (`app.rs:303–347`). Extract that tail into a `fn install_preview_holder(&mut self, frame, image_id, vt, dims)` and call it from BOTH `reveal_srgb_preview` and here, so there is one holder-install path (DRY). Verify the exact fields it sets (`v.view`, `v.image_dims`, `v.loaded`, `v.idle`, the `PresentBuffers`, `ViewerGpu` insert, `mark_histogram_dirty`).
> - `apply_display_tail` already exists and is called the same way in `reveal_srgb_preview`.
> - Set `v.idle = false` for a display hit (the sparse full still streams) unless the full tier was installed (Task 6).
> - Call `try_warm_reveal` in the open flow BEFORE the normal preview/decode submission so a hit short-circuits; on `true`, skip the `spawn_preview`/cache-read gating for this open (guard the existing open-flow block on `!v.loaded` or a new `v.warm_revealed` flag so it doesn't also decode). Confirm the exact gate against the real open-flow (added in Phases 2–3).

- [ ] **Step 4: Insert into the warm cache on every reveal**

At the end of the display reveal (`reveal_srgb_preview`, after the holder is installed and `loaded=true`) AND the RAW reveal (`apply_full_decoded`, after the rung-1 preview VT is installed), capture the rung-1 texture and insert a `DisplayEntry`. Add a shared helper:

```rust
    /// Record the just-installed rung-1 display texture into the warm cache so a
    /// later re-open of this `(image_id, op_stack_hash)` reveals instantly.
    fn warm_insert_display(&mut self, frame: &eframe::Frame, image_id: i64) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let key = match self.state.viewer.as_ref() {
            Some(v) if v.image_id == image_id => crate::develop::cache::CacheKey {
                image_id,
                op_stack_hash: v.op_stack_hash(),
            },
            _ => return,
        };
        let (tex, dims) = {
            let renderer = rs.renderer.read();
            let Some(g) = renderer.callback_resources.get::<viewer::ViewerGpu>() else {
                return;
            };
            if g.image_id != image_id {
                return;
            }
            match (g.preview.single_texture_arc(), g.preview.single_dims()) {
                (Some(t), Some(d)) => (t, d),
                _ => return,
            }
        };
        // Rgba16Float rung-1 texture = 8 B/px.
        let bytes = dims.0 as u64 * dims.1 as u64 * 8;
        self.state.warm_cache.set_open(Some(key));
        self.state
            .warm_cache
            .insert_display(key, crate::develop::cache::DisplayEntry { tex: Some(tex), dims, bytes });
    }
```

Call `self.warm_insert_display(frame, image_id);` at the end of `reveal_srgb_preview` (when it returns true) and at the end of `apply_full_decoded`'s successful preview install. Also call `self.state.warm_cache.set_open(Some(key))` — done inside the helper.

- [ ] **Step 5: Build + full crate test**

Run: `cargo build -p ferrolite-app 2>&1 | tail -20 && cargo test -p ferrolite-app 2>&1 | tail -12`
Expected: compiles; all tests pass (cache + viewer + existing).

- [ ] **Step 6: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/state.rs ferrolite-app/src/viewer/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): display-tier warm reveal — instant back-navigation (fit view)"
```

> **Phase A deliverable:** re-opening a recently-viewed image (same edits) reveals sharp at fit instantly from RAM; the sparse full still streams for 1:1. Author-visual-testable already.

---

## Phase B — full-pipeline tier + proactive edited-aware prefetch + diagnostics

## Task 5: `develop::cache` full tier — `insert_full`, count cap M, byte accounting

**Files:**
- Modify: `ferrolite-app/src/develop/cache.rs`

**Interfaces:**
- Produces: real `FullEntry { pyramid: Arc<GpuPyramidSource>, tile_source: Arc<dyn TileSource + Send + Sync>, op_stack: OpStack, cam: [[f32;3];3], bytes: u64 }`; `insert_full(&mut self, CacheKey, FullEntry)` (count-capped at `WARM_FULL_COUNT`, LRU-evicting the oldest full entry, never the open one); `get` returns `WarmHit::Full` when both tiers present. Remove the Task-2 `#[allow(dead_code)]`.

- [ ] **Step 1: Write failing tests**

Add to `tests` (extend the `disp`/`key` helpers with a `full` fabricator that avoids real GPU `Arc`s — since `FullEntry` holds real GPU types, gate the full-tier tests on a `bytes`-only shadow. Because `GpuPyramidSource`/`TileSource` cannot be built headlessly, test the COUNT-CAP + LRU via a `#[cfg(test)]` `FullEntry::for_test(bytes)` constructor that stores `None` GPU handles):

First adjust `FullEntry` to hold `Option` GPU handles (like `DisplayEntry.tex`) so tests can fabricate it. Then:

```rust
    fn full_e(bytes: u64) -> FullEntry {
        FullEntry { pyramid: None, tile_source: None, op_stack: Default::default(), cam: IDENTITY_CAM, bytes }
    }

    #[test]
    fn full_tier_caps_at_warm_full_count() {
        let mut c = WarmCache::new(1 << 30);
        for i in 0..(WARM_FULL_COUNT as i64 + 2) {
            c.insert_display(key(i, 0), disp(10));
            c.insert_full(key(i, 0), full_e(10));
        }
        assert_eq!(c.len_full(), WARM_FULL_COUNT, "full tier bounded by count");
    }

    #[test]
    fn get_returns_full_when_both_tiers_present() {
        let mut c = WarmCache::new(1 << 30);
        c.insert_display(key(1, 0), disp(10));
        c.insert_full(key(1, 0), full_e(10));
        assert!(matches!(c.get(key(1, 0)), WarmHit::Full { .. }));
        // Full without display degrades to nothing (full needs its display too).
        c.insert_full(key(2, 0), full_e(10));
        assert!(matches!(c.get(key(2, 0)), WarmHit::Miss));
    }
```

Add `const IDENTITY_CAM: [[f32;3];3] = [[1.0,0.0,0.0],[0.0,1.0,0.0],[0.0,0.0,1.0]];` in the test module.

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p ferrolite-app develop::cache::tests::full 2>&1 | tail; cargo test -p ferrolite-app develop::cache::tests::get_returns 2>&1 | tail`
Expected: FAIL.

- [ ] **Step 3: Implement**

Replace the stub `FullEntry` with the real one (GPU handles `Option` so headless tests fabricate `None`):

```rust
use ferrolite_pipeline::GpuPyramidSource;
use ferrolite_pipeline::OpStack;
use ferrolite_vt::TileSource;

/// Full-pipeline payload for the 1–2 most-recent images: the GPU pyramid + tile
/// source + the op stack/cam needed to rebuild the (`!Send`) producer + sparse
/// VT on the render thread. GPU handles are `Option` only so headless tests can
/// fabricate the struct; production always stores `Some`.
#[derive(Clone)]
pub struct FullEntry {
    pub pyramid: Option<Arc<GpuPyramidSource>>,
    pub tile_source: Option<Arc<dyn TileSource + Send + Sync>>,
    pub op_stack: OpStack,
    pub cam: [[f32; 3]; 3],
    pub bytes: u64,
}
```

Add `insert_full` + `len_full` + fix `get` (already returns `Full` when both present — verify it compiles now that `FullEntry` is real):

```rust
impl WarmCache {
    /// Insert a full-pipeline entry, bounding the tier to `WARM_FULL_COUNT` by
    /// evicting the least-recently-touched full entry that is not the open image.
    pub fn insert_full(&mut self, key: CacheKey, entry: FullEntry) {
        let now = self.tick();
        self.full.insert(key, Slot { entry, touched: now });
        while self.full.len() > WARM_FULL_COUNT {
            let victim = self
                .full
                .iter()
                .filter(|(k, _)| Some(**k) != self.open)
                .min_by_key(|(_, s)| s.touched)
                .map(|(k, _)| *k);
            match victim {
                Some(k) => {
                    self.full.remove(&k);
                }
                None => break,
            }
        }
        self.evict_to_budget();
    }

    pub fn len_full(&self) -> usize {
        self.full.len()
    }
}
```

Remove the Task-2 `#[allow(dead_code)]` on `FullEntry`/`full`/`WarmHit::Full`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ferrolite-app develop::cache 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app develop::cache
git add ferrolite-app/src/develop/cache.rs
git commit -m "feat(develop): warm-cache full tier — insert_full, count cap, WarmHit::Full"
```

---

## Task 6: Full-pipeline reveal integration (instant 1:1 on back-and-forth)

**Files:**
- Modify: `ferrolite-app/src/app.rs`

**Interfaces:**
- Consumes: `WarmHit::Full`, `FullEntry`, `VirtualTexture::sparse`, the producer-rebuild path in `apply_pyramid_ready`.

- [ ] **Step 1: Insert the full entry when the pyramid is ready**

In `apply_pyramid_ready` (after `v.pyramid = Some(...)` and the producer is built, `app.rs:~1235–1258`), record the full entry so a re-open skips the ~1.2 s rebuild:

```rust
        // Warm cache: retain this image's full pipeline (GPU pyramid + tile source
        // + stack/cam) so an immediate back-navigation reveals 1:1 instantly.
        {
            let key = crate::develop::cache::CacheKey {
                image_id,
                op_stack_hash: ferrolite_previews::hash_serde(&self.state.viewer.as_ref().map(|v| v.op_stack.clone()).unwrap_or_default()),
            };
            // Estimate: the GpuPyramidSource resident bytes (process-global helper
            // gives the delta poorly, so use a per-image estimate = full-res f32
            // *4/3 mip tail, matching the diag gather).
            let bytes = self
                .state
                .viewer
                .as_ref()
                .and_then(|v| v.image_dims)
                .map(|(w, h)| w as u64 * h as u64 * 8 * 4 / 3)
                .unwrap_or(0);
            self.state.warm_cache.insert_full(
                key,
                crate::develop::cache::FullEntry {
                    pyramid: Some(std::sync::Arc::clone(gpu_pyramid)),
                    tile_source: Some(std::sync::Arc::clone(tile_source)),
                    op_stack: self.state.viewer.as_ref().map(|v| v.op_stack.clone()).unwrap_or_default(),
                    cam,
                    bytes,
                },
            );
        }
```

> Note: compute the `key` op_stack_hash from the SAME viewer op stack used to build the producer (`cam` is already in scope in `apply_pyramid_ready`). Use `v.op_stack_hash()` (Task 4) rather than re-hashing inline if the borrow allows — prefer the helper for consistency.

- [ ] **Step 2: Install the full pipeline on a `WarmHit::Full`**

Extend `try_warm_reveal` (Task 4): when the hit is `WarmHit::Full { full, display }`, after installing the display preview VT, ALSO rebuild the sparse full VT + producer from `full.pyramid`/`full.tile_source`/`full.op_stack`/`full.cam` — the same construction as `apply_pyramid_ready` (build `VirtualTexture::sparse`, `TileEditPipeline::new`, `EditTileProducer`, set `v.pyramid`, `v.edit_producer`, `v.full_ready = true`, install into the holder, `set_producing(true)`). Factor that construction out of `apply_pyramid_ready` into `fn install_full_pipeline(&mut self, frame, image_id, pyramid, tile_source, op_stack, cam)` and call it from BOTH sites (DRY). On a full warm reveal set `v.full_ready = true` immediately (no pyramid job submitted).

> Implementer notes:
> - This is the most intricate wiring in the plan. Keep `apply_pyramid_ready`'s existing behavior identical; only extract the shared installer. Verify the `!Send` producer is built on the UI/render thread here (it is — `try_warm_reveal` runs in the frame update).
> - On a full hit, do NOT submit the pyramid Background job for this open (skip the decode/pyramid path entirely) — guard the open-flow decode submission on the warm-reveal outcome.
> - If `full.pyramid`/`tile_source` is `None` (cannot happen in production, only headless tests), fall back to the display-only path.

- [ ] **Step 3: Build + full crate test**

Run: `cargo build -p ferrolite-app 2>&1 | tail -25 && cargo test -p ferrolite-app 2>&1 | tail -12`
Expected: compiles; all tests pass.

- [ ] **Step 4: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/app.rs
git commit -m "feat(develop): full-pipeline warm reveal — instant 1:1 on back-and-forth"
```

---

## Task 7: Forward-biased neighbor window + prefetch source delivery

**Files:**
- Modify: `ferrolite-app/src/develop/cache.rs` (pure `warm_window` selector)
- Modify: `ferrolite-app/src/develop/preview_cache.rs` or a new `develop/warm_prefetch.rs` (deliver decoded source + op stack)
- Modify: `ferrolite-app/src/events.rs` (a `WarmSourceReady` event) + `app.rs` (dispatch)

**Interfaces:**
- Produces: `warm_window(ids: &[i64], current: i64, forward: usize, back: usize) -> Vec<i64>` (pure, forward-biased, clamped, excludes current); an off-thread job delivering `AppEvent::WarmSourceReady { image_id, source: Arc<LinearRgbaF32>, op_stack, kind, color_profile }` for each neighbor not already warm.

- [ ] **Step 1: Write failing test for `warm_window`**

Add to `cache.rs` tests:

```rust
    #[test]
    fn warm_window_is_forward_biased_and_clamped() {
        let ids = [10, 20, 30, 40, 50, 60, 70];
        // current=30 (pos 2), forward 3 back 1 -> [40,50,60] forward + [20] back,
        // forward first (culling direction), current excluded.
        assert_eq!(warm_window(&ids, 30, 3, 1), vec![40, 50, 60, 20]);
        // Clamp at the end.
        assert_eq!(warm_window(&ids, 70, 3, 1), vec![60]);
        // current absent -> empty.
        assert!(warm_window(&ids, 999, 3, 1).is_empty());
    }
```

- [ ] **Step 2: Implement `warm_window`**

```rust
/// Forward-biased neighbor selection for warm prefetch: up to `forward` ids ahead
/// of `current` (the culling direction, listed first) then up to `back` behind,
/// clamped to the list, excluding `current`. Empty if `current` is absent.
pub fn warm_window(ids: &[i64], current: i64, forward: usize, back: usize) -> Vec<i64> {
    let Some(pos) = ids.iter().position(|id| *id == current) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 1..=forward {
        if let Some(id) = ids.get(pos + i) {
            out.push(*id);
        }
    }
    for i in 1..=back {
        if pos >= i {
            out.push(ids[pos - i]);
        }
    }
    out
}
```

Run: `cargo test -p ferrolite-app develop::cache::tests::warm_window 2>&1 | tail` → RED then GREEN.

- [ ] **Step 3: Off-thread source delivery**

Add a `WarmSourceReady` variant to `AppEvent` (in `events.rs`, following the existing `PreviewReady` shape — include `image_id`, `source: Arc<LinearRgbaF32>`, `op_stack: OpStack`, `kind: FileKind`, `color_profile: ColorProfile`; add its `Debug` arm + `None` toast mapping like the other decode events).

Add `develop/warm_prefetch.rs` with `spawn_warm_sources(jobs, tx, ctx, neighbors: Vec<(i64, PathBuf, FileKind)>, working_space)` — a SINGLE serialized `Background` job (mirroring `spawn_prefetch`'s bounded-concurrency contract) that, per neighbor: reads its op-stack sidecar (reuse `ops_persist` read logic or `decode`), decodes+demosaics the source (reuse the `spawn_prefetch` decode block), and emits `WarmSourceReady`. Cancellable via a handle stored on the viewer (`warm_prefetch_handles`).

> Implementer notes:
> - Reuse the exact decode/demosaic/orientation block from `preview_cache::spawn_prefetch` (RAW: `decode_full` + `QuadBin`/RCD? — use the SAME path `spawn_full` uses so the warm source matches the on-screen render; Standard: `decode_preview`). Confirm which demosaic the on-screen full uses and match it so the warm render is pixel-consistent.
> - Op stack: read the neighbor's `.frl:ops` sidecar off-thread (the same read `spawn_ops_read` does). If absent → default stack.
> - Bound to one job, sequential neighbors, source dropped at end of each iteration (peak = one source), per the memory contract.
> - This task delivers SOURCES only; the render-thread warm-render is Task 8.

- [ ] **Step 4: Dispatch prefetch on settle**

In `app.rs`, where the disk `spawn_prefetch` is dispatched (the `v.loaded && !v.prefetch_requested` block, `app.rs:~4049`), also compute `warm_window(&ids, current_id, WARM_WINDOW_FORWARD, WARM_WINDOW_BACK)`, resolve `(id, path, kind)` for each, and dispatch `spawn_warm_sources`. Store handles; gate one-shot like the disk prefetch.

- [ ] **Step 5: Build + test (handler is a no-op stub until Task 8)**

Add a `WarmSourceReady` handler in the event pump that, for now, just drops the event (Task 8 fills it). Run: `cargo build -p ferrolite-app 2>&1 | tail -20 && cargo test -p ferrolite-app 2>&1 | tail -10`.

- [ ] **Step 6: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/develop/cache.rs ferrolite-app/src/develop/warm_prefetch.rs ferrolite-app/src/develop/mod.rs ferrolite-app/src/events.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): forward-biased warm window + off-thread neighbor source prefetch"
```

---

## Task 8: Bounded one-per-frame warm-render (edited-aware)

**Files:**
- Modify: `ferrolite-app/src/state.rs` (a small queue of pending `WarmSourceReady` payloads)
- Modify: `ferrolite-app/src/app.rs` (queue on event; drain one per frame → render → `insert_display`)

**Interfaces:**
- Consumes: the `WarmSourceReady` payload; the rung-1 edit-pipeline eval (as in `apply_full_decoded`); `WarmCache::insert_display`.

- [ ] **Step 1: Queue warm sources**

Add to `AppState`: `pub warm_render_queue: std::collections::VecDeque<crate::events::WarmSourcePayload>` (define `WarmSourcePayload` as the owned fields of the event). In the `WarmSourceReady` handler, push onto the queue (bounded — cap length at `WARM_WINDOW_FORWARD + WARM_WINDOW_BACK`, dropping oldest, so a fast scrub can't pile up).

- [ ] **Step 2: Drain one per frame on the render thread**

In the frame update (a suitable point with `frame.wgpu_render_state()`, e.g. near `drive_viewer`), if the queue is non-empty, pop ONE payload and warm-render it:

```rust
    /// Render at most ONE queued warm neighbor's edited rung-1 display texture per
    /// frame (bounded GPU work, CLAUDE.md rule 2) and insert it into the warm cache
    /// so clicking that neighbor reveals instantly. The heavy decode already
    /// happened off-thread (`spawn_warm_sources`); this is the fast GPU edit pass,
    /// the same one every open runs.
    fn drain_one_warm_render(&mut self, frame: &eframe::Frame) {
        let Some(payload) = self.state.warm_render_queue.pop_front() else {
            return;
        };
        let Some(rs) = frame.wgpu_render_state() else {
            self.state.warm_render_queue.push_front(payload); // retry next frame
            return;
        };
        let key = crate::develop::cache::CacheKey {
            image_id: payload.image_id,
            op_stack_hash: ferrolite_previews::hash_serde(&payload.op_stack),
        };
        // Already warm at this exact stack? skip.
        if !matches!(self.state.warm_cache.get(key), crate::develop::cache::WarmHit::Miss) {
            return;
        }
        // Build the rung-1 edited texture from the decoded source, exactly as
        // apply_full_decoded builds its reveal preview (EditPipeline over the
        // full-res source with the neighbor's op stack + cam), but WITHOUT
        // installing a holder — we only want the output texture.
        let cam = /* compose camera->working for payload.color_profile + op_stack WB */;
        let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let mut ep = ferrolite_pipeline::EditPipeline::new(
            ctx_arc.clone(),
            &payload.source,
            payload.op_stack.clone(),
            cam,
        );
        let out = ep.evaluate();
        let tex = out.texture.clone();
        let dims = (out.width, out.height);
        let bytes = dims.0 as u64 * dims.1 as u64 * 8;
        self.state.warm_cache.insert_display(
            key,
            crate::develop::cache::DisplayEntry { tex: Some(tex), dims, bytes },
        );
        frame_repaint_hint(); // request_repaint so remaining queue drains promptly
    }
```

> Implementer notes:
> - Compose `cam` the same way `apply_full_decoded` does for the OPEN image but for the neighbor's `payload.color_profile` and `payload.op_stack` WB — factor a helper `camera_to_working_for(profile, op_stack, working_space)` if one doesn't cleanly exist; reuse `source_to_working`/`camera_to_working` patterns. Standard images use the sRGB→working `preview_to_working` path instead of camera→working — branch on `payload.kind` exactly as the reveal does (`apply_preview_ready` vs `apply_full_decoded`).
> - `EditPipeline::new` + `evaluate` is the SAME rung-1 build `apply_full_decoded` uses; verify the constructor signature and that lens/vignette uniforms default correctly for a not-yet-lens-baked neighbor (identity is fine; the neighbor re-bakes on real open).
> - `request_repaint` after a render so the next queued neighbor drains on the following frame (one-per-frame cadence, not all-at-once).
> - This is GPU/threading code — no unit test; verified in the visual test. The pure cache insert is already covered.

- [ ] **Step 3: Build + full test**

Run: `cargo build -p ferrolite-app 2>&1 | tail -25 && cargo test -p ferrolite-app 2>&1 | tail -10`
Expected: compiles; tests pass.

- [ ] **Step 4: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/state.rs ferrolite-app/src/app.rs ferrolite-app/src/events.rs
git commit -m "feat(develop): bounded one-per-frame edited warm-render into the cache"
```

---

## Task 9: Diagnostics — `ram_cache` reports real warm bytes

**Files:**
- Modify: `ferrolite-app/src/app.rs` (`gather_mem_breakdown`)

**Interfaces:**
- Consumes: `WarmCache::resident_bytes`.

- [ ] **Step 1: Wire the gauge**

In `gather_mem_breakdown` (`app.rs:~2730`), set the `RamCache` category from the warm cache (currently unset / 0):

```rust
        b.set(
            crate::diag_mem::MemCategory::RamCache,
            self.state.warm_cache.resident_bytes(),
        );
```

- [ ] **Step 2: Build + test**

Run: `cargo build -p ferrolite-app 2>&1 | tail -10 && cargo test -p ferrolite-app 2>&1 | tail -8`
Expected: compiles; tests pass.

- [ ] **Step 3: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/app.rs
git commit -m "feat(diag): report warm-cache resident bytes in the ram_cache category"
```

---

## Coordinator wrap-up (not a subagent task)

After Task 9:

1. `rustup update stable` (note if sandbox-blocked), then the **repo gate**:
   `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo build --all-targets && cargo test --workspace`.
2. Hand the author the spec's **Visual test plan** (instant forward culling; instant back-and-forth at fit AND 1:1; edited neighbor warms with edits; edit invalidates; budget bound via F10 `ram_cache` plateau; no regressions), plus the **memory guardrail** (`FERROLITE_DIAG=1`, F10, scroll a large shoot — `ram_cache` plateaus at budget, `pyr#` ≈ open + M + in-flight, `unattributed` flat).
3. **HOLD for the author's hands-on test** before finishing the branch (CLAUDE.md).

---

## Self-Review

- **Spec coverage:** two-level cache keyed by `(image_id, op_stack_hash)` ✓ (Tasks 2,5); display-tier window + full M ✓ (Tasks 2/4, 5/6); `Arc`-handles-not-holder ✓ (Tasks 4,6); proactive forward-biased edited-aware prefetch ✓ (Tasks 7,8); decode-off-thread + one-warm-render-per-frame ✓ (Tasks 7,8); tunable budget consts in `diag_mem` + window/count consts in `develop::cache` ✓ (Tasks 1,2); byte-budget real cap + never-evict-open + LRU ✓ (Task 3); reveal short-circuit + fall-through ladder ✓ (Tasks 4,6); `ram_cache` diag ✓ (Task 9); pure headless tests for all cache logic ✓.
- **Placeholder scan:** the one intentional `/* compose cam */` in Task 8 Step 2 is flagged with an explicit implementer note naming the exact existing helpers to reuse (`camera_to_working`/`source_to_working`/`preview_to_working`) and the `payload.kind` branch — not a silent TODO. All pure-logic steps carry complete code.
- **Type consistency:** `CacheKey`/`DisplayEntry`/`FullEntry`/`WarmHit`/`WarmCache` and `op_stack_hash`/`warm_window`/`adaptive_budget`/`WARM_*` consts are used identically across Tasks 1–9. `FullEntry` GPU handles are `Option` (Task 5) so headless tests fabricate them — matching `DisplayEntry.tex`.
- **Flagged verifications:** `install_preview_holder`/`install_full_pipeline` extraction points, the open-flow decode-skip guard on a warm hit, the exact demosaic path the warm source must match, and `EditPipeline::new`/`evaluate` signatures — each has an inline "verify against real source" note, consistent with the shipped Phase-0 / Phases-2-3 plans.
- **Decomposition:** Phase A (Tasks 1–4) is an independently shippable increment (reactive instant back-navigation at fit); Phase B (5–9) adds full-tier 1:1 + proactive edited prefetch + diagnostics.
