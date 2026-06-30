# Spec 2 Plan 3 — VT halo + GPU tile producer (full-res edited 1:1 view) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the virtual texture tile overlap/halo + a source-agnostic GPU tile-producer seam, and an app/pipeline edit producer that renders the full-res 1:1 view edited per-tile (no CPU readback) — the OpStack applied tile-by-tile with a haloed neighborhood so Sharpen has no seams, crop/rotate applied as a per-tile geometry head transform, edited tiles versioned + lazily re-streamed, all wired into the viewer with the OpStack defaulting to identity.

**Architecture:** Three tiers move in lockstep. (1) **`ferrolite-image`** gains pure halo origin/extent math. (2) **`ferrolite-vt`** (engine tier, photo-agnostic) gains `TileSource::tile_with_halo`, a `TileProducer` GPU-fill seam, a `TilePool` GPU→GPU slot copy, a pure `VersionedResidency` brain, and a producer-backed sparse fill path (bounded, on the render thread, version-invalidated). (3) **`ferrolite-pipeline`** (photo tier) gains an `out_origin` on the geometry pass so it can render a tile sub-region, a `GpuPyramidSource` (the QuadBin pyramid uploaded to the GPU once), and a `TileEditPipeline` that runs geometry-head → color chain over a haloed tile and returns the interior 256². **`ferrolite-app`** defines `EditTileProducer` (impl `ferrolite_vt::TileProducer`) wrapping a `TileEditPipeline`, holds an `OpStack` on `ViewerState` (default identity), and engages the producer for the full-res VT only when the stack is non-identity. The generic `ferrolite-gpu::Graph<O>` executor is **not modified** (contract §4).

**Tech Stack:** Rust (workspace edition), `wgpu 22.1` (compute passes + `copy_texture_to_texture`), WGSL, `bytemuck` Pod uniforms, `half::f16` uploads, `ferrolite-jobs` (Visible-priority + cancellation for the *CPU* source path). Tests: `#[test]` pure units (run everywhere) + headless-skipping golden GPU diffs (`GpuContext::headless()` → `None` ⇒ skip).

## Global Constraints

- **Engine/photo tier boundary stays intact.** Halo math → `ferrolite-image` (engine, permissive). `tile_with_halo` + `TileProducer` trait + `VersionedResidency` + producer fill path → `ferrolite-vt` (engine, permissive, **no photo concepts** — the trait takes only `GpuContext`/`TileCoord` and returns a `wgpu::Texture`). The edit producer + per-tile pipeline → `ferrolite-pipeline`/`ferrolite-app` (photo, GPL-OK). Do **not** add a photo or `ferrolite-pipeline` dependency to `ferrolite-vt` (contract §5; spec §3, §5.2). `ferrolite-pipeline` must **not** depend on `ferrolite-vt` (the `TileProducer` impl lives in `ferrolite-app`, which already may depend on both).
- **Executor frozen.** No `ferrolite-gpu::Graph<O>` changes (contract §4). New per-tile nodes are `Node<PipelineImage>` impls only.
- **Pipelines built once, reused.** Every node/producer builds its `wgpu::ComputePipeline`/`BindGroupLayout`/sampler exactly once in its constructor and reuses it across tiles and edits. The `GpuPyramidSource` uploads the source LODs once on full-decode. Never rebuild a pipeline or re-upload a source per tile/edit/open (CLAUDE.md GPU rule; spec §4.2, §5.2).
- **GPU work stays on the render thread, bounded.** The GPU tile producer **runs on the render thread** (GPU work cannot run on a `ferrolite-jobs` worker). It is bounded by a **per-frame produce budget** and ordered by the VT's needed set. "Cancellable + prioritized" (spec §5.3, contract §1) is realized for produced tiles as: each frame the produce set is recomputed from the current needed set ∖ resident-at-current-version; superseded coords are simply dropped (never produced); a navigation/version bump invalidates + clears pending. The Spec-1 **CPU source-upload** path keeps its `ferrolite-jobs` Visible jobs + cancellation unchanged.
- **No CPU readback on the produce path.** The producer renders the edited tile into a GPU texture; the VT copies it into the pool slot with `copy_texture_to_texture` (GPU→GPU). No `map_async`/`read_*` on the hot path (spec §5.2).
- **VT source-agnostic + Spec-1 paths byte-identical.** Unless the producer drive path is engaged, the sparse VT behaves exactly as Spec 1 (CPU-upload streaming) — the existing rung-1..4 goldens must stay within tolerance (golden gate). `tile(coord)` stays defined as `tile_with_halo(coord, 0)`.
- **The producer is NOT stored in the VT.** `TileEditPipeline` reuses the Plan 1/2 nodes, which hold `Rc<Cell<_>>`/`RefCell<_>` — so it is `!Send`/`!Sync` and CANNOT live in `callback_resources` (eframe requires `Send + Sync` there; that is why `VirtualTexture` wraps its tile receiver in a `Mutex`). Therefore: the producer lives in `ViewerState` (owned, single-threaded, no `Send+Sync` bound); the `TileProducer` trait takes `&mut self` and is **not** `Send`/`Sync`; and `VirtualTexture::produce_view` receives a `&mut dyn TileProducer` **per call** rather than storing it. The VT keeps only a `u64` version + a `bool` produce-mode flag (both trivially `Send+Sync`), so it stays `Sync`.
- **Color space is display-linear** (spec §4.3) — unchanged from Plans 1–2.
- **Geometry at the head for the tiled full-res path.** The canonical OpStack order is color-chain-then-geometry (geometry last) on the preview tier (Plan 2). For the full-res tiled tier, geometry is applied **at the head** of the per-tile pass: each output-tile pixel maps back to a source sample via the geometry transform, then the color chain (incl. Sharpen) runs in output/tile space (spec §8.4 "rotate applied as a sampling transform at the head of the per-tile pass"). For **identity geometry** the head is a 1:1 haloed copy, so the full-res result is identical to Plan 2's preview chain and to a whole-image render — this is what the tile-seam golden asserts. The small preview-vs-full discrepancy for *non-identity* geometry+sharpen (sharpen in output vs source space) is accepted (image quality secondary, architecture map §2; documented in the `TileEditPipeline` doc-comment).
- **Halo width = sharpen halo.** Because geometry is applied by direct per-output-pixel source sampling at the head (the geometry node samples the source for the full haloed region, edge-clamped), the only neighborhood op needing over-fetch is Sharpen. `H = ferrolite_pipeline::sharpen_halo(stack.sharpen())`. Point-only / identity-sharpen stacks request `H = 0` (spec §5.1).
- **Goldens auto-skip headless.** Every GPU golden begins `let Some(ctx) = GpuContext::headless() else { eprintln!(...); return; };`. Fixtures are authored on the dev GPU (RTX 3060/3070 class) on first run (auto-written when absent, per each crate's `tests/common`) and committed; the author validates them in the hands-on test (spec §10).
- **Gate (necessary, not sufficient):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → then **STOP and hold for the author's (Jann's) visual test** (open an image, zoom to 1:1 → sharp, seam-free; OpStack identity in Plan 3) before finishing the branch (CLAUDE.md "Finishing a branch").
- **Tolerances:** `ferrolite-vt`/`ferrolite-pipeline` `tests/common` use `TOL = 4` (u8, post-sRGB). The tile-seam golden compares the stitched producer output to the whole-image render; allow a slightly looser `SEAM_TOL = 6` to absorb f16 + an extra resample at the geometry head (still proves seam correctness — a broken halo drifts by tens of levels at the seam, not 6).

---

## File Structure

**Modified (engine tier):**
- `ferrolite-image/src/tile.rs` — add `haloed_tile_extent(halo)` + `haloed_tile_origin(coord, halo)` pure helpers + tests.
- `ferrolite-image/src/lib.rs` — export the two new helpers.
- `ferrolite-vt/src/source.rs` — add `TileSource::tile_with_halo`; make `tile` a provided default = `tile_with_halo(_, 0)`; implement `tile_with_halo` for `PyramidTileSource`.
- `ferrolite-vt/src/pool.rs` — add `TilePool::copy_into(ctx, slot, src_texture)` (GPU→GPU); add `STORAGE`? No — `COPY_DST` already present; the producer tile texture carries `COPY_SRC`.
- `ferrolite-vt/src/residency.rs` — add the pure `VersionedResidency` struct + tests.
- `ferrolite-vt/src/producer.rs` (**new**) — the `TileProducer` trait (engine-tier, photo-agnostic; `&mut self`, NOT `Send`/`Sync`).
- `ferrolite-vt/src/view.rs` — `SparseResources` gains a `VersionedResidency`, a `producing: bool` flag, and a stashed `last_needed`; new `set_opstack_version`/`set_producing`/`produce_view`/`needed_now` methods; `request_view_feedback` skips CPU-job submission while `producing`. The producer object is **not** stored — it is passed to `produce_view` per call. Spec-1 (non-producing) path unchanged.
- `ferrolite-vt/src/lib.rs` — export `TileProducer`, `VersionedResidency`.
- `ferrolite-pipeline/src/uniforms.rs` — add `out_origin` field to `GeometryUniform`; `geometry_uniform` sets it to `[0,0]`; `geometry_tile_uniform(...)` builds the per-tile uniform; tests.
- `ferrolite-pipeline/src/shaders/geometry.wgsl` — add `out_origin` to `struct P` and use it.
- `ferrolite-pipeline/src/nodes.rs` — add `GeometryHeadNode` (root node sampling a `GpuPyramidSource` LOD with a per-tile uniform).
- `ferrolite-pipeline/src/lib.rs` — export `GpuPyramidSource`, `TileEditPipeline`.
- `ferrolite-pipeline/tests/golden.rs` — add the tile-seam golden.

**Created (photo tier):**
- `ferrolite-pipeline/src/gpu_pyramid.rs` — `GpuPyramidSource`: the source LODs uploaded as GPU textures once.
- `ferrolite-pipeline/src/tile_edit.rs` — `TileEditPipeline`: geometry-head → color chain → interior 256².
- `ferrolite-app/src/viewer/edit_producer.rs` — `EditTileProducer` impl `ferrolite_vt::TileProducer`.

**Modified (app):**
- `ferrolite-app/Cargo.toml` — add `ferrolite-pipeline` dependency.
- `ferrolite-app/src/viewer/mod.rs` — `ViewerState` gains `op_stack: OpStack`.
- `ferrolite-app/src/viewer/mod.rs` — `ViewerState` also gains `edit_producer: Option<EditTileProducer>` (built on full-decode when `!op_stack.is_identity()`; `None` in Plan 3 since no UI mutates the stack). `EditTileProducer` is `!Send`/`!Sync` and lives here, NOT in `callback_resources`.
- `ferrolite-app/src/app.rs` — on full-decode, build the `GpuPyramidSource` + `TileEditPipeline` → `EditTileProducer` into `ViewerState`; in `drive_viewer`, when the viewer has an `edit_producer`, drive `produce_view` (bounded) passing it per call; otherwise the unchanged Spec-1 CPU path.

---

## Task 1: `ferrolite-image` halo origin/extent math (pure)

