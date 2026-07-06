# ferrolite-mask Brush Rasterizer + VT Streaming (P1 Plan 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the inert `MaskComponent::Brush` from Plan 1 live: a pure, tested stroke→dab-spacing + incremental-stamp + halo math layer, a dab-stamping WGSL rasterizer over the single-channel `MaskBuffer`, and a **haloed per-tile** rasterization path that reuses the Spec 2 VT tile/halo math — proven by a brush-rasterization golden and a tile-seam equality test (per-tile haloed brush == whole-image at tile borders).

**Architecture:** All new code lives in the engine-tier `ferrolite-mask` crate. The parametric `Stroke`/`BrushNode` model already exists (Plan 1). Plan 2 adds: (1) a pure CPU layer (`stroke.rs`) that resamples a stroke polyline into evenly-spaced `Dab`s, selects only the *new* dabs since the last pointer sample, computes the dab falloff/compositing reference the WGSL mirrors, and derives `halo = max dab radius`; (2) a build-once `BrushRasterizer` compute pass (`brush.rs` + `brush_dab.wgsl`) that stamps a batch of dabs onto an existing `MaskBuffer` (incremental, ping-pong, no read-modify-write), with `rasterize_full` and `rasterize_tile` entry points; (3) goldens. The rasterizer computes coverage **analytically in normalized source coordinates** (identical to the Plan 1 shape evaluators), so a dab is a circle in normalized space and per-tile rasterization is bit-consistent with whole-image at borders once `halo = max dab radius` is honored. **`ferrolite-vt` is not modified** — Plan 2 reuses `ferrolite-image`'s existing halo/tile math (`haloed_tile_origin`/`haloed_tile_extent`/`tile_pixel_origin`/`level_size`), which is the same seam `PyramidTileSource` and the Spec 2 tile producer use. Wiring the rasterizer into an actual VT `TileProducer` is Plan 3 (pipeline/app), exactly as `ferrolite-vt/src/producer.rs` already documents ("the edit producer … reuses the Plan 1/2 nodes … lives in ferrolite-app/pipeline, never here").

**Tech Stack:** Rust 2021, `wgpu` 22, `bytemuck` (Pod uniforms + a Pod dab storage-buffer struct), `half` (dev-only, golden input upload), `serde` (model, already present), `serde_json` + `image` (dev-only, goldens). `ferrolite-image` (engine-tier) re-added for tile/halo math. Compute shader in WGSL under `ferrolite-mask/src/shaders/`.

## Global Constraints