**Files:**
- Modify: `ferrolite-image/src/tile.rs`
- Modify: `ferrolite-image/src/lib.rs`

**Interfaces:**
- Consumes: existing `TileCoord`, `TILE_SIZE`, `tile_pixel_origin`.
- Produces:
  - `pub fn haloed_tile_extent(halo: u32) -> u32` — `TILE_SIZE + 2*halo`.
  - `pub fn haloed_tile_origin(coord: TileCoord, halo: u32) -> (i64, i64)` — the (possibly negative) top-left pixel of the haloed region = `tile_pixel_origin(coord)` minus `halo` on each axis.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `ferrolite-image/src/tile.rs`:

```rust
    #[test]
    fn haloed_extent_is_tile_plus_two_halos() {
        assert_eq!(haloed_tile_extent(0), TILE_SIZE);
        assert_eq!(haloed_tile_extent(3), TILE_SIZE + 6);
    }

    #[test]
    fn haloed_origin_subtracts_halo_and_can_go_negative() {
        // Tile (0,0) with halo 4 starts at (-4, -4).
        assert_eq!(haloed_tile_origin(TileCoord { lod: 0, x: 0, y: 0 }, 4), (-4, -4));
        // Tile (1,2) at lod 0 starts at (256, 512); halo 2 -> (254, 510).
        assert_eq!(
            haloed_tile_origin(TileCoord { lod: 0, x: 1, y: 2 }, 2),
            (254, 510)
        );
        // halo 0 == tile_pixel_origin.
        let o = tile_pixel_origin(TileCoord { lod: 0, x: 3, y: 1 });
        assert_eq!(
            haloed_tile_origin(TileCoord { lod: 0, x: 3, y: 1 }, 0),
            (o.0 as i64, o.1 as i64)
        );
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-image --lib tile::tests::haloed`
Expected: FAIL — `cannot find function haloed_tile_extent` / `haloed_tile_origin`.

- [ ] **Step 3: Implement the helpers**

In `ferrolite-image/src/tile.rs`, after `tile_pixel_origin`:

```rust
/// Edge length of the haloed tile region: `TILE_SIZE + 2*halo`. A producer that
/// over-fetches `halo` pixels on every side reads/writes a buffer this wide.
pub fn haloed_tile_extent(halo: u32) -> u32 {
    TILE_SIZE + 2 * halo
}

/// Top-left pixel of the haloed region for `coord` within its LOD level. The
/// interior tile origin minus `halo` on each axis; can be negative where the
/// halo overhangs the level's top/left edge (the consumer edge-clamps on read).
pub fn haloed_tile_origin(coord: TileCoord, halo: u32) -> (i64, i64) {
    let (ox, oy) = tile_pixel_origin(coord);
    (ox as i64 - halo as i64, oy as i64 - halo as i64)
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ferrolite-image --lib tile::tests::haloed`
Expected: PASS.

- [ ] **Step 5: Export the helpers**

In `ferrolite-image/src/lib.rs`, extend the `pub use tile::{...}` list to add `haloed_tile_extent, haloed_tile_origin`:

```rust
pub use tile::{
    haloed_tile_extent, haloed_tile_origin, level_size, pyramid_level_count, tile_pixel_origin,
    tiles_per_level, TileCoord, TILE_SIZE,
};
```

- [ ] **Step 6: Commit**

```bash
git add ferrolite-image/src/tile.rs ferrolite-image/src/lib.rs
git commit -m "feat(image): haloed tile origin/extent math"
```

---

## Task 2: `TileSource::tile_with_halo` + `PyramidTileSource` impl

**Files:**
- Modify: `ferrolite-vt/src/source.rs`

**Interfaces:**
- Consumes: `haloed_tile_extent`, `haloed_tile_origin` (Task 1); existing `LinearRgbaF32`, `TileCoord`, `TILE_SIZE`.
- Produces:
  - `TileSource::tile_with_halo(&self, coord: TileCoord, halo: u32) -> LinearRgbaF32` — a `haloed_tile_extent(halo)`² tile, edge-clamped where it overhangs the level.
  - `TileSource::tile(&self, coord)` becomes a **provided default** = `self.tile_with_halo(coord, 0)`.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `ferrolite-vt/src/source.rs`:

```rust
    #[test]
    fn tile_with_halo_zero_equals_tile() {
        let src = PyramidTileSource::new(solid(512, 512, [0.2, 0.4, 0.6]));
        let c = TileCoord { lod: 0, x: 1, y: 1 };
        assert_eq!(src.tile_with_halo(c, 0).pixels, src.tile(c).pixels);
    }

    #[test]
    fn tile_with_halo_is_haloed_extent_squared() {
        let src = PyramidTileSource::new(solid(512, 512, [0.2, 0.4, 0.6]));
        let t = src.tile_with_halo(TileCoord { lod: 0, x: 0, y: 0 }, 4);
        let ext = ferrolite_image::haloed_tile_extent(4);
        assert_eq!((t.width, t.height), (ext, ext));
    }

    #[test]
    fn tile_with_halo_edge_clamps_overhang() {
        // Tile (0,0) with halo 2 overhangs top-left by 2px; those clamp to (0,0).
        let mut px = Vec::new();
        for y in 0..8u32 {
            for x in 0..8u32 {
                // distinct per-pixel so clamping is observable: r = x/8, g = y/8.
                px.extend_from_slice(&[x as f32 / 8.0, y as f32 / 8.0, 0.0, 1.0]);
            }
        }
        let src = PyramidTileSource::new(LinearRgbaF32::new(8, 8, px).unwrap());
        let t = src.tile_with_halo(TileCoord { lod: 0, x: 0, y: 0 }, 2);
        // Top-left haloed pixel maps to source (-2,-2) -> clamps to (0,0) = (0,0).
        assert_eq!(&t.pixels[0..2], &[0.0, 0.0]);
        // The pixel at haloed (2,2) is source (0,0) too (origin); (3,3) is source (1,1).
        let ext = ferrolite_image::haloed_tile_extent(2) as usize;
        let at = |x: usize, y: usize| {
            let i = (y * ext + x) * 4;
            (t.pixels[i], t.pixels[i + 1])
        };
        assert_eq!(at(2, 2), (0.0, 0.0));
        assert_eq!(at(3, 3), (1.0 / 8.0, 1.0 / 8.0));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-vt --lib source::tests::tile_with_halo`
Expected: FAIL — `no method named tile_with_halo`.

- [ ] **Step 3: Add `tile_with_halo` to the trait with `tile` as a default**

In `ferrolite-vt/src/source.rs`, replace the `pub trait TileSource { ... }` block with:

```rust
pub trait TileSource {
    fn level_count(&self) -> u32;
    fn level_size(&self, lod: u32) -> (u32, u32);
    /// A `haloed_tile_extent(halo)`² tile centered on `coord`'s interior, edge-
    /// clamped where it overhangs the level. `halo = 0` yields the plain
    /// `TILE_SIZE`² interior tile.
    fn tile_with_halo(&self, coord: TileCoord, halo: u32) -> LinearRgbaF32;
    /// A `TILE_SIZE`² tile, edge-clamped where it overhangs the level. Defined as
    /// the no-halo haloed tile so Spec-1 display paths are unchanged.
    fn tile(&self, coord: TileCoord) -> LinearRgbaF32 {
        self.tile_with_halo(coord, 0)
    }
}
```

- [ ] **Step 4: Implement `tile_with_halo` for `PyramidTileSource` (replace its `tile`)**

In the `impl TileSource for PyramidTileSource` block, **replace** the existing `fn tile(&self, coord)` method with a `tile_with_halo` implementation (keep `level_count`/`level_size` as-is). Update the `use` line to import the new helpers:

```rust
use ferrolite_image::{
    haloed_tile_extent, haloed_tile_origin, level_size as img_level_size, pyramid_level_count,
    LinearRgbaF32, TileCoord,
};
```

Then in the impl:

```rust
    fn tile_with_halo(&self, coord: TileCoord, halo: u32) -> LinearRgbaF32 {
        let level = &self.levels[coord.lod as usize];
        let (ox, oy) = haloed_tile_origin(coord, halo);
        let ext = haloed_tile_extent(halo);
        let mut px = Vec::with_capacity((ext * ext * 4) as usize);
        for ty in 0..ext {
            for tx in 0..ext {
                // Signed source coordinate, edge-clamped into [0, dim-1].
                let sx = (ox + tx as i64).clamp(0, level.width as i64 - 1) as u32;
                let sy = (oy + ty as i64).clamp(0, level.height as i64 - 1) as u32;
                let i = ((sy * level.width + sx) * 4) as usize;
                px.extend_from_slice(&level.pixels[i..i + 4]);
            }
        }
        LinearRgbaF32::new(ext, ext, px).expect("haloed tile length")
    }
```

Note: the old `tile` used `tile_pixel_origin` + `TILE_SIZE` imports — remove `tile_pixel_origin` and `TILE_SIZE` from the top `use` if now unused (clippy `-D warnings` will flag unused imports). The test module re-imports `TILE_SIZE` itself, so leave that.

- [ ] **Step 5: Run the source tests**

Run: `cargo test -p ferrolite-vt --lib source::`
Expected: PASS (the three new tests + the existing `tile_*` tests, which now route through the default `tile` → `tile_with_halo(_, 0)`).

- [ ] **Step 6: Confirm Spec-1 display goldens unaffected**

Run: `cargo test -p ferrolite-vt`
Expected: PASS on a GPU host (rung1..4 goldens unchanged — `tile()` is byte-identical to before) and SKIP on headless.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-vt/src/source.rs
git commit -m "feat(vt): TileSource::tile_with_halo (tile = halo 0)"
```

---

## Task 3: `TileProducer` seam, `TilePool` GPU copy, pure `VersionedResidency`

**Files:**
- Create: `ferrolite-vt/src/producer.rs`
- Modify: `ferrolite-vt/src/pool.rs`
- Modify: `ferrolite-vt/src/residency.rs`
- Modify: `ferrolite-vt/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub trait TileProducer { fn produce(&mut self, ctx: &GpuContext, coord: TileCoord) -> wgpu::Texture; }` — returns a `TILE_SIZE`² `Rgba16Float` texture with `COPY_SRC` usage holding the produced (edited) interior. Photo-agnostic. **Not** `Send`/`Sync` (the impl wraps `!Send` pipeline state); the VT receives `&mut dyn TileProducer` per call and never stores it.
  - `TilePool::copy_into(&self, ctx: &GpuContext, slot: u32, src: &wgpu::Texture)` — GPU→GPU copy of a `TILE_SIZE`² texture into array layer `slot`.
  - `pub struct VersionedResidency` with `new()`, `set_version(v) -> Vec<TileCoord>` (bump → coords to invalidate), `mark(t)`, `is_current(t) -> bool`, `forget(t)`, `to_produce(needed) -> Vec<TileCoord>`, `current() -> u64`.

- [ ] **Step 1: Write the failing `VersionedResidency` tests**

Append to the `#[cfg(test)] mod tests` block in `ferrolite-vt/src/residency.rs`:

```rust
    #[test]
    fn version_bump_invalidates_stale_tiles_only() {
        let mut vr = VersionedResidency::new();
        vr.mark(tc(0, 0, 0));
        vr.mark(tc(0, 1, 0));
        assert!(vr.is_current(tc(0, 0, 0)));
        // Bump: every previously-marked tile is now stale and returned to invalidate.
        let stale = vr.set_version(1);
        assert_eq!(stale.len(), 2);
        assert!(!vr.is_current(tc(0, 0, 0)));
        // A no-op bump to the same version invalidates nothing.
        vr.mark(tc(0, 0, 0));
        assert!(vr.set_version(1).is_empty());
    }

    #[test]
    fn to_produce_is_needed_minus_current_resident() {
        let mut vr = VersionedResidency::new();
        vr.mark(tc(0, 0, 0)); // resident at current version
        let needed = vec![tc(0, 0, 0), tc(0, 1, 0)];
        // (0,0) is current; only (1,0) must be produced.
        assert_eq!(vr.to_produce(&needed), vec![tc(0, 1, 0)]);
        // After a version bump, (0,0) is stale -> both must be (re)produced.
        vr.set_version(2);
        assert_eq!(vr.to_produce(&needed), vec![tc(0, 0, 0), tc(0, 1, 0)]);
    }

    #[test]
    fn forget_drops_a_tile() {
        let mut vr = VersionedResidency::new();
        vr.mark(tc(0, 0, 0));
        vr.forget(tc(0, 0, 0));
        assert!(!vr.is_current(tc(0, 0, 0)));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-vt --lib residency::tests::version`
Expected: FAIL — `cannot find type VersionedResidency`.

- [ ] **Step 3: Implement `VersionedResidency`**

Add to `ferrolite-vt/src/residency.rs` (after `ResidencySet`). It needs `HashMap`:

```rust
use std::collections::HashMap;

/// Tracks the opstack version each *produced* (edited) tile was rendered at.
/// Pure bookkeeping — no GPU. An edit bumps the version; tiles produced at an
/// older version are stale and must be re-produced lazily for the current view.
pub struct VersionedResidency {
    current: u64,
    /// coord -> the version it was last produced at.
    at: HashMap<TileCoord, u64>,
}

impl VersionedResidency {
    pub fn new() -> Self {
        Self {
            current: 0,
            at: HashMap::new(),
        }
    }

    pub fn current(&self) -> u64 {
        self.current
    }

    /// Set the active version. If it changed, returns every coord whose produced
    /// version is now stale (≠ the new version) so the caller can free those slots.
    pub fn set_version(&mut self, v: u64) -> Vec<TileCoord> {
        if v == self.current {
            return Vec::new();
        }
        self.current = v;
        let stale: Vec<TileCoord> = self
            .at
            .iter()
            .filter(|&(_, &ver)| ver != v)
            .map(|(&c, _)| c)
            .collect();
        for c in &stale {
            self.at.remove(c);
        }
        stale
    }

    /// Record that `t` was produced at the current version (resident + fresh).
    pub fn mark(&mut self, t: TileCoord) {
        self.at.insert(t, self.current);
    }

    /// Is `t` resident AND produced at the current version?
    pub fn is_current(&self, t: TileCoord) -> bool {
        self.at.get(&t) == Some(&self.current)
    }

    /// Drop `t` entirely (slot freed).
    pub fn forget(&mut self, t: TileCoord) {
        self.at.remove(&t);
    }

    /// Of `needed`, those not resident at the current version (must (re)produce),
    /// preserving the needed order (visibility priority).
    pub fn to_produce(&self, needed: &[TileCoord]) -> Vec<TileCoord> {
        needed.iter().copied().filter(|t| !self.is_current(*t)).collect()
    }
}

impl Default for VersionedResidency {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ferrolite-vt --lib residency::`
Expected: PASS (the three new tests + existing residency tests).

- [ ] **Step 5: Add `TilePool::copy_into`**

In `ferrolite-vt/src/pool.rs`, add a method to `impl TilePool` (after `upload`):

```rust
    /// GPU→GPU copy: copy a `TILE_SIZE`² `Rgba16Float` texture (the producer's
    /// rendered tile, `COPY_SRC`) into physical `slot` (array layer). No CPU
    /// readback. The source must be exactly `TILE_SIZE`² and `Rgba16Float`.
    pub fn copy_into(&self, ctx: &GpuContext, slot: u32, src: &wgpu::Texture) {
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vt-pool-copy-into"),
            });
        enc.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: src,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: slot },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        ctx.queue.submit([enc.finish()]);
    }
```