- **Branch:** `feat/p1-masking-brush-vt` (already checked out, off `main`; Plan 1 is already merged into `main`). Do NOT merge/PR/finish — stop at the green gate and report. This is 1 of 5 plans.
- **Engine tier / dependency purity (map §3, design §3, contract 4/D7):** `ferrolite-mask` may depend ONLY on `ferrolite-gpu`, `ferrolite-image`, `wgpu`, `bytemuck`, `half`, `serde` (+ dev-deps `serde_json`, `image`). NO copyleft/photo-domain deps (`ferrolite-pipeline`, `-color`, `-decode`, `-catalog`, `-export`, `-lens`, `-vt`), NO `ferrolite-ai`, NO model weights. `ferrolite-image` is engine-tier and permitted (it is the generic image/tile vocabulary — `LinearRgbaF32`, `TileCoord`, halo math).
- **License:** `license.workspace = true` (GPL-3.0-only). Do NOT override; the relicensable property is a property of the dependency graph, not the label.
- **Executor unchanged (contract 4):** do NOT modify `ferrolite-gpu/src/executor.rs`. Do NOT modify `ferrolite-vt`. The brush rasterizer is a pass living in `ferrolite-mask`.
- **Coordinates:** dabs, strokes, and radii are all in **normalized source coordinates** ([0,1]² over the pre-geometry image). A dab is a **circle in normalized space** (an ellipse in pixels on non-square images), matching the `RadialGradient` evaluator's convention. Display→normalized mapping and any round-in-pixels brush is a Plan 4 (UI) concern.
- **The only halo in the masking stage is here:** `halo = max dab radius`, converted to pixels per level. Parametric/range shapes stay zero-halo.
- **GPU discipline (CLAUDE.md):** build the rasterizer pipeline ONCE (via the `GpuContext::shader_module` cache); never rebuild per stroke/dab/tile. Incremental stamping stamps only the *new* dabs onto the cached buffer (ping-pong), never a full re-raster per pointer move. Nothing slow on the UI thread (that scheduling is Plan 3's; Plan 2 keeps the rasterizer a cheap, bounded, cancellation-friendly pass).
- **Goldens:** GPU tests must `let Some(ctx) = GpuContext::headless() else { return; }` first so `cargo test --workspace` stays green headless. Golden PNGs are authored on the dev GPU (RTX 3060/3070 class) with the fixture absent or `UPDATE_GOLDEN=1`, visually confirmed, and committed.
- **Gate (this plan's end state):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` all green.
- **Style (rust rules):** `snake_case`/`PascalCase`, immutable-by-default, no `unwrap()` outside tests, files focused (<800 lines). Uniform + storage structs are `#[repr(C)]` + `bytemuck::Pod`/`Zeroable` with **explicit padding to a 16-byte multiple** for uniforms and a member-alignment-consistent size for storage-array structs (see `RadialGradientUniform`/`FoldUniform` precedent).

---

## Key design decisions (locked here; the WGSL and pure reference mirror each other exactly)

**Dab coverage (falloff).** For normalized distance `d` from a dab center, dab radius `r`, hardness `h∈[0,1]`, flow `f∈[0,1]`:

```
t = d / r                                  (r <= 0 -> alpha 0)
core = clamp(h, 0, 1)
ring: if t <= core        -> 1.0
      else if t >= 1.0    -> 0.0
      else if core >= 1.0 -> (t < 1.0 ? 1.0 : 0.0)      // hard edge, avoid /0
      else                -> 1.0 - smoothstep(0, 1, (t - core) / (1 - core))
alpha = ring * clamp(f, 0, 1)
```

**Compositing along a stroke.** Accumulator `acc∈[0,1]`, dab `alpha`:
- **paint** (`erase = false`): `acc' = acc + (1 - acc) * alpha`  (standard "over")
- **erase** (`erase = true`):  `acc' = acc * (1 - alpha)`

Dabs composite **in order**. This is left-to-right sequential, so processing `[a,b,c]` in one pass == processing `[a]` then `[b,c]` in two passes over the same accumulator (float-equal within golden tolerance) — the property the incremental path relies on.

**Dab spacing (append-stable).** A stroke resamples into dabs at arc-lengths `0, step, 2·step, …` for every `k·step <= total_len`, where `step = spacing_frac · r_max_stroke` (`r_max_stroke` = the stroke's largest node radius; guards `step > 0`). Params (pos/radius/hardness/flow) are interpolated by global arc-length fraction. **No forced endpoint dab** — appending a node only *adds* dabs at the tail and never moves an existing one, so incremental stamping is a stable index suffix. A single-node stroke → one dab at that node; an empty stroke → no dabs.

**Halo.** `halo_norm = max dab radius over all strokes` (normalized). `halo_px(level_w, level_h) = ceil(halo_norm · max(level_w, level_h))`. Using `max(w,h)` covers both axes for a normalized-circular dab on a non-square level (conservative on the shorter axis; correctness over tightness). Zero strokes → halo 0.

**VT streaming boundary.** Plan 2 does **not** touch `ferrolite-vt`. "Streaming the brush buffer as a generic large source through the Spec 2 halo + tile-producer seam" is realized by `BrushRasterizer::rasterize_tile`, which rasterizes the interior `TILE_SIZE²` of a tile using the **haloed** region (via `ferrolite-image::haloed_tile_origin`/`haloed_tile_extent`) so border dabs are complete. The Plan 3 edit producer (in pipeline/app) calls this per visible tile and writes the VT slot. The tile-seam test proves the halo makes per-tile == whole-image.

> **REVIEW FLAG for Jann (VT boundary):** This plan keeps Plan 2 engine-only and does NOT add a `ferrolite-vt` `TileProducer` impl or a mask-side `TileSource` — the VT producer wiring lands in Plan 3, matching the `producer.rs` doc comment. If you want Plan 2 to also land a concrete brush→VT producer demonstration (e.g. a `MaskTileSource`), that is a scope shift into Plan 3's territory and needs `ferrolite-vt`/pipeline changes; confirm before Task 3.

> **REVIEW FLAG for Jann (dab shape):** Dabs are circular in **normalized** space (ellipse in pixels on non-square images), consistent with `RadialGradient`. If you want brushes round in **pixel/display** space instead, the radius would need an aspect term; that is a Plan 4 (UI mapping) decision but would change `dab_alpha`'s distance metric here. Confirm the normalized-circle convention before Task 1 is committed.

> **REVIEW FLAG for Jann (spacing default):** `SPACING_FRAC` default `0.25` (a dab every quarter-radius). Confirm or adjust — it only affects density/cost, not correctness, and is a `pub const` one-liner.

---

## File Structure

New in `ferrolite-mask/`:

- `src/stroke.rs` — pure CPU layer: `Dab`, `stroke_dabs`, `StrokeCursor`, `dab_alpha`, `composite_dabs`, `max_dab_radius`, `halo_px`, `SPACING_FRAC`. No GPU. The 80%+ pure-math target.
- `src/brush.rs` — `BrushRasterizer` (build-once compute pass), `BrushUniform`, `GpuDab`, `stamp_onto`, `rasterize_full`, `rasterize_tile`.
- `src/shaders/brush_dab.wgsl` — the dab-stamping compute shader (mirrors `dab_alpha` + `composite_dabs`).
- `tests/brush_golden.rs` — full-image paint + erase rasterization goldens.
- `tests/brush_tile_seam.rs` — tile-seam halo-correctness (whole == tiled interiors) + incremental-equals-single-shot GPU tests.
- `tests/fixtures/brush_stroke.png`, `tests/fixtures/brush_erase.png` — committed golden references (authored on dev GPU).

Modified in `ferrolite-mask/`:

- `Cargo.toml` — re-add `ferrolite-image = { workspace = true }`.
- `src/buffer.rs` — add `MaskBuffer::alloc_zeroed`.
- `src/lib.rs` — `mod brush; mod stroke;` + public re-exports.

Reused unchanged: `src/pass.rs` conventions (mirrored, not extended — the brush pass needs a storage-buffer binding the existing helpers don't have), `tests/common/mod.rs` (`read_r32f`, `assert_mask_golden`, `mask_max_abs_diff`), `ferrolite-image` tile/halo math, `ferrolite-vt` (untouched).

---

### Task 1: Pure dab-geometry math (`stroke.rs`: `Dab`, `stroke_dabs`, `max_dab_radius`)

**Files:**
- Create: `ferrolite-mask/src/stroke.rs`
- Modify: `ferrolite-mask/src/lib.rs` (`mod stroke;` + re-exports)

**Interfaces:**
- Consumes: `crate::vec::Vec2`, `crate::model::{Stroke, BrushNode}` (Plan 1).
- Produces:
  - `ferrolite_mask::Dab { pos: Vec2, radius: f32, hardness: f32, flow: f32 }` — `#[derive(Clone, Copy, PartialEq, Debug)]`, a resolved stamp in normalized coords.
  - `ferrolite_mask::SPACING_FRAC: f32 = 0.25` — default dab spacing as a fraction of the stroke's max radius.
  - `ferrolite_mask::stroke_dabs(stroke: &Stroke, spacing_frac: f32) -> Vec<Dab>` — append-stable resample (see locked decision).
  - `ferrolite_mask::max_dab_radius(strokes: &[Stroke]) -> f32` — largest node radius across all strokes; `0.0` if none.

- [ ] **Step 1: Write the failing tests**

Create `ferrolite-mask/src/stroke.rs` with only the test module first (fails to compile — the RED):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BrushNode, Stroke};

    fn node(x: f32, y: f32, r: f32) -> BrushNode {
        BrushNode {
            pos: Vec2::new(x, y),
            radius: r,
            hardness: 0.5,
            flow: 1.0,
        }
    }

    #[test]
    fn empty_stroke_yields_no_dabs() {
        let s = Stroke {
            nodes: vec![],
            erase: false,
        };
        assert!(stroke_dabs(&s, SPACING_FRAC).is_empty());
    }

    #[test]
    fn single_node_yields_one_dab_at_that_node() {
        let s = Stroke {
            nodes: vec![node(0.3, 0.4, 0.1)],
            erase: false,
        };
        let dabs = stroke_dabs(&s, SPACING_FRAC);
        assert_eq!(dabs.len(), 1);
        assert_eq!(dabs[0].pos, Vec2::new(0.3, 0.4));
        assert_eq!(dabs[0].radius, 0.1);
    }

    #[test]
    fn straight_stroke_spaces_dabs_by_step() {
        // Horizontal 0.0->0.4, constant radius 0.1, spacing 0.5 -> step 0.05.
        // Dabs at k*0.05 for k*0.05 <= 0.4 -> k = 0..=8 -> 9 dabs.
        let s = Stroke {
            nodes: vec![node(0.0, 0.5, 0.1), node(0.4, 0.5, 0.1)],
            erase: false,
        };
        let dabs = stroke_dabs(&s, 0.5);
        assert_eq!(dabs.len(), 9);
        assert!((dabs[0].pos.x - 0.0).abs() < 1e-5);
        assert!((dabs[1].pos.x - 0.05).abs() < 1e-5);
        assert!((dabs[8].pos.x - 0.40).abs() < 1e-5);
        assert!(dabs.iter().all(|d| (d.pos.y - 0.5).abs() < 1e-5));
    }

    #[test]
    fn appending_a_node_only_adds_tail_dabs() {
        // Append-stability: extending 0.4 -> 0.5 keeps the first 9 dabs identical.
        let short = Stroke {
            nodes: vec![node(0.0, 0.5, 0.1), node(0.4, 0.5, 0.1)],
            erase: false,
        };
        let long = Stroke {
            nodes: vec![node(0.0, 0.5, 0.1), node(0.5, 0.5, 0.1)],
            erase: false,
        };
        let a = stroke_dabs(&short, 0.5);
        let b = stroke_dabs(&long, 0.5);
        assert!(b.len() > a.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((x.pos.x - y.pos.x).abs() < 1e-5);
        }
    }

    #[test]
    fn params_interpolate_along_the_path() {
        // radius 0.1 -> 0.3 across the stroke; a mid dab is ~0.2.
        let s = Stroke {
            nodes: vec![node(0.0, 0.5, 0.1), node(0.4, 0.5, 0.3)],
            erase: false,
        };
        let dabs = stroke_dabs(&s, 0.5);
        let mid = &dabs[dabs.len() / 2];
        assert!(mid.radius > 0.15 && mid.radius < 0.25, "got {}", mid.radius);
    }

    #[test]
    fn degenerate_zero_length_stroke_yields_one_dab() {
        let s = Stroke {
            nodes: vec![node(0.2, 0.2, 0.1), node(0.2, 0.2, 0.1)],
            erase: false,
        };
        assert_eq!(stroke_dabs(&s, 0.5).len(), 1);
    }

    #[test]
    fn zero_radius_stroke_does_not_hang_and_yields_endpoints() {
        // step guards to > 0 so this terminates; exact count is not asserted.
        let s = Stroke {
            nodes: vec![node(0.0, 0.5, 0.0), node(0.4, 0.5, 0.0)],
            erase: false,
        };
        let dabs = stroke_dabs(&s, 0.5);
        assert!(!dabs.is_empty());
    }

    #[test]
    fn max_dab_radius_is_the_largest_node_radius() {
        let strokes = vec![
            Stroke {
                nodes: vec![node(0.0, 0.0, 0.1), node(0.1, 0.1, 0.25)],
                erase: false,
            },
            Stroke {
                nodes: vec![node(0.2, 0.2, 0.05)],
                erase: false,
            },
        ];
        assert!((max_dab_radius(&strokes) - 0.25).abs() < 1e-6);
        assert_eq!(max_dab_radius(&[]), 0.0);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-mask --lib stroke`
Expected: FAIL — compile error, `Dab`/`stroke_dabs`/`max_dab_radius`/`SPACING_FRAC` not found.

- [ ] **Step 3: Implement the pure dab geometry**

Prepend to `ferrolite-mask/src/stroke.rs` (above the test module):

```rust
//! Pure CPU brush math (no GPU): resample a `Stroke` polyline into evenly-spaced
//! `Dab`s, select the new dabs since the last pointer sample, and derive the
//! `halo = max dab radius`. The `brush_dab.wgsl` rasterizer mirrors `dab_alpha`
//! and `composite_dabs` exactly. All coordinates are normalized source space.

use crate::model::{BrushNode, Stroke};
use crate::vec::Vec2;

/// Default dab spacing as a fraction of the stroke's max node radius.
pub const SPACING_FRAC: f32 = 0.25;

/// A resolved brush stamp in normalized source coordinates.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Dab {
    pub pos: Vec2,
    pub radius: f32,
    pub hardness: f32,
    pub flow: f32,
}

impl Dab {
    fn from_node(n: &BrushNode) -> Self {
        Self {
            pos: n.pos,
            radius: n.radius,
            hardness: n.hardness,
            flow: n.flow,
        }
    }
}

fn dist(a: Vec2, b: Vec2) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    (dx * dx + dy * dy).sqrt()
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_node(a: &BrushNode, b: &BrushNode, t: f32) -> Dab {
    Dab {
        pos: Vec2::new(lerp(a.pos.x, b.pos.x, t), lerp(a.pos.y, b.pos.y, t)),
        radius: lerp(a.radius, b.radius, t),
        hardness: lerp(a.hardness, b.hardness, t),
        flow: lerp(a.flow, b.flow, t),
    }
}

/// The largest node radius across all strokes (normalized); `0.0` if none.
pub fn max_dab_radius(strokes: &[Stroke]) -> f32 {
    strokes
        .iter()
        .flat_map(|s| s.nodes.iter())
        .map(|n| n.radius)
        .fold(0.0_f32, f32::max)
}

/// Resample `stroke` into append-stable, evenly-spaced dabs. Dabs sit at
/// arc-lengths `0, step, 2*step, …` for every `k*step <= total_len`, where
/// `step = spacing_frac * (max node radius)` (guarded > 0). Params interpolate by
/// global arc-length fraction. Appending a node only adds tail dabs (see tests).
pub fn stroke_dabs(stroke: &Stroke, spacing_frac: f32) -> Vec<Dab> {
    let nodes = &stroke.nodes;
    if nodes.is_empty() {
        return Vec::new();
    }
    if nodes.len() == 1 {
        return vec![Dab::from_node(&nodes[0])];
    }

    // Cumulative arc-length at each node.
    let mut cum = Vec::with_capacity(nodes.len());
    cum.push(0.0_f32);
    for w in nodes.windows(2) {
        let prev = *cum.last().unwrap();
        cum.push(prev + dist(w[0].pos, w[1].pos));
    }
    let total_len = *cum.last().unwrap();

    let r_max = nodes.iter().map(|n| n.radius).fold(0.0_f32, f32::max);
    let step = (spacing_frac * r_max).max(1e-4);

    if total_len <= 1e-6 {
        return vec![Dab::from_node(&nodes[0])];
    }

    let mut dabs = Vec::new();
    let mut k = 0u32;
    loop {
        let s = k as f32 * step;
        if s > total_len {
            break;
        }
        // Locate the segment containing arc-length `s`.
        let seg = cum
            .windows(2)
            .position(|c| s >= c[0] && s <= c[1])
            .unwrap_or(nodes.len() - 2);
        let seg_len = cum[seg + 1] - cum[seg];
        let t = if seg_len > 1e-9 {
            (s - cum[seg]) / seg_len
        } else {
            0.0
        };
        dabs.push(lerp_node(&nodes[seg], &nodes[seg + 1], t));
        k += 1;
    }
    dabs
}
```

- [ ] **Step 4: Wire into `lib.rs`**

Add `mod stroke;` (alphabetical: after `mod shapes;`) and the re-export line:

```rust
pub use stroke::{max_dab_radius, stroke_dabs, Dab, SPACING_FRAC};
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ferrolite-mask --lib stroke`
Expected: PASS (8 tests).

- [ ] **Step 6: fmt + clippy**

Run: `cargo fmt -p ferrolite-mask && cargo clippy -p ferrolite-mask --all-targets -- -D warnings`
Expected: no diffs, no warnings.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-mask/src/stroke.rs ferrolite-mask/src/lib.rs
git commit -m "feat(mask): pure stroke->dab spacing math (append-stable) + max dab radius"
```

---

### Task 2: Incremental cursor + dab-coverage/composite reference + halo (`stroke.rs`)

**Files:**
- Modify: `ferrolite-mask/src/stroke.rs` (add `StrokeCursor`, `dab_alpha`, `composite_dabs`, `halo_px` + tests)
- Modify: `ferrolite-mask/src/lib.rs` (extend re-exports)

**Interfaces:**
- Consumes: `Dab`, `stroke_dabs` (Task 1).
- Produces:
  - `ferrolite_mask::StrokeCursor { emitted: usize }` — `#[derive(Clone, Copy, Debug, Default)]`; `StrokeCursor::new() -> Self`; `fn advance<'a>(&mut self, all_dabs: &'a [Dab]) -> &'a [Dab]` returns the not-yet-emitted suffix and advances the cursor; `fn reset(&mut self)`.
  - `ferrolite_mask::dab_alpha(dist: f32, radius: f32, hardness: f32, flow: f32) -> f32` — the CPU reference the WGSL mirrors (see locked decision).
  - `ferrolite_mask::composite_dabs(alphas: &[f32], base: f32, erase: bool) -> f32` — fold dab alphas onto `base` in order (paint = over, erase = multiplicative).
  - `ferrolite_mask::halo_px(halo_norm: f32, level_w: u32, level_h: u32) -> u32` — `ceil(halo_norm * max(level_w, level_h))`, `0` if `halo_norm <= 0`.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `ferrolite-mask/src/stroke.rs`:

```rust
    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn cursor_yields_only_new_dabs() {
        let dabs = vec![
            Dab { pos: Vec2::new(0.0, 0.0), radius: 0.1, hardness: 0.5, flow: 1.0 },
            Dab { pos: Vec2::new(0.1, 0.0), radius: 0.1, hardness: 0.5, flow: 1.0 },
            Dab { pos: Vec2::new(0.2, 0.0), radius: 0.1, hardness: 0.5, flow: 1.0 },
        ];
        let mut cur = StrokeCursor::new();
        assert_eq!(cur.advance(&dabs[..1]).len(), 1); // first sample: 1 new
        assert_eq!(cur.advance(&dabs[..1]).len(), 0); // no growth: 0 new
        assert_eq!(cur.advance(&dabs).len(), 2); // grew to 3: 2 new
        assert_eq!(cur.advance(&dabs).len(), 0); // stable
    }

    #[test]
    fn cursor_reset_reemits_from_start() {
        let dabs = vec![Dab { pos: Vec2::new(0.0, 0.0), radius: 0.1, hardness: 0.5, flow: 1.0 }];
        let mut cur = StrokeCursor::new();
        assert_eq!(cur.advance(&dabs).len(), 1);
        cur.reset();
        assert_eq!(cur.advance(&dabs).len(), 1);
    }

    #[test]
    fn dab_alpha_is_full_in_core_and_zero_outside() {
        // hardness 0.5 -> full inside t<=0.5, zero at/after t>=1.
        assert!(approx(dab_alpha(0.0, 0.1, 0.5, 1.0), 1.0)); // center
        assert!(approx(dab_alpha(0.04, 0.1, 0.5, 1.0), 1.0)); // t=0.4 in core
        assert!(approx(dab_alpha(0.1, 0.1, 0.5, 1.0), 0.0)); // t=1 edge
        assert!(approx(dab_alpha(0.2, 0.1, 0.5, 1.0), 0.0)); // outside
    }

    #[test]
    fn dab_alpha_scales_by_flow() {
        assert!(approx(dab_alpha(0.0, 0.1, 0.5, 0.3), 0.3));
    }

    #[test]
    fn dab_alpha_zero_radius_is_zero() {
        assert!(approx(dab_alpha(0.0, 0.0, 0.5, 1.0), 0.0));
    }

    #[test]
    fn dab_alpha_hard_edge_when_hardness_one() {
        assert!(approx(dab_alpha(0.09, 0.1, 1.0, 1.0), 1.0)); // t=0.9 < 1
        assert!(approx(dab_alpha(0.1, 0.1, 1.0, 1.0), 0.0)); // t=1
    }

    #[test]
    fn dab_alpha_softens_between_core_and_edge() {
        // t=0.75 with core 0.5 -> smoothstep midpoint -> 1 - 0.5 = 0.5.
        let a = dab_alpha(0.075, 0.1, 0.5, 1.0);
        assert!(a > 0.45 && a < 0.55, "got {a}");
    }

    #[test]
    fn composite_dabs_paint_is_over() {
        // over(0, 0.5) = 0.5; over(0.5, 0.5) = 0.75.
        assert!(approx(composite_dabs(&[0.5, 0.5], 0.0, false), 0.75));
    }

    #[test]
    fn composite_dabs_erase_is_multiplicative() {
        // start 1.0, erase 0.5 -> 0.5, erase 0.5 -> 0.25.
        assert!(approx(composite_dabs(&[0.5, 0.5], 1.0, true), 0.25));
    }

    #[test]
    fn composite_dabs_split_equals_whole() {
        let all = [0.3, 0.7, 0.2];
        let whole = composite_dabs(&all, 0.0, false);
        let split = composite_dabs(&all[2..], composite_dabs(&all[..2], 0.0, false), false);
        assert!(approx(whole, split));
    }

    #[test]
    fn halo_px_ceils_over_the_larger_axis() {
        // 0.1 * max(200, 100) = 20 -> 20.
        assert_eq!(halo_px(0.1, 200, 100), 20);
        // 0.101 * 200 = 20.2 -> ceil 21.
        assert_eq!(halo_px(0.101, 200, 100), 21);
        assert_eq!(halo_px(0.0, 200, 100), 0);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-mask --lib stroke`
Expected: FAIL — `StrokeCursor`/`dab_alpha`/`composite_dabs`/`halo_px` not found.

- [ ] **Step 3: Implement the reference + cursor + halo**

Append to `ferrolite-mask/src/stroke.rs` (before the test module):

```rust
/// A pointer-sample cursor: tracks how many dabs of a growing stroke have been
/// stamped, so incremental stamping submits only the new suffix (design §4.3).
/// Relies on `stroke_dabs` being append-stable.
#[derive(Clone, Copy, Debug, Default)]
pub struct StrokeCursor {
    emitted: usize,
}

impl StrokeCursor {
    pub fn new() -> Self {
        Self { emitted: 0 }
    }

    /// The dabs of `all_dabs` not yet emitted; advances the cursor to the end.
    pub fn advance<'a>(&mut self, all_dabs: &'a [Dab]) -> &'a [Dab] {
        let start = self.emitted.min(all_dabs.len());
        self.emitted = all_dabs.len();
        &all_dabs[start..]
    }

    /// Re-emit from the start on the next `advance` (e.g. buffer was cleared).
    pub fn reset(&mut self) {
        self.emitted = 0;
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Dab coverage at normalized distance `dist` (the WGSL mirrors this exactly).
pub fn dab_alpha(dist: f32, radius: f32, hardness: f32, flow: f32) -> f32 {
    if radius <= 0.0 {
        return 0.0;
    }
    let t = dist / radius;
    let core = hardness.clamp(0.0, 1.0);
    let ring = if t <= core {
        1.0
    } else if t >= 1.0 {
        0.0
    } else if core >= 1.0 {
        // Guarded above by t < 1.0, so this is the hard-edge inside.
        1.0
    } else {
        1.0 - smoothstep(0.0, 1.0, (t - core) / (1.0 - core))
    };
    ring * flow.clamp(0.0, 1.0)
}

/// Fold dab `alphas` onto `base` in order: paint = "over", erase = multiplicative.
pub fn composite_dabs(alphas: &[f32], base: f32, erase: bool) -> f32 {
    alphas.iter().fold(base, |acc, &a| {
        if erase {
            acc * (1.0 - a)
        } else {
            acc + (1.0 - acc) * a
        }
    })
}

/// Pixel halo for a normalized `halo_norm` (= max dab radius) at a level. Uses
/// the larger axis so a normalized-circular dab is fully covered on both axes.
pub fn halo_px(halo_norm: f32, level_w: u32, level_h: u32) -> u32 {
    if halo_norm <= 0.0 {
        return 0;
    }
    (halo_norm * level_w.max(level_h) as f32).ceil() as u32
}
```

- [ ] **Step 4: Extend `lib.rs` re-exports**

Update the stroke re-export line to:

```rust
pub use stroke::{
    composite_dabs, dab_alpha, halo_px, max_dab_radius, stroke_dabs, Dab, StrokeCursor,
    SPACING_FRAC,
};
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ferrolite-mask --lib stroke`
Expected: PASS (all Task 1 + 11 new tests).

- [ ] **Step 6: fmt + clippy**

Run: `cargo fmt -p ferrolite-mask && cargo clippy -p ferrolite-mask --all-targets -- -D warnings`
Expected: no diffs, no warnings.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-mask/src/stroke.rs ferrolite-mask/src/lib.rs
git commit -m "feat(mask): incremental stroke cursor + dab falloff/composite reference + halo math"
```

---

### Task 3: GPU dab-stamping rasterizer + `alloc_zeroed` + WGSL

**Files:**
- Modify: `ferrolite-mask/Cargo.toml` (re-add `ferrolite-image`)
- Modify: `ferrolite-mask/src/buffer.rs` (add `MaskBuffer::alloc_zeroed`)
- Create: `ferrolite-mask/src/brush.rs`
- Create: `ferrolite-mask/src/shaders/brush_dab.wgsl`
- Create: `ferrolite-mask/tests/brush_smoke.rs` (GPU sanity test, reuses `tests/common`)
- Modify: `ferrolite-mask/src/lib.rs` (`mod brush;` + re-exports)

**Interfaces:**
- Consumes: `GpuContext`, `MaskBuffer`, `MASK_FORMAT`, `Dab`, `dab_alpha`/`composite_dabs` semantics; `ferrolite_image::{TileCoord, TILE_SIZE, haloed_tile_extent, haloed_tile_origin, level_size}`.
- Produces:
  - `MaskBuffer::alloc_zeroed(ctx: &GpuContext, width: u32, height: u32) -> MaskBuffer` — an `R32Float` buffer initialised to `0.0`.
  - `ferrolite_mask::BrushRasterizer` with `new(ctx: Arc<GpuContext>) -> Self`.
  - `BrushRasterizer::stamp_onto(&self, base: &MaskBuffer, dabs: &[Dab], erase: bool, origin: (i32, i32), level_dims: (u32, u32)) -> MaskBuffer` — new buffer = `base` + `dabs` (same dims as `base`); `origin`/`level_dims` map output texels to normalized uv (`uv = (origin + gid + 0.5) / level_dims`).
  - `BrushRasterizer::rasterize_full(&self, dabs: &[Dab], erase: bool, width: u32, height: u32) -> MaskBuffer` — `stamp_onto` a zeroed `width×height` buffer with `origin = (0,0)`, `level_dims = (width, height)`.
  - `BrushRasterizer::rasterize_tile(&self, dabs: &[Dab], erase: bool, coord: TileCoord, halo: u32, level_dims: (u32, u32)) -> MaskBuffer` — rasterizes the `haloed_tile_extent(halo)²` region, then copies the interior `TILE_SIZE²` (offset `halo`) into the returned buffer.

- [ ] **Step 1: Re-add `ferrolite-image` to `Cargo.toml`**

In `ferrolite-mask/Cargo.toml` under `[dependencies]`, restore:

```toml
ferrolite-image = { workspace = true }
```

(Place it after `ferrolite-gpu` to match the Plan 1 ordering.)

- [ ] **Step 2: Add `MaskBuffer::alloc_zeroed`**

Append a method inside the existing `impl MaskBuffer` block in `ferrolite-mask/src/buffer.rs`:

```rust
    /// Allocate an `R32Float` mask texture initialised to `0.0` everywhere.
    pub fn alloc_zeroed(ctx: &GpuContext, width: u32, height: u32) -> Self {
        let buf = Self::alloc(ctx, width, height);
        let (w, h) = (buf.width, buf.height);
        let zeros = vec![0.0f32; (w * h) as usize];
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &buf.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&zeros),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        buf
    }
```

- [ ] **Step 3: Write the dab-stamping shader**

`ferrolite-mask/src/shaders/brush_dab.wgsl` (mirrors `dab_alpha` + `composite_dabs`):

```wgsl
// Dab-stamping rasterizer. Reads the current accumulator (in_tex), stamps a batch
// of dabs (in normalized source coords) in order, writes the new accumulator
// (out_tex). in_tex and out_tex share dims. Output texel gid maps to a level
// pixel `origin + gid`, then to normalized uv `(pixel + 0.5) / level_dims` — so a
// haloed tile (origin < 0 possible) evaluates identical uv to the whole image.
struct Dab {
    center: vec2<f32>,
    radius: f32,
    hardness: f32,
    flow: f32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
};

struct Params {
    origin: vec2<i32>,
    level_dims: vec2<u32>,
    dab_count: u32,
    erase: u32,
    pad0: u32,
    pad1: u32,
};

@group(0) @binding(0) var in_tex: texture_2d<f32>;
@group(0) @binding(1) var out_tex: texture_storage_2d<r32float, write>;
@group(0) @binding(2) var<uniform> p: Params;
@group(0) @binding(3) var<storage, read> dabs: array<Dab>;

fn dab_alpha(dist: f32, radius: f32, hardness: f32, flow: f32) -> f32 {
    if (radius <= 0.0) { return 0.0; }
    let t = dist / radius;
    let core = clamp(hardness, 0.0, 1.0);
    var ring = 0.0;
    if (t < core) {
        ring = 1.0;
    } else if (t >= 1.0) {
        ring = 0.0;
    } else {
        ring = 1.0 - smoothstep(0.0, 1.0, (t - core) / (1.0 - core));
    }
    return ring * clamp(flow, 0.0, 1.0);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(out_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }

    let px = vec2<f32>(
        f32(i32(gid.x) + p.origin.x),
        f32(i32(gid.y) + p.origin.y),
    );
    let uv = (px + vec2<f32>(0.5, 0.5))
        / vec2<f32>(f32(p.level_dims.x), f32(p.level_dims.y));

    var acc = textureLoad(in_tex, vec2<i32>(i32(gid.x), i32(gid.y)), 0).r;
    for (var i = 0u; i < p.dab_count; i = i + 1u) {
        let d = dabs[i];
        let dist = distance(uv, d.center);
        let a = dab_alpha(dist, d.radius, d.hardness, d.flow);
        if (p.erase == 1u) {
            acc = acc * (1.0 - a);
        } else {
            acc = acc + (1.0 - acc) * a;
        }
    }
    textureStore(out_tex, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(acc, 0.0, 0.0, 1.0));
}
```

- [ ] **Step 4: Write `src/brush.rs`**

```rust
//! Dab-stamping brush rasterizer. A build-once compute pass that stamps a batch
//! of `Dab`s onto an existing `MaskBuffer` (incremental, ping-pong — no
//! read-modify-write on one texture). Coverage is analytic in normalized source
//! coords (like the shape evaluators), so `rasterize_tile` with `halo = max dab
//! radius` is bit-consistent with `rasterize_full` at tile borders.

use std::sync::Arc;

use ferrolite_gpu::GpuContext;
use ferrolite_image::{haloed_tile_extent, haloed_tile_origin, TileCoord, TILE_SIZE};

use crate::buffer::{MaskBuffer, MASK_FORMAT};
use crate::stroke::Dab;

/// Storage-buffer dab record. 32 bytes, member-alignment consistent (vec2 -> 8).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuDab {
    center: [f32; 2],
    radius: f32,
    hardness: f32,
    flow: f32,
    _pad: [f32; 3],
}

impl GpuDab {
    fn from(d: &Dab) -> Self {
        Self {
            center: [d.pos.x, d.pos.y],
            radius: d.radius,
            hardness: d.hardness,
            flow: d.flow,
            _pad: [0.0; 3],
        }
    }
}

/// Uniform: haloed origin (may be negative), level dims, dab count, erase flag.
/// 32 bytes (multiple of 16).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BrushUniform {
    origin: [i32; 2],
    level_dims: [u32; 2],
    dab_count: u32,
    erase: u32,
    _pad: [u32; 2],
}

pub struct BrushRasterizer {
    ctx: Arc<GpuContext>,
    bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

impl BrushRasterizer {
    pub fn new(ctx: Arc<GpuContext>) -> Self {
        let bgl = ctx
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mask-brush-dab"),
                entries: &[
                    // 0: input accumulator (non-filterable float, textureLoad)
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    // 1: output accumulator (write storage)
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: MASK_FORMAT,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                    // 2: params uniform
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // 3: dab storage buffer (read-only)
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let module = ctx.shader_module("mask-brush-dab", include_str!("shaders/brush_dab.wgsl"));
        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mask-brush-dab"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            });
        let pipeline = ctx
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("mask-brush-dab"),
                layout: Some(&layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            });
        Self { ctx, bgl, pipeline }
    }

    /// New buffer = `base` with `dabs` stamped (same dims as `base`).
    pub fn stamp_onto(
        &self,
        base: &MaskBuffer,
        dabs: &[Dab],
        erase: bool,
        origin: (i32, i32),
        level_dims: (u32, u32),
    ) -> MaskBuffer {
        use wgpu::util::DeviceExt;
        let out = MaskBuffer::alloc(&self.ctx, base.width, base.height);
        let in_view = base
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let out_view = out
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // wgpu requires a non-empty storage binding; upload >= 1 record.
        let mut records: Vec<GpuDab> = dabs.iter().map(GpuDab::from).collect();
        if records.is_empty() {
            records.push(GpuDab::zeroed());
        }
        let dab_buf = self
            .ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mask-brush-dabs"),
                contents: bytemuck::cast_slice(&records),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let ubuf = self
            .ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mask-brush-uniform"),
                contents: bytemuck::bytes_of(&BrushUniform {
                    origin: [origin.0, origin.1],
                    level_dims: [level_dims.0, level_dims.1],
                    dab_count: dabs.len() as u32,
                    erase: u32::from(erase),
                    _pad: [0; 2],
                }),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mask-brush-dab"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&in_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&out_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: ubuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: dab_buf.as_entire_binding(),
                    },
                ],
            });
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mask-brush-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(out.width.div_ceil(8), out.height.div_ceil(8), 1);
        }
        self.ctx.queue.submit([enc.finish()]);
        out
    }

    /// Rasterize `dabs` onto a fresh zeroed `width×height` buffer (whole image).
    pub fn rasterize_full(
        &self,
        dabs: &[Dab],
        erase: bool,
        width: u32,
        height: u32,
    ) -> MaskBuffer {
        let base = MaskBuffer::alloc_zeroed(&self.ctx, width, height);
        self.stamp_onto(&base, dabs, erase, (0, 0), (width, height))
    }

    /// Rasterize the interior `TILE_SIZE²` of `coord`, evaluating the haloed
    /// region so border dabs are complete. Returns a `TILE_SIZE²` buffer.
    pub fn rasterize_tile(
        &self,
        dabs: &[Dab],
        erase: bool,
        coord: TileCoord,
        halo: u32,
        level_dims: (u32, u32),
    ) -> MaskBuffer {
        let ext = haloed_tile_extent(halo);
        let (ox, oy) = haloed_tile_origin(coord, halo);
        let base = MaskBuffer::alloc_zeroed(&self.ctx, ext, ext);
        let haloed = self.stamp_onto(
            &base,
            dabs,
            erase,
            (ox as i32, oy as i32),
            (level_dims.0, level_dims.1),
        );
        // Copy the interior TILE_SIZE² (offset `halo`) into the returned buffer.
        let interior = MaskBuffer::alloc(&self.ctx, TILE_SIZE, TILE_SIZE);
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: &haloed.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: halo,
                    y: halo,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: &interior.texture,
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
        interior
    }
}
```

No `#[cfg(test)]` unit tests in `src/brush.rs` — the rasterizer needs GPU readback to verify, which lives in the integration `tests/common` module (Step 6). The pure math it mirrors is already tested in `stroke.rs` (Tasks 1–2).