(`TilePool`'s texture already has `COPY_DST`; no descriptor change needed.)

- [ ] **Step 6: Create the `TileProducer` trait**

Create `ferrolite-vt/src/producer.rs`:

```rust
//! GPU tile-producer seam (cross-cutting contract §5). The VT can fill a pool
//! slot by asking a producer to RENDER the tile on the GPU — no CPU readback —
//! instead of uploading CPU pixels. The trait is photo-agnostic: it knows only a
//! `GpuContext` and a `TileCoord`, and returns a `TILE_SIZE`² `Rgba16Float`
//! texture (`COPY_SRC`) that the VT copies into the slot. The photo edit producer
//! lives in `ferrolite-app`/`ferrolite-pipeline`, never here.
//!
//! The trait is intentionally NOT `Send`/`Sync`: the edit producer wraps a
//! `TileEditPipeline` that holds `Rc`/`RefCell` (it reuses the Plan 1/2 nodes).
//! It lives in `ViewerState` (single-threaded app state) and is handed to the VT
//! as `&mut dyn TileProducer` per `produce_view` call — never stored in the VT,
//! which must stay `Sync` to live in eframe's `callback_resources`.

use ferrolite_gpu::GpuContext;
use ferrolite_image::TileCoord;

pub trait TileProducer {
    /// Render the `TILE_SIZE`² interior for `coord` into a fresh `Rgba16Float`
    /// texture with `COPY_SRC` usage. Runs on the render thread (GPU work).
    fn produce(&mut self, ctx: &GpuContext, coord: TileCoord) -> wgpu::Texture;
}
```

- [ ] **Step 7: Wire the module + exports**

In `ferrolite-vt/src/lib.rs`, add `mod producer;` (after `mod pool;`) and extend the `pub use` block:

```rust
pub use producer::TileProducer;
pub use residency::{needed_tiles, ResidencySet, VersionedResidency};
```

- [ ] **Step 8: Run the crate (compile + unit) and confirm green**

Run: `cargo test -p ferrolite-vt --lib`
Expected: PASS. (No GPU needed for the unit tests; `copy_into`/`TileProducer` are compiled but exercised by the golden + app later.)

- [ ] **Step 9: Commit**

```bash
git add ferrolite-vt/src/producer.rs ferrolite-vt/src/pool.rs \
  ferrolite-vt/src/residency.rs ferrolite-vt/src/lib.rs
git commit -m "feat(vt): TileProducer seam + pool GPU copy + versioned residency"
```

---

## Task 4: Producer-backed sparse fill path (bounded, render-thread, version-invalidated)

**Files:**
- Modify: `ferrolite-vt/src/view.rs`

**Interfaces:**
- Consumes: `TileProducer` (Task 3), `VersionedResidency` (Task 3), `TilePool::copy_into` (Task 3); the existing `SparseResources`/`SlotAllocator`/page-table machinery.
- Produces (on `VirtualTexture`):
  - `pub fn set_producing(&mut self, on: bool)` — mark the sparse VT as producer-driven. While `on`, `request_view_feedback` does residency/eviction/feedback bookkeeping but **does not** submit CPU load jobs (the producer fills tiles instead).
  - `pub fn set_opstack_version(&mut self, version: u64)` — bump on edit; invalidates stale produced tiles (frees their slots + clears the page table) so they re-produce lazily.
  - `pub fn needed_now(&self) -> Vec<TileCoord>` — the needed set from the most recent `request_view_feedback` (GPU-truth), so the app knows which tiles to produce.
  - `pub fn produce_view(&mut self, ctx: &GpuContext, producer: &mut dyn TileProducer, needed: &[TileCoord], budget: usize) -> usize` — render up to `budget` not-current tiles from `needed` (in order) via the **passed** producer, copy each into its slot, update residency/page table; returns the count produced. The producer is borrowed per call, never stored (it is `!Send`/`!Sync`).
- Behavior contract: when not producing (`set_producing(false)`, the default), every existing rung-3/rung-4 method is byte-identical to Spec 1.

**Design note (in the method doc-comments):** `produce_view` is the render-thread, bounded counterpart of the CPU `request_view_feedback` job submission. The caller passes the needed set (from `needed_now()` after a `request_view_feedback` reconcile). Cancellation/priority = re-derive `needed` each frame and only produce `to_produce(needed)` up to the budget; superseded coords are never produced. `set_opstack_version` is the edit invalidation. GPU produce cannot run on a `ferrolite-jobs` worker (CLAUDE.md), so it is bounded here instead. The producer is **not stored** on the VT (it is `!Send`/`!Sync`); it is borrowed per call from `ViewerState`.

- [ ] **Step 1: Add version + producing fields to `SparseResources`**

In `ferrolite-vt/src/view.rs`, extend the `use` block to bring in the new type:

```rust
use crate::residency::{needed_tiles, ResidencySet, VersionedResidency};
use crate::producer::TileProducer;
```

Add fields to `struct SparseResources` (after `slots: Vec<u32>`):

```rust
    // Plan 3: edited-tile version tracking + producer-drive bookkeeping. The
    // producer object itself is NOT stored here (it is !Send/!Sync); it is passed
    // to `produce_view` per call. `producing` suppresses CPU job submission in
    // `request_view_feedback` while the producer fills tiles instead.
    versions: VersionedResidency,
    producing: bool,
    last_needed: Vec<TileCoord>,
```

In `VirtualTexture::sparse`, initialize them in the `SparseResources { ... }` literal:

```rust
            versions: VersionedResidency::new(),
            producing: false,
            last_needed: Vec::new(),
```

- [ ] **Step 1b: Stash `last_needed` and skip CPU loads while producing in `request_view_feedback`**

In `request_view_feedback`, immediately after `let needed = s.feedback.read_back(ctx, &s.layout);`, add `s.last_needed = needed.clone();`. Then wrap the existing "Submit loads for newly-needed tiles" `for t in to_load { ... }` block in `if !s.producing { ... }` so producer-driven VTs don't also spawn CPU jobs for the same tiles. (Eviction, stale-cancel, page-table update, and feedback clear stay unconditional.)

- [ ] **Step 2: Write a headless-skipping integration test for the produce path**

Producer correctness is proven end-to-end by the tile-seam golden (Task 8). Here add a smaller VT-level GPU test that a trivial **copy producer** (no photo) fills the requested tiles and that a version bump invalidates them. Append to `ferrolite-vt/tests/golden.rs`:

```rust
/// A trivial GPU producer for testing the VT produce path without any photo
/// dependency: uploads a solid-color `TILE_SIZE`² `Rgba16Float` tile whose color
/// encodes the coord, returning a `COPY_SRC` texture.
struct SolidProducer;
impl ferrolite_vt::TileProducer for SolidProducer {
    fn produce(&mut self, ctx: &ferrolite_gpu::GpuContext, coord: ferrolite_image::TileCoord) -> wgpu::Texture {
        use wgpu::util::DeviceExt;
        let n = (TILE_SIZE * TILE_SIZE) as usize;
        let r = half::f16::from_f32((coord.x as f32 + 1.0) / 16.0);
        let g = half::f16::from_f32((coord.y as f32 + 1.0) / 16.0);
        let b = half::f16::from_f32(0.5);
        let a = half::f16::from_f32(1.0);
        let mut texels = Vec::with_capacity(n * 4);
        for _ in 0..n {
            texels.extend_from_slice(&[r, g, b, a]);
        }
        ctx.device.create_texture_with_data(
            &ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("solid-producer-tile"),
                size: wgpu::Extent3d { width: TILE_SIZE, height: TILE_SIZE, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&texels),
        )
    }
}

#[test]
fn producer_fills_requested_tiles_and_version_bump_invalidates() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let (iw, ih) = (600u32, 500u32);
    let img = ferrolite_image::LinearRgbaF32::black(iw, ih);
    let src: Arc<dyn TileSource + Send + Sync> = Arc::new(PyramidTileSource::new(img));
    let total: u32 = (0..src.level_count())
        .map(|lod| {
            let (lw, lh) = src.level_size(lod);
            lw.div_ceil(TILE_SIZE) * lh.div_ceil(TILE_SIZE)
        })
        .sum();
    let jobs = Arc::new(JobSystem::new(1));
    let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    let mut vt = VirtualTexture::sparse(&ctx, Arc::clone(&src), Arc::clone(&jobs), total, &pipelines);
    let mut producer = SolidProducer;

    let needed = vec![TileCoord { lod: 0, x: 0, y: 0 }, TileCoord { lod: 0, x: 1, y: 0 }];
    let made = vt.produce_view(&ctx, &mut producer, &needed, 8);
    assert_eq!(made, 2, "both needed tiles produced");
    assert!(vt.is_resident(needed[0]) && vt.is_resident(needed[1]));

    // Re-producing the same view with no version change produces nothing more.
    assert_eq!(vt.produce_view(&ctx, &mut producer, &needed, 8), 0, "already current");

    // A version bump invalidates them; they must re-produce.
    vt.set_opstack_version(1);
    assert!(!vt.is_resident(needed[0]), "stale tile freed by version bump");
    assert_eq!(
        vt.produce_view(&ctx, &mut producer, &needed, 8),
        2,
        "re-produced at new version"
    );
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p ferrolite-vt --test golden producer_fills`
Expected: FAIL to COMPILE — `no method named produce_view`/`set_opstack_version`.

- [ ] **Step 4: Implement `set_producing` / `set_opstack_version` / `needed_now` / `produce_view`**

Add to `impl VirtualTexture` in `ferrolite-vt/src/view.rs` (near the other sparse methods). `produce_view` reuses the same eviction/page-table helpers (`flat_index`) the CPU path uses:

```rust
    /// Plan 3: mark this sparse VT producer-driven. While `on`, `request_view_
    /// feedback` keeps reconciling residency + clearing feedback but skips CPU
    /// load-job submission (the producer fills tiles instead). No-op if non-sparse.
    pub fn set_producing(&mut self, on: bool) {
        if let Some(s) = self.sparse.as_mut() {
            s.producing = on;
        }
    }

    /// Plan 3: the needed set from the most recent `request_view_feedback`
    /// (GPU-truth). The producer-drive loop produces these. Empty if non-sparse
    /// or before the first reconcile.
    pub fn needed_now(&self) -> Vec<TileCoord> {
        self.sparse
            .as_ref()
            .map(|s| s.last_needed.clone())
            .unwrap_or_default()
    }

    /// Plan 3: set the active opstack version. On change, free the slots of every
    /// resident tile produced at an older version, clear their CPU slot-mirror
    /// entries, AND flush the GPU page table so the shader never samples a
    /// freed/aliased slot for a frame. No-op if unchanged or non-sparse.
    pub fn set_opstack_version(&mut self, ctx: &GpuContext, version: u64) {
        let Some(s) = self.sparse.as_mut() else { return };
        let stale = s.versions.set_version(version);
        for t in &stale {
            s.allocator.free(*t);
            s.residency.forget(*t);
            if let Some(idx) = flat_index(&s.layout, *t) {
                s.slots[idx] = NOT_RESIDENT;
            }
        }
        if !stale.is_empty() {
            s.page_table.update(ctx, &s.slots);
        }
    }

    /// Plan 3: render up to `budget` not-current tiles from `needed` (in order)
    /// via the passed `producer`, copy each into its pool slot, update residency
    /// + page table. Returns the count produced. The producer is borrowed per call
    /// (it is !Send/!Sync, owned by `ViewerState`). Runs on the render thread (GPU
    /// work); bounded by `budget` per call. No-op (0) on a non-sparse VT.
    pub fn produce_view(
        &mut self,
        ctx: &GpuContext,
        producer: &mut dyn TileProducer,
        needed: &[TileCoord],
        budget: usize,
    ) -> usize {
        let Some(s) = self.sparse.as_mut() else { return 0 };

        let to_produce = s.versions.to_produce(needed);
        let mut produced = 0;
        for coord in to_produce.into_iter().take(budget) {
            // Allocate a slot, evicting an LRU resident if the pool is full.
            let slot = match s.allocator.alloc(coord) {
                Some(slot) => slot,
                None => {
                    if let Some(victim) = s.residency.lru() {
                        s.allocator.free(victim);
                        s.residency.forget(victim);
                        s.versions.forget(victim);
                        if let Some(idx) = flat_index(&s.layout, victim) {
                            s.slots[idx] = NOT_RESIDENT;
                        }
                    }
                    match s.allocator.alloc(coord) {
                        Some(slot) => slot,
                        None => continue, // capacity 0; nothing to do
                    }
                }
            };
            let tile_tex = producer.produce(ctx, coord);
            s.pool.copy_into(ctx, slot, &tile_tex);
            s.residency.touch(coord);
            s.versions.mark(coord);
            if let Some(idx) = flat_index(&s.layout, coord) {
                s.slots[idx] = slot;
            }
            produced += 1;
        }
        if produced > 0 {
            s.page_table.update(ctx, &s.slots);
        }
        produced
    }
```

- [ ] **Step 5: Run the VT test suite**

Run: `cargo test -p ferrolite-vt`
Expected: PASS on a GPU host (`producer_fills_*` + the existing rung1..4 goldens — all unchanged since no producer is set on those paths) and SKIP on headless. `cargo clippy -p ferrolite-vt --all-targets -- -D warnings` clean.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-vt/src/view.rs ferrolite-vt/tests/golden.rs
git commit -m "feat(vt): producer-backed bounded sparse fill + version invalidation"
```

---

## Task 5: Geometry `out_origin` — render a tile sub-region

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs`
- Modify: `ferrolite-pipeline/src/shaders/geometry.wgsl`

**Interfaces:**
- Consumes: existing `geometry_uniform`, `GeometryUniform`, `Geometry`/`CropRect`/`Aspect`.
- Produces:
  - `GeometryUniform` gains `pub out_origin: [f32; 2]` (replaces one of the two `pad` slots; the struct stays 16-byte aligned). `geometry_uniform(...)` sets `out_origin: [0.0, 0.0]` (whole-image path unchanged).
  - `pub fn geometry_tile_uniform(op: Option<Geometry>, src_w: u32, src_h: u32, out_origin: (f32, f32), ext: u32) -> GeometryUniform` — same `m`/`off`/`src_dims` as `geometry_uniform`, but `out_origin` set and `out_dims = [ext, ext]` (the haloed tile extent). Used by the per-tile geometry head.

**Design note:** The shader maps `po = out_origin + (gid + 0.5)`; `out_dims` is used only for the bounds check, so a tile pass sets it to the haloed extent. The whole-image path keeps `out_origin = [0,0]` and `out_dims = full`, so the existing `geometry_crop_rotate` golden is byte-identical.

- [ ] **Step 1: Write the failing tests**

Append to `#[cfg(test)] mod tests` in `ferrolite-pipeline/src/uniforms.rs`:

```rust
    #[test]
    fn geometry_uniform_default_out_origin_is_zero() {
        let (u, _, _) = geometry_uniform(None, 64, 48);
        assert_eq!(u.out_origin, [0.0, 0.0]);
    }

    #[test]
    fn geometry_tile_uniform_sets_origin_and_extent() {
        // Identity geometry, source 600x500, tile origin (254, -2), extent 260.
        let u = geometry_tile_uniform(None, 600, 500, (254.0, -2.0), 260);
        assert_eq!(u.out_origin, [254.0, -2.0]);
        assert_eq!(u.out_dims, [260.0, 260.0]);
        // Identity transform + source dims preserved.
        assert_eq!(u.m, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(u.src_dims, [600.0, 500.0]);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests::geometry`
Expected: FAIL — `no field out_origin` / `cannot find function geometry_tile_uniform`.

- [ ] **Step 3: Add `out_origin` to `GeometryUniform` and set it in `geometry_uniform`**

In `ferrolite-pipeline/src/uniforms.rs`, replace the `GeometryUniform` struct with (note: `out_origin` takes the old `pad`'s 8 bytes; the struct stays 16-byte aligned — `m`(16)+`off`(8)+`src_dims`(8)+`out_dims`(8)+`out_origin`(8) = 48 bytes, a multiple of 16):

```rust
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GeometryUniform {
    /// Row-major 2×2 mapping output-pixel → source-pixel: [m00, m01, m10, m11].
    pub m: [f32; 4],
    /// Source-pixel translation: src = m·out + off.
    pub off: [f32; 2],
    pub src_dims: [f32; 2],
    pub out_dims: [f32; 2],
    /// Output-pixel origin added to `gid` before the transform, so a tile pass can
    /// render a sub-region of the output image. Whole-image path uses `[0,0]`.
    pub out_origin: [f32; 2],
}
```

In `geometry_uniform`, replace the returned `GeometryUniform { ... pad: [0.0; 2] }` with `out_origin: [0.0, 0.0]`:

```rust
    (
        GeometryUniform {
            m,
            off,
            src_dims: [sw, sh],
            out_dims: [out_w as f32, out_h as f32],
            out_origin: [0.0, 0.0],
        },
        out_w,
        out_h,
    )
```

Then add the per-tile constructor after `geometry_uniform`:

```rust
/// A per-tile geometry-head uniform: identical `m`/`off`/`src_dims` to
/// `geometry_uniform` at the given source dims, but with the output origin set to
/// the haloed tile's top-left (may be negative) and `out_dims` set to the haloed
/// extent. Used by `TileEditPipeline`'s geometry head to resample the source for
/// one output tile (geometry applied at the head; spec §8.4).
pub fn geometry_tile_uniform(
    op: Option<Geometry>,
    src_w: u32,
    src_h: u32,
    out_origin: (f32, f32),
    ext: u32,
) -> GeometryUniform {
    let (base, _, _) = geometry_uniform(op, src_w, src_h);
    GeometryUniform {
        out_dims: [ext as f32, ext as f32],
        out_origin: [out_origin.0, out_origin.1],
        ..base
    }
}
```

- [ ] **Step 4: Update the geometry shader to use `out_origin`**

Replace `ferrolite-pipeline/src/shaders/geometry.wgsl` with (adds `out_origin` to `struct P` and the `po` computation; the rest is unchanged):

```wgsl
// Geometry: crop + rotate as a bilinear sampling transform. Output dims differ
// from input dims, so this is NOT a point op — it has its own bind layout
// (0 = src texture, 1 = dst storage, 2 = uniform, 3 = sampler). Uses
// textureSampleLevel (compute has no implicit derivatives). `out_origin` offsets
// the output pixel so a tile pass can render a haloed sub-region of the output.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P {
    m: vec4<f32>,         // row-major 2x2: m00,m01,m10,m11
    off: vec2<f32>,
    src_dims: vec2<f32>,
    out_dims: vec2<f32>,
    out_origin: vec2<f32>,
};
@group(0) @binding(2) var<uniform> p: P;
@group(0) @binding(3) var samp: sampler;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ow = u32(p.out_dims.x);
    let oh = u32(p.out_dims.y);
    if (gid.x >= ow || gid.y >= oh) { return; }
    let po = p.out_origin + vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5);
    let sx = p.m.x * po.x + p.m.y * po.y + p.off.x;
    let sy = p.m.z * po.x + p.m.w * po.y + p.off.y;
    let uv = vec2<f32>(sx, sy) / p.src_dims;
    let c = textureSampleLevel(src, samp, uv, 0.0);
    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), c);
}
```

- [ ] **Step 5: Run the pipeline tests (uniforms + existing geometry golden)**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS. On a GPU host the existing `geometry_crop_rotate_matches_golden` stays within tolerance (whole-image path uses `out_origin [0,0]`, so output is byte-identical). On headless, GPU tests skip.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/shaders/geometry.wgsl
git commit -m "feat(pipeline): geometry out_origin for tile sub-region rendering"
```

---

## Task 6: `GpuPyramidSource` — upload the source LODs to the GPU once

**Files:**
- Create: `ferrolite-pipeline/src/gpu_pyramid.rs`
- Modify: `ferrolite-pipeline/src/lib.rs`

**Interfaces:**
- Consumes: `GpuContext`, `LinearRgbaF32`, `PIPELINE_FORMAT`, `ferrolite_image::{pyramid_level_count, level_size}`; `half::f16`.
- Produces:
  - `pub struct GpuPyramidSource` holding one `Arc<wgpu::Texture>` per LOD (display-linear `Rgba16Float`, `TEXTURE_BINDING`), built once from a `&LinearRgbaF32` full image (box-downsample per LOD — reusing the same downsample math as `PyramidTileSource`).
  - `GpuPyramidSource::new(ctx: &GpuContext, full: &LinearRgbaF32) -> Self`
  - `GpuPyramidSource::level_count(&self) -> u32`
  - `GpuPyramidSource::level_size(&self, lod: u32) -> (u32, u32)`
  - `GpuPyramidSource::level(&self, lod: u32) -> PipelineImage` — the LOD texture wrapped as a `PipelineImage` (cheap Arc clone) for the geometry head's input.

**Design note:** This duplicates `PyramidTileSource`'s CPU box-downsample (which lives in `ferrolite-vt`, an engine crate `ferrolite-pipeline` does not depend on). The downsample is ~10 lines; copy it rather than add a `ferrolite-vt` dependency to the photo tier (keeps the tier boundary clean; DRY across a tier boundary is not worth a dependency). At ~6 MP (QuadBin) the whole pyramid is ≈64 MB of `Rgba16Float` — comfortably GPU-resident, uploaded once on full-decode (spec §5.2).

- [ ] **Step 1: Write a failing unit test (level math, no GPU)**

The texture upload needs a GPU; gate it. But the level **count/size** math is pure and worth a guarded check. Create `ferrolite-pipeline/src/gpu_pyramid.rs` with the test first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_gpu::GpuContext;
    use ferrolite_image::LinearRgbaF32;

    #[test]
    fn pyramid_levels_match_image_math() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let full = LinearRgbaF32::black(1024, 512);
        let p = GpuPyramidSource::new(&ctx, &full);
        assert_eq!(p.level_count(), ferrolite_image::pyramid_level_count(1024, 512));
        assert_eq!(p.level_size(0), (1024, 512));
        assert_eq!(p.level_size(1), (512, 256));
        // Each level wraps a same-size texture.
        let l1 = p.level(1);
        assert_eq!((l1.width, l1.height), (512, 256));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-pipeline --lib gpu_pyramid`
Expected: FAIL — module not declared / `cannot find GpuPyramidSource`.

- [ ] **Step 3: Implement `GpuPyramidSource`**

Prepend to `ferrolite-pipeline/src/gpu_pyramid.rs` (above the test module):

```rust
//! The edit source pyramid, uploaded to the GPU once on full-decode. Each LOD is
//! an `Rgba16Float` texture (display-linear); the per-tile edit producer samples
//! the LOD matching a requested tile's level. Built once, reused for every tile
//! and every edit (CLAUDE.md GPU rule; spec §5.2).

use std::sync::Arc;

use ferrolite_gpu::GpuContext;
use ferrolite_image::{level_size, pyramid_level_count, LinearRgbaF32};
use half::f16;
use wgpu::util::DeviceExt;

use crate::image::{PipelineImage, PIPELINE_FORMAT};

pub struct GpuPyramidSource {
    levels: Vec<PipelineImage>, // index = lod
}

impl GpuPyramidSource {
    pub fn new(ctx: &GpuContext, full: &LinearRgbaF32) -> Self {
        let count = pyramid_level_count(full.width, full.height);
        // Build CPU LODs by box-downsample (same math as PyramidTileSource), then
        // upload each as a texture. (Copied across the tier boundary on purpose —
        // ferrolite-pipeline must not depend on the engine-tier ferrolite-vt.)
        let mut cpu: Vec<LinearRgbaF32> = Vec::with_capacity(count as usize);
        cpu.push(full.clone());
        for lod in 1..count {
            let (w, h) = level_size(full.width, full.height, lod);
            cpu.push(box_downsample(&cpu[(lod - 1) as usize], w, h));
        }
        let levels = cpu.iter().map(|l| upload_level(ctx, l)).collect();
        Self { levels }
    }

    pub fn level_count(&self) -> u32 {
        self.levels.len() as u32
    }

    pub fn level_size(&self, lod: u32) -> (u32, u32) {
        let l = &self.levels[lod as usize];
        (l.width, l.height)
    }

    pub fn level(&self, lod: u32) -> PipelineImage {
        self.levels[lod as usize].clone()
    }
}

fn upload_level(ctx: &GpuContext, img: &LinearRgbaF32) -> PipelineImage {
    let texels: Vec<f16> = img.pixels.iter().map(|&v| f16::from_f32(v)).collect();
    let texture = ctx.device.create_texture_with_data(
        &ctx.queue,
        &wgpu::TextureDescriptor {
            label: Some("gpu-pyramid-level"),
            size: wgpu::Extent3d {
                width: img.width,
                height: img.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PIPELINE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(&texels),
    );
    PipelineImage {
        texture: Arc::new(texture),
        width: img.width,
        height: img.height,
    }
}

/// 2×2-average downsample to `(dst_w, dst_h)` (box filter; adequate for the edit
/// source pyramid). Mirrors `ferrolite_vt::source`'s box_downsample.
fn box_downsample(src: &LinearRgbaF32, dst_w: u32, dst_h: u32) -> LinearRgbaF32 {
    let mut px = vec![0.0f32; LinearRgbaF32::expected_len(dst_w, dst_h)];
    for dy in 0..dst_h {
        for dx in 0..dst_w {
            let sx0 = (dx * src.width / dst_w).min(src.width - 1);
            let sy0 = (dy * src.height / dst_h).min(src.height - 1);
            let sx1 = (sx0 + 1).min(src.width - 1);
            let sy1 = (sy0 + 1).min(src.height - 1);
            let mut acc = [0.0f32; 4];
            for &(x, y) in &[(sx0, sy0), (sx1, sy0), (sx0, sy1), (sx1, sy1)] {
                let i = ((y * src.width + x) * 4) as usize;
                for (c, acc_c) in acc.iter_mut().enumerate() {
                    *acc_c += src.pixels[i + c];
                }
            }
            let di = ((dy * dst_w + dx) * 4) as usize;
            for (c, acc_c) in acc.iter().enumerate() {
                px[di + c] = acc_c * 0.25;
            }
        }
    }
    LinearRgbaF32::new(dst_w, dst_h, px).expect("downsample length")
}
```

- [ ] **Step 4: Declare the module + export**

In `ferrolite-pipeline/src/lib.rs`, add `mod gpu_pyramid;` (after `mod image;`) and `pub use gpu_pyramid::GpuPyramidSource;`.

- [ ] **Step 5: Run the test**

Run: `cargo test -p ferrolite-pipeline --lib gpu_pyramid`
Expected: PASS on a GPU host, SKIP on headless.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-pipeline/src/gpu_pyramid.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): GpuPyramidSource (edit source LODs on the GPU)"
```

---

## Task 7: `GeometryHeadNode` + `TileEditPipeline` (per-tile producer)

**Files:**
- Modify: `ferrolite-pipeline/src/nodes.rs`
- Create: `ferrolite-pipeline/src/tile_edit.rs`
- Modify: `ferrolite-pipeline/src/lib.rs`

**Interfaces:**
- Consumes: `GpuPyramidSource` (Task 6), `geometry_tile_uniform`/`GeometryUniform` (Task 5), `sharpen_halo`, the existing `PointOpNode`/`CurveNode` + all `*_uniform` helpers, `Graph`/`NodeId`, `haloed_tile_extent`, `TileCoord`/`tile_pixel_origin`.
- Produces:
  - `pub(crate) struct GeometryHeadNode` — a root `Node<PipelineImage>` that, on each evaluate, picks its `GpuPyramidSource` LOD for the current `TileCoord`, builds a per-tile `geometry_tile_uniform`, dispatches the geometry pass into a `(ext×ext)` haloed buffer, and returns it. Driven by an `Rc<Cell<TileRequest>>` (coord) updated per produce.
  - `pub struct TileEditPipeline` with:
    - `new(ctx: Arc<GpuContext>, source: Arc<GpuPyramidSource>, stack: OpStack) -> Self`
    - `produce_tile(&mut self, coord: TileCoord) -> PipelineImage` — renders the edited interior 256² as an `Rgba16Float` `COPY_SRC` texture.
    - `halo(&self) -> u32` (the active sharpen halo; the haloed extent is `TILE_SIZE + 2*halo`).
    - `set_stack(&mut self, stack: OpStack)` — re-point op params (Plan 4 uses this; here it is exercised once at construction).

**Design note (in `TileEditPipeline`'s doc-comment):** Geometry is applied **at the head** (the `GeometryHeadNode` resamples the source for the haloed output tile via the geometry transform), then the color chain (exposure→WB→contrast→curve→HSL→sharpen) runs in tile/output space. For identity geometry this equals the whole-image Plan-2 chain exactly; for non-identity geometry, Sharpen operates in output rather than source space — an accepted pragmatic difference (architecture map §2; spec §8.4). The interior 256² is extracted at offset `halo` and returned.

- [ ] **Step 1: Add `GeometryHeadNode` to `nodes.rs`**

Extend the top `use` in `ferrolite-pipeline/src/nodes.rs` to add the pieces the head needs:

```rust
use ferrolite_image::{haloed_tile_extent, tile_pixel_origin, LinearRgbaF32, TileCoord};
use crate::gpu_pyramid::GpuPyramidSource;
use crate::uniforms::{geometry_tile_uniform, GeometryUniform};
use crate::op::Geometry;
```

(Keep the existing imports; add only what is missing.) Add the node (after `GeometryNode`):

```rust
/// The current tile request driving the geometry head (coord + active halo).
#[derive(Clone, Copy)]
pub(crate) struct TileRequest {
    pub coord: TileCoord,
    pub halo: u32,
}

/// Root node for the per-tile edit pipeline: samples the `GpuPyramidSource` LOD
/// for the current `TileRequest` through the geometry transform (geometry at the
/// head), producing a `(ext×ext)` haloed, geometrically-resampled tile in output
/// space. The color chain runs downstream of it.
pub(crate) struct GeometryHeadNode {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    sampler: wgpu::Sampler,
    source: Arc<GpuPyramidSource>,
    geometry: Geometry,
    request: Rc<Cell<TileRequest>>,
    out: RefCell<Option<PipelineImage>>,
}

impl GeometryHeadNode {
    pub(crate) fn new(
        ctx: Arc<GpuContext>,
        source: Arc<GpuPyramidSource>,
        geometry: Geometry,
        request: Rc<Cell<TileRequest>>,
    ) -> Self {
        let bgl = geometry_bgl(&ctx.device); // reuse the geometry pass bind layout
        let module = ctx
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("geometry-head"),
                source: wgpu::ShaderSource::Wgsl(include_str!("shaders/geometry.wgsl").into()),
            });
        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("geometry-head"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            });
        let pipeline = ctx
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("geometry-head"),
                layout: Some(&layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            });
        let uniform_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("geometry-head-uniform"),
            size: std::mem::size_of::<GeometryUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = ctx.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("geometry-head-samp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            ctx,
            pipeline,
            bgl,
            uniform_buf,
            sampler,
            source,
            geometry,
            request,
            out: RefCell::new(None),
        }
    }

    fn ensure_out(&self, ext: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((ext, ext)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("geometry-head-out"),
                size: wgpu::Extent3d {
                    width: ext,
                    height: ext,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PIPELINE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            });
            *out = Some(PipelineImage {
                texture: Arc::new(tex),
                width: ext,
                height: ext,
            });
        }
        out.as_ref().unwrap().clone()
    }
}