- [ ] **Step 5: Wire into `lib.rs`**

Add `mod brush;` (alphabetical: after `mod buffer;`) and re-export:

```rust
pub use brush::BrushRasterizer;
```

- [ ] **Step 6: Write a GPU smoke test that reuses `tests/common`**

`ferrolite-mask/tests/brush_smoke.rs`:

```rust
mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{BrushRasterizer, Dab, Vec2};
use std::sync::Arc;

#[test]
fn single_dab_paints_center_and_leaves_corner_empty() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let r = BrushRasterizer::new(ctx.clone());
    let dab = Dab {
        pos: Vec2::new(0.5, 0.5),
        radius: 0.25,
        hardness: 0.5,
        flow: 1.0,
    };
    let mask = r.rasterize_full(&[dab], false, 64, 64);
    let values = common::read_r32f(&ctx, &mask);
    let center = values[((64 / 2) * 64 + 64 / 2) as usize];
    assert!(center > 0.99, "center painted, got {center}");
    assert!(values[0] < 0.01, "corner empty, got {}", values[0]);
}

#[test]
fn empty_dab_batch_is_identity() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let r = BrushRasterizer::new(ctx.clone());
    // Zero dabs must not panic (>=1 storage record uploaded internally) and must
    // leave the zeroed base untouched.
    let mask = r.rasterize_full(&[], false, 32, 32);
    let values = common::read_r32f(&ctx, &mask);
    assert!(values.iter().all(|&v| v == 0.0), "empty batch is identity");
}
```