impl Node<PipelineImage> for GeometryHeadNode {
    fn evaluate(&self, _inputs: &[&PipelineImage]) -> PipelineImage {
        let req = self.request.get();
        let lod = req.coord.lod;
        let src = self.source.level(lod);
        let (sw, sh) = self.source.level_size(lod);
        let ext = haloed_tile_extent(req.halo);
        let dst = self.ensure_out(ext);

        // Haloed output-tile origin at this LOD (interior origin minus halo).
        let (ox, oy) = tile_pixel_origin(req.coord);
        let out_origin = (ox as f32 - req.halo as f32, oy as f32 - req.halo as f32);
        let u = geometry_tile_uniform(Some(self.geometry), sw, sh, out_origin, ext);
        self.ctx
            .queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));

        let src_view = src
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("geometry-head-bind"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&dst_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.uniform_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("geometry-head-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(ext.div_ceil(8), ext.div_ceil(8), 1);
        }
        self.ctx.queue.submit([enc.finish()]);
        dst
    }
}
```

`geometry_bgl` is currently a private free fn in `nodes.rs` — it is already defined; reuse it. If it is `fn geometry_bgl` (module-private) it is accessible here (same module).

- [ ] **Step 2: Create `TileEditPipeline`**

Create `ferrolite-pipeline/src/tile_edit.rs`:

```rust
//! `TileEditPipeline` — the per-tile, full-res GPU edit producer. For each
//! requested tile it runs geometry-at-the-head (resampling the GPU-resident
//! source for the haloed output tile) then the color chain (exposure→WB→contrast
//! →tone-curve→HSL→sharpen) over the haloed buffer, and returns the interior
//! `TILE_SIZE`² as an `Rgba16Float` `COPY_SRC` texture for the VT to copy into a
//! pool slot. No CPU readback (spec §5.2).
//!
//! Geometry is applied at the head (spec §8.4). For identity geometry the head is
//! a 1:1 haloed copy, so the result is identical to the whole-image Plan-2 chain
//! and to a whole-image render — this is what the tile-seam golden asserts. For
//! non-identity geometry, Sharpen operates in output space rather than source
//! space, an accepted pragmatic difference (architecture map §2).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Graph, NodeId};
use ferrolite_image::{TileCoord, TILE_SIZE};

use crate::gpu_pyramid::GpuPyramidSource;
use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::nodes::{CurveNode, GeometryHeadNode, PointOpNode, TileRequest};
use crate::op::{Aspect, CropRect, Geometry, OpStack};
use crate::uniforms::{
    contrast_uniform, curve_lut, exposure_uniform, hsl_uniform, sharpen_halo, sharpen_uniform,
    ContrastUniform, ExposureUniform, HslUniform, SharpenUniform, WbUniform,
};

pub struct TileEditPipeline {
    ctx: Arc<GpuContext>,
    graph: Graph<PipelineImage>,
    output_id: NodeId,
    request: Rc<Cell<TileRequest>>,
    head_id: NodeId,
    halo: u32,
    // Param cells (set from the stack; Plan 4 mutates via set_stack).
    exposure: Rc<Cell<ExposureUniform>>,
    wb: Rc<Cell<WbUniform>>,
    contrast: Rc<Cell<ContrastUniform>>,
    tone_curve: Rc<Cell<[f32; 256]>>,
    hsl: Rc<Cell<HslUniform>>,
    sharpen: Rc<Cell<SharpenUniform>>,
}

impl TileEditPipeline {
    pub fn new(ctx: Arc<GpuContext>, source: Arc<GpuPyramidSource>, stack: OpStack) -> Self {
        let halo = sharpen_halo(stack.sharpen());
        let geometry = stack.geometry().unwrap_or(Geometry {
            crop: CropRect::full(),
            angle_deg: 0.0,
            aspect: Aspect::Original,
        });
        let request = Rc::new(Cell::new(TileRequest {
            coord: TileCoord { lod: 0, x: 0, y: 0 },
            halo,
        }));

        let mut graph = Graph::new();
        let head = GeometryHeadNode::new(ctx.clone(), source, geometry, request.clone());
        let head_id = graph.add_node(Box::new(head), vec![]);

        let exposure = Rc::new(Cell::new(exposure_uniform(stack.exposure())));
        let exposure_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/exposure.wgsl"),
                "exposure",
                exposure.clone(),
            )),
            vec![head_id],
        );
        let wb = Rc::new(Cell::new(crate::uniforms::wb_uniform(stack.white_balance())));
        let wb_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/white_balance.wgsl"),
                "white-balance",
                wb.clone(),
            )),
            vec![exposure_id],
        );
        let contrast = Rc::new(Cell::new(contrast_uniform(stack.contrast())));
        let contrast_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/contrast.wgsl"),
                "contrast",
                contrast.clone(),
            )),
            vec![wb_id],
        );
        let tone_curve = Rc::new(Cell::new(curve_lut(
            &stack.tone_curve().map(|t| t.points).unwrap_or_default(),
        )));
        let tone_curve_id = graph.add_node(
            Box::new(CurveNode::new(ctx.clone(), tone_curve.clone())),
            vec![contrast_id],
        );
        let hsl = Rc::new(Cell::new(hsl_uniform(stack.hsl())));
        let hsl_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/hsl.wgsl"),
                "hsl",
                hsl.clone(),
            )),
            vec![tone_curve_id],
        );
        let sharpen = Rc::new(Cell::new(sharpen_uniform(stack.sharpen())));
        let sharpen_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/sharpen.wgsl"),
                "sharpen",
                sharpen.clone(),
            )),
            vec![hsl_id],
        );

        Self {
            ctx,
            graph,
            output_id: sharpen_id,
            request,
            head_id,
            halo,
            exposure,
            wb,
            contrast,
            tone_curve,
            hsl,
            sharpen,
        }
    }

    pub fn halo(&self) -> u32 {
        self.halo
    }

    /// Render the edited interior `TILE_SIZE`² for `coord` as an `Rgba16Float`
    /// `COPY_SRC` texture. Re-runs the whole per-tile chain (the geometry head is
    /// dirtied each call because the tile coord changed).
    pub fn produce_tile(&mut self, coord: TileCoord) -> wgpu::Texture {
        self.request.set(TileRequest { coord, halo: self.halo });
        self.graph.mark_dirty(self.head_id);
        let haloed = self.graph.evaluate(self.output_id).clone();
        self.extract_interior(&haloed)
    }

    /// Copy the central `TILE_SIZE`² (offset by `halo`) of the haloed chain output
    /// into a fresh `COPY_SRC` texture. GPU→GPU; no readback.
    fn extract_interior(&self, haloed: &PipelineImage) -> wgpu::Texture {
        let out = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile-edit-interior"),
            size: wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PIPELINE_FORMAT,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: &haloed.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: self.halo,
                    y: self.halo,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: &out,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        self.ctx.queue.submit([enc.finish()]);
        out
    }
}
```

Note: `produce_tile` returns `wgpu::Texture` (the `TileProducer` contract), not `PipelineImage`. The geometry-head buffer needs `COPY_SRC` so `extract_interior` can copy from it — add `COPY_SRC` to the `GeometryHeadNode::ensure_out` texture usage: change its `usage` to `TEXTURE_BINDING | STORAGE_BINDING | COPY_SRC`. (Update Task 7 Step 1's `ensure_out` accordingly.)

- [ ] **Step 2a: Add `COPY_SRC` to the edit-node output textures**

`extract_interior` copies from the chain's **output** node (`sharpen`, a `PointOpNode`) — NOT the geometry head. So `COPY_SRC` must be on the edit nodes' output textures. In `nodes.rs`, add `| wgpu::TextureUsages::COPY_SRC` to the `ensure_out` texture `usage` of **`PointOpNode`, `CurveNode`, and `GeometryNode`** (all three currently use `TEXTURE_BINDING | STORAGE_BINDING`). This is purely additive — the display/blit path samples via `TEXTURE_BINDING` and is unaffected, and the existing `EditPipeline` goldens stay green. (Adding it to `GeometryHeadNode::ensure_out` too is harmless but not strictly required, since the head output is consumed via `TEXTURE_BINDING`; the load-bearing fix is the three edit nodes — the `TileEditPipeline` output is the `sharpen` `PointOpNode`, and `EditPipeline`'s output is the `geometry` `GeometryNode`, so both must be `COPY_SRC` for the Task-8 readback helpers to copy them directly.)

```rust
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
```

- [ ] **Step 3: Declare modules + exports**

In `ferrolite-pipeline/src/lib.rs`:
- add `mod tile_edit;` (after `mod pipeline;`)
- add `pub use tile_edit::TileEditPipeline;`
- `GeometryHeadNode`/`TileRequest` stay `pub(crate)` (internal to the pipeline crate).

- [ ] **Step 4: Compile-check (no test yet — covered by Task 8 golden)**

Run: `cargo build -p ferrolite-pipeline` and `cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings`
Expected: clean compile. (The behavior is asserted by the tile-seam golden in Task 8.)

- [ ] **Step 5: Commit**

```bash
git add ferrolite-pipeline/src/nodes.rs ferrolite-pipeline/src/tile_edit.rs \
  ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): TileEditPipeline (geometry-head + color chain per tile)"