> `Dab`'s fields are public (Task 1), so the test constructs one directly. `read_r32f` is the existing helper in `tests/common/mod.rs`.

- [ ] **Step 7: Run to verify build + smoke test (skips headless)**

Run: `cargo test -p ferrolite-mask --test brush_smoke`
Expected: compiles; on the dev GPU both tests PASS (center painted / corner empty; empty batch identity); on a headless box they print the skip line and pass.

- [ ] **Step 8: fmt + clippy**

Run: `cargo fmt -p ferrolite-mask && cargo clippy -p ferrolite-mask --all-targets -- -D warnings`
Expected: no diffs, no warnings.

- [ ] **Step 9: Commit**

```bash
git add ferrolite-mask/Cargo.toml ferrolite-mask/src/buffer.rs ferrolite-mask/src/brush.rs ferrolite-mask/src/shaders/brush_dab.wgsl ferrolite-mask/src/lib.rs ferrolite-mask/tests/brush_smoke.rs
git commit -m "feat(mask): dab-stamping brush rasterizer (full + haloed-tile) + alloc_zeroed"
```

---

### Task 4: Brush rasterization golden (paint + erase)

**Files:**
- Create: `ferrolite-mask/tests/brush_golden.rs`
- Create (authored on dev GPU): `ferrolite-mask/tests/fixtures/brush_stroke.png`, `ferrolite-mask/tests/fixtures/brush_erase.png`

**Interfaces:**
- Consumes: `BrushRasterizer`, `stroke_dabs`, `SPACING_FRAC`, `Stroke`/`BrushNode`, `Vec2`; `common::{read_r32f, assert_mask_golden}`.

- [ ] **Step 1: Write the golden test**

`ferrolite-mask/tests/brush_golden.rs`:

```rust
mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{stroke_dabs, BrushNode, BrushRasterizer, Stroke, Vec2, SPACING_FRAC};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 64;

fn node(x: f32, y: f32, r: f32, hardness: f32) -> BrushNode {
    BrushNode {
        pos: Vec2::new(x, y),
        radius: r,
        hardness,
        flow: 1.0,
    }
}

#[test]
fn brush_stroke_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping golden (expected in headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let r = BrushRasterizer::new(ctx.clone());
    // A soft diagonal stroke across the frame.
    let stroke = Stroke {
        nodes: vec![
            node(0.2, 0.25, 0.12, 0.4),
            node(0.5, 0.5, 0.15, 0.4),
            node(0.8, 0.75, 0.12, 0.4),
        ],
        erase: false,
    };
    let dabs = stroke_dabs(&stroke, SPACING_FRAC);
    assert!(!dabs.is_empty());
    let mask = r.rasterize_full(&dabs, false, W, H);
    let values = common::read_r32f(&ctx, &mask);
    // Sanity: mid of the stroke is painted, a far corner is not.
    let mid = values[((H / 2) * W + W / 2) as usize];
    assert!(mid > 0.9, "stroke midpoint painted, got {mid}");
    assert!(values[0] < 0.01, "top-left corner untouched");
    common::assert_mask_golden(&values, W, H, "brush_stroke.png");
}

#[test]
fn brush_erase_carves_out_of_full_mask() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let r = BrushRasterizer::new(ctx.clone());
    // Start from a fully-painted mask (one big central dab), then erase a dot.
    let paint = stroke_dabs(
        &Stroke {
            nodes: vec![node(0.5, 0.5, 1.2, 1.0)],
            erase: false,
        },
        SPACING_FRAC,
    );
    let full = r.rasterize_full(&paint, false, W, H);
    let erase = stroke_dabs(
        &Stroke {
            nodes: vec![node(0.5, 0.5, 0.25, 0.8)],
            erase: true,
        },
        SPACING_FRAC,
    );
    let carved = r.stamp_onto(&full, &erase, true, (0, 0), (W, H));
    let values = common::read_r32f(&ctx, &carved);
    let center = values[((H / 2) * W + W / 2) as usize];
    assert!(center < 0.2, "center erased, got {center}");
    common::assert_mask_golden(&values, W, H, "brush_erase.png");
}
```