```

---

## Task 8: Tile-seam golden — Sharpen across borders matches the whole-image result

**Files:**
- Modify: `ferrolite-pipeline/tests/golden.rs`
- Modify: `ferrolite-pipeline/tests/common/mod.rs` (add a `stitch`/`crop` helper)

**Interfaces:**
- Consumes: `TileEditPipeline`, `GpuPyramidSource`, `EditPipeline`, `blit_to_rgba8`, `OpStack`/`Op`/`Sharpen`; `ferrolite_image::{TileCoord, TILE_SIZE, LinearRgbaF32}`; `ferrolite_gpu::GpuContext`.
- Produces: the halo-correctness proof — a Sharpen edit, produced per-tile via `TileEditPipeline` (identity geometry, multi-tile image), stitched, must match the whole-image `EditPipeline` Sharpen render within `SEAM_TOL`.

**Why this proves the halo:** with identity geometry the per-tile head is a 1:1 haloed copy, so each tile's Sharpen reads its true ±radius neighbors from the halo. If the halo were dropped (`halo = 0`), tiles would sharpen against edge-clamped borders and the seam at x=`TILE_SIZE` would drift by tens of u8 levels — far outside `SEAM_TOL = 6`.

- [ ] **Step 1: Add a readback+stitch helper to the test common module**

Append to `ferrolite-pipeline/tests/common/mod.rs`:

```rust
use ferrolite_gpu::GpuContext;

/// Read a `TILE_SIZE`² `Rgba16Float` GPU texture back to display-linear f32 RGBA
/// on the CPU (test-only; the production produce path never reads back).
pub fn read_tile_linear(ctx: &GpuContext, tex: &wgpu::Texture) -> Vec<f32> {
    use ferrolite_image::TILE_SIZE;
    let bpp = 8u32; // RGBA16F
    let bpr_unpadded = TILE_SIZE * bpp;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bpr_padded = bpr_unpadded.div_ceil(align) * align;
    let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tile-readback"),
        size: (bpr_padded * TILE_SIZE) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buf,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(bpr_padded),
                rows_per_image: Some(TILE_SIZE),
            },
        },
        wgpu::Extent3d {
            width: TILE_SIZE,
            height: TILE_SIZE,
            depth_or_array_layers: 1,
        },
    );
    ctx.queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    ctx.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let mut out = vec![0.0f32; (TILE_SIZE * TILE_SIZE * 4) as usize];
    for row in 0..TILE_SIZE {
        let start = (row * bpr_padded) as usize;
        for px in 0..(TILE_SIZE * 4) {
            let o = start + px as usize * 2;
            let h = half::f16::from_le_bytes([data[o], data[o + 1]]);
            out[(row * TILE_SIZE * 4 + px) as usize] = h.to_f32();
        }
    }
    drop(data);
    buf.unmap();
    out
}
```

(The test crate already depends on `half`, `wgpu`, `ferrolite-gpu`, `ferrolite-image` via the existing goldens; if `half`/`wgpu` are not yet dev-deps of `ferrolite-pipeline`, add them under `[dev-dependencies]` in `ferrolite-pipeline/Cargo.toml` — they are already normal deps, so `dev-dependencies` inherit is unnecessary; confirm `cargo test` compiles.)

- [ ] **Step 2: Write the tile-seam golden test**

Append to `ferrolite-pipeline/tests/golden.rs` (extend the `use ferrolite_pipeline::{...}` import to add `GpuPyramidSource, TileEditPipeline`):

```rust
const SEAM_TOL: f32 = 0.02; // display-linear; absorbs f16 + the head resample.