- [ ] **Step 2: Run — author the goldens on the dev GPU**

Run (dev GPU, fixtures absent so they auto-author): `cargo test -p ferrolite-mask --test brush_golden`
Expected: writes `tests/fixtures/brush_stroke.png` and `brush_erase.png`, prints `wrote golden …`, passes. On a headless box: prints the skip lines and passes without writing.

- [ ] **Step 3: Visually confirm the goldens**

Open both PNGs. `brush_stroke.png`: a soft diagonal band, brighter along the centerline, fading to black at the edges — no seams, no clipping artifacts. `brush_erase.png`: a bright field with a soft dark dot in the center. If either looks wrong, fix `dab_alpha`/spacing, delete the fixture, and re-run Step 2.

- [ ] **Step 4: Re-run to verify comparison passes**

Run: `cargo test -p ferrolite-mask --test brush_golden`
Expected: PASS by comparison (no `wrote golden` line).

- [ ] **Step 5: fmt + clippy**

Run: `cargo fmt -p ferrolite-mask && cargo clippy -p ferrolite-mask --all-targets -- -D warnings`
Expected: no diffs, no warnings.

- [ ] **Step 6: Commit (include the committed fixtures)**

```bash
git add ferrolite-mask/tests/brush_golden.rs ferrolite-mask/tests/fixtures/brush_stroke.png ferrolite-mask/tests/fixtures/brush_erase.png
git commit -m "test(mask): brush rasterization goldens (paint + erase)"
```

---

### Task 5: Tile-seam halo correctness + incremental-equals-single-shot

**Files:**
- Create: `ferrolite-mask/tests/brush_tile_seam.rs`

**Interfaces:**
- Consumes: `BrushRasterizer`, `stroke_dabs`, `max_dab_radius`, `halo_px`, `SPACING_FRAC`, `Stroke`/`BrushNode`, `Vec2`; `ferrolite_image::{TileCoord, TILE_SIZE}`; `common::{read_r32f, mask_max_abs_diff}`.

No fixtures — both tests are self-consistency proofs (tiled interiors vs whole image; incremental vs single-shot), so there is no golden to author.

- [ ] **Step 1: Write the tile-seam + incremental tests**

`ferrolite-mask/tests/brush_tile_seam.rs`:

```rust
mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_image::{TileCoord, TILE_SIZE};
use ferrolite_mask::{
    halo_px, max_dab_radius, stroke_dabs, BrushNode, BrushRasterizer, Stroke, Vec2, SPACING_FRAC,
};
use std::sync::Arc;

fn node(x: f32, y: f32, r: f32) -> BrushNode {
    BrushNode {
        pos: Vec2::new(x, y),
        radius: r,
        hardness: 0.4,
        flow: 1.0,
    }
}

// Quantize an [0,1] value the way the golden helper does, for u8 diffing.
fn q(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[test]
fn haloed_tiles_match_whole_image_at_seams() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let r = BrushRasterizer::new(ctx.clone());

    // 512x512 = exactly 2x2 tiles at lod 0 (no partial tiles).
    let dim = TILE_SIZE * 2;
    // A stroke that crosses the central seam (x=0.5) so a dab straddles the
    // tile border and MUST rasterize completely on each side.
    let stroke = Stroke {
        nodes: vec![node(0.35, 0.5, 0.06), node(0.65, 0.5, 0.06)],
        erase: false,
    };
    let dabs = stroke_dabs(&stroke, SPACING_FRAC);
    assert!(!dabs.is_empty());

    // Whole-image reference.
    let whole = r.rasterize_full(&dabs, false, dim, dim);
    let whole_vals = common::read_r32f(&ctx, &whole);

    // halo = max dab radius, in pixels at this level.
    let halo = halo_px(max_dab_radius(std::slice::from_ref(&stroke)), dim, dim);
    assert!(halo > 0, "stroke has a positive radius -> positive halo");

    // For each of the 4 tiles, rasterize the interior with halo and compare it
    // to the corresponding TILE_SIZE region of the whole image.
    for ty in 0..2u32 {
        for tx in 0..2u32 {
            let coord = TileCoord { lod: 0, x: tx, y: ty };
            let tile = r.rasterize_tile(&dabs, false, coord, halo, (dim, dim));
            let tile_vals = common::read_r32f(&ctx, &tile);
            let (ox, oy) = (tx * TILE_SIZE, ty * TILE_SIZE);
            let mut max_diff = 0u8;
            for iy in 0..TILE_SIZE {
                for ix in 0..TILE_SIZE {
                    let t = q(tile_vals[(iy * TILE_SIZE + ix) as usize]);
                    let w = q(whole_vals[((oy + iy) * dim + (ox + ix)) as usize]);
                    max_diff = max_diff.max(t.abs_diff(w));
                }
            }
            assert!(
                max_diff <= 1,
                "tile ({tx},{ty}) drifted from whole image by {max_diff} (seam/halo bug)"
            );
        }
    }
}

#[test]
fn incremental_stamping_equals_single_shot() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let r = BrushRasterizer::new(ctx.clone());
    let dim = 128;
    let stroke = Stroke {
        nodes: vec![node(0.2, 0.5, 0.08), node(0.5, 0.5, 0.08), node(0.8, 0.5, 0.08)],
        erase: false,
    };
    let dabs = stroke_dabs(&stroke, SPACING_FRAC);
    assert!(dabs.len() >= 4, "need several dabs to split");

    // Single shot.
    let whole = r.rasterize_full(&dabs, false, dim, dim);
    let whole_vals = common::read_r32f(&ctx, &whole);

    // Incremental: split the dab list and stamp in two passes (ping-pong).
    let split = dabs.len() / 2;
    let base = ferrolite_mask::MaskBuffer::alloc_zeroed(&ctx, dim, dim);
    let step1 = r.stamp_onto(&base, &dabs[..split], false, (0, 0), (dim, dim));
    let step2 = r.stamp_onto(&step1, &dabs[split..], false, (0, 0), (dim, dim));
    let inc_vals = common::read_r32f(&ctx, &step2);

    let a: Vec<u8> = whole_vals.iter().map(|&v| q(v)).collect();
    let b: Vec<u8> = inc_vals.iter().map(|&v| q(v)).collect();
    assert!(
        common::mask_max_abs_diff(&a, &b) <= 1,
        "incremental stamping diverged from single-shot"
    );
}
```

> Note: `incremental_stamping_equals_single_shot` references `ferrolite_mask::MaskBuffer::alloc_zeroed`, so `MaskBuffer` must be public (it already is, re-exported in `lib.rs`).

- [ ] **Step 2: Run (dev GPU) / skip (headless)**

Run: `cargo test -p ferrolite-mask --test brush_tile_seam`
Expected: PASS on the dev GPU (2 tests: seams match within 1 quant level, incremental == single-shot); skips cleanly headless.

- [ ] **Step 3: fmt + clippy**

Run: `cargo fmt -p ferrolite-mask && cargo clippy -p ferrolite-mask --all-targets -- -D warnings`
Expected: no diffs, no warnings.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-mask/tests/brush_tile_seam.rs
git commit -m "test(mask): tile-seam halo correctness + incremental-equals-single-shot"
```

---

## Final gate (this plan's end state)

- [ ] **Step 1: Workspace fmt**

Run: `cargo fmt --check`
Expected: no diffs.

- [ ] **Step 2: Workspace clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 3: Workspace tests**

Run: `cargo test --workspace`
Expected: green. On the dev GPU the brush goldens + tile-seam tests run; on headless CI they skip and everything else passes.

- [ ] **Step 4: Report — do NOT finish the branch**

Stop at the green gate. Report status and hand over the visual-test plan below. This is 1 of 5 plans; do NOT merge/PR/finish `feat/p1-masking-brush-vt`.

---

## Visual test plan for the author (per CLAUDE.md "Finishing a branch")

**Nothing to visually test in the running app for this plan — and why:** `ferrolite-mask` is an engine-only crate with no wiring into FerroLite's Develop module yet. `Op::LocalAdjustments`, the `LocalAdjustmentsNode`, the VT edit producer, and the Masking UI all land in Plan 3 (pipeline) and Plan 4 (UI). Nothing reachable from the running app changed; there is no panel, control, or gesture to exercise.

**Optional offline artifacts worth a glance** (authored on your dev GPU during Task 4):
- `ferrolite-mask/tests/fixtures/brush_stroke.png` — a soft diagonal painted band, seam-free, fading to black at the edges.
- `ferrolite-mask/tests/fixtures/brush_erase.png` — a bright field with a soft dark dot erased from the center.

If either looks wrong (hard edges where softness is expected, banding, clipped falloff, or visible tile seams), that indicates a `dab_alpha`/spacing/halo bug to fix before Plan 3 builds on it.

**Where the real hands-on test lands:** Plan 4 (Develop masking UI) — painting a brush mask on a loaded image, watching the red overlay update at sub-frame latency while dragging, and inspecting the result seam-free at 1:1. The tile-seam and incremental-stamp correctness proven here by `brush_tile_seam.rs` is what makes that full-res 1:1 inspection seam-free.

---

## Self-review against the spec (§4.3, §6 plan 2, §12.2, §13)

- **§4.3 dab stamping (radial falloff, hardness/flow, accumulated along polyline):** Task 1 (`stroke_dabs`) + Task 2 (`dab_alpha`/`composite_dabs`) + Task 3 (`brush_dab.wgsl`). ✓
- **§4.3 generic compute pass over a single-channel buffer:** Task 3 `BrushRasterizer` over `MaskBuffer` (R32F). ✓
- **§4.3 preview tier (a preview-res mask texture):** `rasterize_full` at preview dims serves this; the preview *scheduling* is Plan 3. Covered as a capability. ✓
- **§4.3 full-res tier streams as a generic large source through the VT halo + tile-producer seam; halo = max dab radius so a border dab rasterizes completely:** `rasterize_tile` + `halo_px`, reusing `ferrolite-image` halo math; proven by `brush_tile_seam.rs`. VT producer wiring deferred to Plan 3 (flagged). ✓
- **§4.3 incremental stamping (only new dabs since last pointer sample; no full re-raster per move):** `StrokeCursor` (Task 2) + `stamp_onto` ping-pong (Task 3); proven by `incremental_stamping_equals_single_shot` (Task 5). ✓
- **§6 two-tier recompute:** preview = `rasterize_full`; full-res = `rasterize_tile` with the only halo in the stage. ✓
- **§12 plan 2 (stroke model + dab rasterizer WGSL + incremental stamping + brush-buffer streaming through the VT seam + tile-seam golden):** all present; the tile-seam "golden" is a whole-vs-tiled equality proof (stronger than a static fixture, no fixture drift). ✓
- **§13 decisions honored:** parametric strokes are the source of truth, raster is a cache (rasterizer takes strokes/dabs, produces a re-derivable buffer); no photo concepts in `ferrolite-vt`; executor unchanged; engine-tier dependency purity (only `ferrolite-gpu`/`ferrolite-image` + permissive). ✓
- **Testing (§11): pure stroke/dab-spacing/incremental-stamp/halo math to 80%+:** Tasks 1–2 are all pure `#[cfg(test)]` unit tests (spacing, append-stability, interpolation, degenerate strokes, cursor selection, falloff, compositing, halo). Goldens auto-skip headless. ✓
- **CLAUDE.md GPU rule (build pipeline once):** `BrushRasterizer::new` builds the pipeline once; `stamp_onto` only rewrites buffers/bind groups. ✓

**Placeholder scan:** none — every code step shows complete code; no TBD/"handle edge cases"/"similar to".
**Type consistency:** `Dab`, `stroke_dabs(&Stroke, f32)`, `StrokeCursor::advance(&[Dab])`, `dab_alpha(f32,f32,f32,f32)`, `composite_dabs(&[f32],f32,bool)`, `halo_px(f32,u32,u32)`, `BrushRasterizer::{stamp_onto,rasterize_full,rasterize_tile}`, `MaskBuffer::alloc_zeroed` are used identically across tasks. ✓