#[test]
fn sharpen_tiles_match_whole_image_at_seam() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    // A multi-tile image: 300x200 -> 2x1 tiles at LOD 0 (seam at x = 256).
    let (iw, ih) = (300u32, 200u32);
    let src = common::gradient(iw, ih);
    let stack = OpStack::default().set_op(Op::Sharpen(Sharpen {
        amount: 0.8,
        radius: 3,
    }));

    // Whole-image reference: render the edited image to display-linear f32 by
    // evaluating the EditPipeline and reading its output back.
    let mut whole = EditPipeline::new(ctx.clone(), &src, stack.clone());
    let whole_lin = common::read_image_linear(&ctx, &whole.evaluate());

    // Per-tile producer over the GPU-resident source pyramid.
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &src));
    let mut tep = TileEditPipeline::new(ctx.clone(), pyramid, stack);

    // Produce both tiles, read interiors, and compare the valid region against
    // the whole-image reference — focusing on the seam column.
    use ferrolite_image::{TileCoord, TILE_SIZE};
    let mut max_diff = 0.0f32;
    for tx in 0..2u32 {
        let tile = tep.produce_tile(TileCoord { lod: 0, x: tx, y: 0 });
        let tile_lin = common::read_tile_linear(&ctx, &tile);
        for ly in 0..TILE_SIZE {
            for lx in 0..TILE_SIZE {
                let gx = tx * TILE_SIZE + lx;
                let gy = ly;
                if gx >= iw || gy >= ih {
                    continue; // out-of-image tile padding
                }
                let ti = ((ly * TILE_SIZE + lx) * 4) as usize;
                let wi = ((gy * iw + gx) * 4) as usize;
                for c in 0..3 {
                    max_diff = max_diff.max((tile_lin[ti + c] - whole_lin[wi + c]).abs());
                }
            }
        }
    }
    eprintln!("tile-seam max linear diff = {max_diff}");
    assert!(
        max_diff <= SEAM_TOL,
        "per-tile sharpen diverged from whole-image (diff {max_diff}) — halo broken?"
    );
}
```

- [ ] **Step 3: Add `read_image_linear` (whole-image readback) to the test common module**

Append to `ferrolite-pipeline/tests/common/mod.rs` (generalizes `read_tile_linear` to arbitrary dims):

```rust
/// Read an arbitrary-size `Rgba16Float` GPU texture back to display-linear f32
/// RGBA (test-only).
pub fn read_image_linear(ctx: &GpuContext, img: &ferrolite_pipeline::PipelineImage) -> Vec<f32> {
    let (w, h) = (img.width, img.height);
    let bpp = 8u32;
    let bpr_unpadded = w * bpp;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bpr_padded = bpr_unpadded.div_ceil(align) * align;
    let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("img-readback"),
        size: (bpr_padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &img.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buf,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(bpr_padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    ctx.queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    ctx.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let mut out = vec![0.0f32; (w * h * 4) as usize];
    for row in 0..h {
        let start = (row * bpr_padded) as usize;
        for px in 0..(w * 4) {
            let o = start + px as usize * 2;
            let hf = half::f16::from_le_bytes([data[o], data[o + 1]]);
            out[(row * w * 4 + px) as usize] = hf.to_f32();
        }
    }
    drop(data);
    buf.unmap();
    out
}
```

This requires `ferrolite-pipeline` to export `PipelineImage` (it already does, via `lib.rs`).

- [ ] **Step 4: Run the tile-seam golden**

Run: `cargo test -p ferrolite-pipeline --test golden sharpen_tiles_match`
Expected: on a GPU host, PASS (the seam diff is within `SEAM_TOL`). On headless, SKIP.

- [ ] **Step 5: Run the full pipeline suite + clippy**

Run: `cargo test -p ferrolite-pipeline` and `cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings`
Expected: PASS / clean (all prior goldens unaffected — they use `EditPipeline`, untouched by Plan 3).

- [ ] **Step 6: Commit**

```bash
git add ferrolite-pipeline/tests/golden.rs ferrolite-pipeline/tests/common/mod.rs
git commit -m "test(pipeline): tile-seam golden proving haloed sharpen matches whole-image"
```

---

## Task 9: Wire the full-res edited view into the viewer

**Files:**
- Modify: `ferrolite-app/Cargo.toml`
- Modify: `ferrolite-app/src/viewer/mod.rs`
- Create: `ferrolite-app/src/viewer/edit_producer.rs`
- Modify: `ferrolite-app/src/viewer/callback.rs`
- Modify: `ferrolite-app/src/app.rs`

**Interfaces:**
- Consumes: `ferrolite_pipeline::{GpuPyramidSource, TileEditPipeline, OpStack}`, `ferrolite_vt::TileProducer`, the sparse VT methods from Task 4 (`set_producing`/`set_opstack_version`/`needed_now`/`produce_view`).
- Produces:
  - `ViewerState.op_stack: OpStack` (default `OpStack::default()` — identity in Plan 3; Plan 4's panel mutates it).
  - `ViewerState.edit_producer: Option<EditTileProducer>` — owns the (`!Send`/`!Sync`) producer in single-threaded app state, NOT in `callback_resources`. `None` in Plan 3 (no UI makes the stack non-identity); built but `None`.
  - `EditTileProducer` (impl `ferrolite_vt::TileProducer`) holding a `TileEditPipeline` directly (no `Mutex` — it is never shared across threads).
  - Full-decode builds the producer into `ViewerState`; the drive loop, when a producer is present, calls `set_producing(true)` + `request_view_feedback` + `produce_view` (bounded) passing the producer by `&mut`. Without a producer the unchanged Spec-1 CPU path runs.

**Design note:** `TileEditPipeline`/`EditTileProducer` are `!Send`/`!Sync` (the pipeline holds `Rc`/`RefCell`/`Graph`). They therefore CANNOT live in eframe's `callback_resources` (which requires `Send+Sync` — that is why `VirtualTexture` wraps its receiver in a `Mutex`). They live in `ViewerState` (owned by `FerroliteApp`, only ever touched on the update/render thread) and are handed to `VirtualTexture::produce_view` as `&mut dyn TileProducer` per call. This is why Task 4's VT does not store the producer.

- [ ] **Step 1: Add the pipeline dependency**

In `ferrolite-app/Cargo.toml`, under `[dependencies]`, add (matching the workspace's path-dep style used for the other ferrolite crates):

```toml
ferrolite-pipeline = { path = "../ferrolite-pipeline" }
```

Run `cargo build -p ferrolite-app` to confirm it resolves.

- [ ] **Step 2: Add `op_stack` + `edit_producer` to `ViewerState`**

In `ferrolite-app/src/viewer/mod.rs`:
- extend the `use` for the pipeline: `use ferrolite_pipeline::OpStack;`
- add two fields to `struct ViewerState` (after `kind`):
```rust
    pub op_stack: OpStack,
    /// Plan 3: the full-res edit producer (built on full-decode when the stack is
    /// non-identity). `!Send`/`!Sync`, so it lives here, never in callback_resources.
    pub edit_producer: Option<edit_producer::EditTileProducer>,
```
- in `ViewerState::open`, initialize them: `op_stack: OpStack::default(),` and `edit_producer: None,`
- Note: `ViewerState` is owned by `FerroliteApp.state` and only touched on the update thread, so its new `!Send`/`!Sync` field is fine (it is not in `callback_resources`).

- [ ] **Step 3: Create `EditTileProducer`**

Create `ferrolite-app/src/viewer/edit_producer.rs`:

```rust
//! The photo edit tile producer: implements the engine-tier `ferrolite_vt::
//! TileProducer` by rendering each tile through a `TileEditPipeline` over the
//! GPU-resident source pyramid. Lives in the app (not the VT) so the VT stays
//! photo-agnostic (spec §5.2). `!Send`/`!Sync` (holds the pipeline's Rc/RefCell);
//! owned by `ViewerState` and only ever called on the render/update thread.

use ferrolite_gpu::GpuContext;
use ferrolite_image::TileCoord;
use ferrolite_pipeline::TileEditPipeline;
use ferrolite_vt::TileProducer;

pub struct EditTileProducer {
    pipeline: TileEditPipeline,
}

impl EditTileProducer {
    pub fn new(pipeline: TileEditPipeline) -> Self {
        Self { pipeline }
    }
}

impl TileProducer for EditTileProducer {
    fn produce(&mut self, _ctx: &GpuContext, coord: TileCoord) -> wgpu::Texture {
        // `_ctx` is the same device the pipeline was built against; the pipeline
        // holds its own Arc<GpuContext>, so we render through it directly.
        self.pipeline.produce_tile(coord)
    }
}
```

Declare the module in `ferrolite-app/src/viewer/mod.rs`: add `pub mod edit_producer;` and `pub use edit_producer::EditTileProducer;` (the `EditTileProducer` field added to `ViewerState` in Step 2 references it).

- [ ] **Step 4: Build the producer into `ViewerState` on full-decode (engaged when non-identity)**

In `ferrolite-app/src/app.rs`, in `apply_full_decoded`, after the sparse `full` VT is built and stored, build the edit producer into `ViewerState` — only when the stack is non-identity (always identity in Plan 3, so this is dormant but compiles). The producer is **not** put in `callback_resources` (it is `!Send`/`!Sync`); the VT (in `callback_resources`) is only marked producing + given a version. Insert, in the `if full_installed { if let Some(v) = self.state.viewer.as_mut() { ... } }` block (where `v.full_ready`/`begin_crossfade` are set):

```rust
                    if !v.op_stack.is_identity() {
                        // Build the GPU-resident pyramid + per-tile edit pipeline.
                        let ctx_arc =
                            std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
                        let pyramid = std::sync::Arc::new(
                            ferrolite_pipeline::GpuPyramidSource::new(&gpu, image),
                        );
                        let tep = ferrolite_pipeline::TileEditPipeline::new(
                            ctx_arc,
                            pyramid,
                            v.op_stack.clone(),
                        );
                        v.edit_producer = Some(viewer::EditTileProducer::new(tep));
                        // Mark the VT producer-driven + bump its version so the
                        // producer fills tiles instead of the CPU path.
                        let mut renderer = rs.renderer.write();
                        if let Some(g) =
                            renderer.callback_resources.get_mut::<viewer::ViewerGpu>()
                        {
                            if let Some(full) = g.full.as_mut() {
                                full.set_producing(true);
                                full.set_opstack_version(&g.ctx, 1);
                            }
                        }
                    }
```

(`gpu`/`rs`/`image` are already in scope in `apply_full_decoded`. No change to `ViewerGpu` is needed — the producer lives in `ViewerState`.)

- [ ] **Step 5: Drive `produce_view` (bounded) in the per-frame loop**

In `ferrolite-app/src/app.rs` `drive_viewer`: the existing `if let Some(full) = g.full.as_mut()` arm reconciles via `request_view_feedback`. The producer drive needs BOTH the VT (`full`, from `callback_resources`) and the producer (`self.state.viewer.edit_producer`, from app state). Because the VT borrow is taken under `rs.renderer.write()` and the producer lives on `self.state`, structure it so the produce step runs after `request_view_feedback` while both are borrowable. Concretely, in the same `renderer.write()` scope, capture the needed set, then produce:

```rust
                } else if let Some(full) = g.full.as_mut() {
                    full.request_view_feedback(&g.ctx);
                    // Plan 3: when an edit producer is present, render the needed
                    // tiles on the render thread (bounded). `produce_view` borrows
                    // the producer (which lives in ViewerState) by &mut per call.
                    if let Some(v) = self.state.viewer.as_mut() {
                        if let Some(producer) = v.edit_producer.as_mut() {
                            let needed = full.needed_now();
                            full.produce_view(&g.ctx, producer, &needed, MAX_PRODUCE_PER_FRAME);
                        }
                    }
                    tiles_pending = full.sparse_pending();
                }
```

If the borrow checker rejects holding `self.state.viewer` and the `renderer` write guard simultaneously (both borrow `self`/`frame` disjointly, so it should be fine — `renderer` comes from `frame`, `viewer` from `self.state`), restructure by collecting `needed` first, dropping the renderer guard, then re-acquiring — but the disjoint-borrow form above is preferred. `needed_now()`/`set_producing`/`produce_view` are the Task-4 methods. Define the budget constant near `VIEWER_TILE_BUDGET` in `app.rs`:

```rust
/// Max edited tiles rendered per frame on the render thread (bounds GPU work;
/// CLAUDE.md GPU rule). Remaining needed tiles are produced on subsequent frames.
const MAX_PRODUCE_PER_FRAME: usize = 8;
```

Note: `needed_now`/`last_needed`/`set_producing`/`produce_view` are all added in Task 4 — no new VT code here.

- [ ] **Step 7: Build + clippy the whole workspace**

Run: `cargo build --workspace` then `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean. (The producer path is dormant in Plan 3 because `op_stack` is always identity — no UI mutates it yet — so the in-app full-res view uses the unchanged Spec-1 sparse path.)

- [ ] **Step 8: Commit**

```bash
git add ferrolite-app/Cargo.toml ferrolite-app/src/viewer/mod.rs \
  ferrolite-app/src/viewer/edit_producer.rs ferrolite-app/src/app.rs
git commit -m "feat(app): wire full-res edit producer into the viewer (identity default)"
```

---

## Task 10: Workspace gate + hold for the author's visual test

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all` then `cargo fmt --all --check`
Expected: no diff.

- [ ] **Step 2: Clippy (workspace, warnings as errors)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: Tests (workspace; goldens skip headless)**

Run: `cargo test --workspace`
Expected: PASS everywhere. On the dev GPU the new goldens auto-author on first run (the `producer_fills_*`, `sharpen_tiles_match_*`, and `gpu_pyramid` tests execute); on headless CI they skip. Re-run once on the dev GPU so the fixtures/goldens are deterministic, then `git add` any newly written PNG fixtures (none expected for Plan 3 — the seam test is a computed comparison, not a committed PNG) and commit if present.

- [ ] **Step 4: STOP — hand off for the author's visual test**

Per CLAUDE.md "Finishing a branch", the green gate is necessary but **not sufficient**. Do not merge/PR/finish. Present finish options, then **hold** for Jann's hands-on test:

> Open an image → zoom to 1:1. Expect: sharp, seam-free full-res (the OpStack is identity in Plan 3, so this is the Spec-1 sparse path validated to still be correct after the producer wiring; the edit-then-1:1 producer path is golden-proven here and becomes interactively visible in Plan 4's panel).

Address any issues the author finds before completing the branch.

---

## Self-Review

**Spec coverage (spec §5, §6.2, §10, §11.3):**
- §5.1 tile halo + ferrolite-image halo math, pure-tested → Tasks 1, 2. ✅
- §5.2 GPU TileProducer seam, no CPU readback, VT source-agnostic, edit producer samples haloed GPU-resident source + runs OpStack per tile → Tasks 3, 6, 7, 9. ✅
- §5.3 edited-tile versioned caching + lazy re-stream + cancellation/bounded → Tasks 3 (`VersionedResidency`), 4 (`set_opstack_version`/`produce_view` bounded), 9 (per-frame budget). ✅
- §6.2 full-res tier streams edited tiles via the producer, coarse-LOD fallback inherited (sparse path unchanged) → Tasks 4, 9. ✅
- §8.4 geometry at the head (crop/rotate via per-tile sampling transform) — user chose the full option → Tasks 5, 7. ✅
- §10 tile-seam golden (Sharpen across borders == whole-image) + pure units (halo math, version keying, tile_with_halo) → Tasks 1, 2, 3, 8. ✅
- §11.3 "Wires the full-res 1:1 edited view into the viewer" → Task 9 (machinery wired; engaged on non-identity; identity default per the user's deferral choice). ✅
- CLAUDE.md: pipelines once (every node/producer + `GpuPyramidSource` built once), bounded GPU (per-frame produce budget), no UI-thread blocking (CPU path keeps `ferrolite-jobs`; GPU produce on render thread bounded), no `ferrolite-gpu`/`ferrolite-vt` photo coupling. ✅

**Placeholder scan:** every code step contains complete code or a shader; commands have explicit expected output. No "TBD"/"add error handling"/"similar to Task N". ✅

**Type consistency:** `tile_with_halo(coord, halo)`, `haloed_tile_extent`/`haloed_tile_origin`, `VersionedResidency::{set_version,mark,is_current,forget,to_produce,current}`, `TileProducer::produce(&self, &GpuContext, TileCoord) -> wgpu::Texture`, `TilePool::copy_into`, `GeometryUniform.out_origin`, `geometry_tile_uniform`, `GpuPyramidSource::{new,level_count,level_size,level}`, `TileEditPipeline::{new,produce_tile,halo}`, `VirtualTexture::{set_producing,set_opstack_version,needed_now,produce_view}` (producer passed per call, not stored) are used consistently across tasks. `TileProducer::produce(&mut self, ...)` is not `Send`/`Sync`; `EditTileProducer` + `TileEditPipeline` live in `ViewerState`, never in `callback_resources`. `produce_tile` returns `wgpu::Texture` (matching `TileProducer`), and `GeometryHeadNode::ensure_out` carries `COPY_SRC` so `extract_interior` can copy from the haloed buffer (Task 7 Step 2a). ✅

**Known residual to verify during execution:** Task 9 Step 6 adds `last_needed`/`needed_now` to keep the producer path self-contained; confirm `request_view_feedback` computes `needed` before the borrow it is stored under (it does — `needed` is the first local). The producer path is dormant in Plan 3 (identity stack), so the workspace gate exercises it only through the headless-skipping Task 4/8 GPU tests; the author's Plan-4 panel is what drives it interactively.
