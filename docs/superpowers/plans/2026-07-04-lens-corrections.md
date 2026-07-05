# Lensfun Lens Corrections Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add opt-in distortion, transverse-CA, and vignetting corrections driven by the pure-Rust `lensfun` crate and the image's EXIF, applied on the GPU (fused into the existing geometry resample) at preview- and full-res-tiled tiers, persisted in the `.xmp` sidecar.

**Architecture:** A new `ferrolite-lens` crate wraps the pinned pre-alpha `lensfun` crate behind our own pure types (`LensMatch`, `WarpGrid`, `VignetteMap`). An off-thread `ferrolite-jobs` bake turns a matched lens into a coarse per-channel warp grid (distortion + TCA) plus a radial vignette gain map. A new `Op::LensCorrection` carries the selection + per-correction Amount. The geometric warp is **fused into the existing geometry resample** (single resample; TCA = per-channel sample); vignetting is a small scene-linear gain pass. Amount is a shader lerp uniform, so dragging it never re-bakes or rebuilds.

**Tech Stack:** Rust (edition 2021, rust-version 1.88), wgpu 22 + WGSL compute, `lensfun` crate (pure Rust, LGPL-3.0-or-later), serde/serde_json, egui/eframe 0.29, `ferrolite-jobs`.

**Design spec:** `docs/superpowers/specs/2026-07-04-lens-corrections-design.md` (read it — this plan implements it).

## Global Constraints

- **Licensing tiers (map §3.1):** all new logic is **photo tier**. `lensfun` (LGPL) lives **only** in `ferrolite-lens`. `ferrolite-gpu`/`ferrolite-vt`/`ferrolite-image` (engine tier) get **no photo concepts and no copyleft deps** — this feature must not touch them (the VT halo already exists from Spec 2). No C toolchain is introduced.
- **Responsiveness (CLAUDE.md §1):** DB load and every bake run on `ferrolite-jobs` (cancellable); nothing slow ever runs on the UI/update thread. Amount changes are uniform-only (no job, no rebuild).
- **GPU build-once (CLAUDE.md §2):** compute pipelines are built once + pre-warmed at startup; warp-grid/vignette textures are cached resources re-created only when a new bake arrives — never per frame, never per image.
- **Per-control reset (CLAUDE.md, load-bearing):** every new adjustable control (each of the three Amount sliders) ships its own reset affordance via the shared `draw_reset_arrow` / `EguiSlider` reset column. A new control is not complete without it.
- **§5 contracts:** the `Graph<PipelineImage>` executor is unchanged; corrections are `ferrolite-pipeline` nodes; the VT is reused source-agnostically; the catalog stores only the selection (a cache), never the baked grids.
- **Finishing (CLAUDE.md):** the gate is `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green — **necessary but not sufficient**. After green, STOP and hold for the author's (Jann's) hands-on visual test before finishing the branch.
- **Every commit** must leave `cargo test --workspace` green (GPU goldens auto-skip headless).
- **Branch:** all tasks land on `feat/lens-corrections` (already created off `main`).

---

## File Structure

**New crate `ferrolite-lens/`** (photo tier):
- `Cargo.toml` — pins `lensfun`, `thiserror`.
- `src/lib.rs` — re-exports; the `LensDb` trait, `load_bundled`, `lens_halo`.
- `src/types.rs` — pure data: `LensMatch`, `LensQuery`, `WarpGrid`, `VignetteMap`, `LensError`, consts.
- `src/backend.rs` — the concrete `LensfunDb` impl wrapping `lensfun` (the only file that names `lensfun`).
- `fixtures/` — a note pointing at the bundled DB; matching/bake tests use `load_bundled`.

**Modified `ferrolite-pipeline/`:**
- `src/op.rs` — add `Correction`, `LensCorrection`, `Op::LensCorrection`, `OpKind::LensCorrection`, accessor.
- `src/serialize.rs` — round-trip test for the new variant (no code change; test only).
- `src/uniforms.rs` — add `LensUniform` + `lens_halo_px`; extend `GeometryUniform` consumers.
- `src/lens_gpu.rs` (**new**) — `WarpGridTexture` + `VignetteTexture` GPU upload wrappers.
- `src/shaders/geometry.wgsl` — add warp-grid bindings + per-channel sample + Amount lerp.
- `src/shaders/vignette.wgsl` (**new**) — radial-gain point pass.
- `src/nodes.rs` — extend `GeometryHeadNode` (warp bindings); add `VignetteNode`; extend preview geometry node.
- `src/pipeline.rs` — insert the vignette node in the preview chain; thread lens params.
- `src/tile_edit.rs` — bake lens halo at construction; bind the warp grid.
- `src/lib.rs` — pre-warm the vignette pipeline; re-export new public items.

**Modified `ferrolite-app/`:**
- `src/develop/ops_edit.rs` — `set_lens_correction` + `needs_full_rebuild` extension.
- `src/develop/lens_bake.rs` (**new**) — the off-thread bake job + `LensBakeResult`.
- `src/develop/lens_match.rs` (**new**) — build `LensQuery` from `Metadata`; the shared `LensfunDb` handle.
- `src/develop/adjustment_panel.rs` — the "Lens Corrections" `CollapsingHeader` section.
- `src/develop/lens_picker.rs` (**new**) — the searchable camera+lens picker widget.
- `src/events.rs` — a `LensBaked` `AppEvent` variant.
- `src/viewer/edit_producer.rs` — accept/pass the baked warp grid + halo to the tile pipeline.
- app state wiring (`app.rs` / develop state) — hold the current bake, upload on receipt, re-bake on open.

**Modified workspace:** `Cargo.toml` — add `ferrolite-lens` member + workspace dep.

---

## Plan A — `ferrolite-lens` crate (pure, testable)

### Task 1: Scaffold `ferrolite-lens` + pin `lensfun` + pure types

**Files:**
- Create: `ferrolite-lens/Cargo.toml`, `ferrolite-lens/src/lib.rs`, `ferrolite-lens/src/types.rs`
- Modify: `Cargo.toml` (workspace members + workspace.dependencies)

**Interfaces:**
- Produces: `ferrolite_lens::{LensMatch, LensQuery, WarpGrid, VignetteMap, LensError, GRID_N, VIGNETTE_LEN}`.

- [ ] **Step 1: Add the crate to the workspace**

In `Cargo.toml`, add `"ferrolite-lens"` to `members` (line 3) and this line to `[workspace.dependencies]`:

```toml
ferrolite-lens = { path = "ferrolite-lens" }
```

- [ ] **Step 2: Pin the `lensfun` version**

Determine the exact latest `0.7.x` and pin it (no caret drift on a pre-alpha crate):

Run: `cargo search lensfun`
Copy the exact version (e.g. `0.7.0`) into the crate manifest below as `=0.7.0`.

- [ ] **Step 3: Write `ferrolite-lens/Cargo.toml`**

```toml
[package]
name = "ferrolite-lens"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
# Pinned exactly: pre-alpha, API may shift. Wrapped behind our own types in backend.rs.
lensfun = "=0.7.0"
thiserror.workspace = true

[lints]
workspace = true
```

- [ ] **Step 4: Write `ferrolite-lens/src/types.rs`**

```rust
//! Pure, GPU/UI-free data the pipeline and app consume. The only lensfun-facing
//! code is `backend.rs`; everything here is our own vocabulary so an upstream
//! `lensfun` API break is a one-file fix.

/// Warp-grid resolution (nodes per axis). Coarse; sampled bilinearly on the GPU.
pub const GRID_N: u32 = 129;
/// Radial vignette-gain LUT length.
pub const VIGNETTE_LEN: u32 = 256;

#[derive(Debug, thiserror::Error)]
pub enum LensError {
    #[error("lens database load failed: {0}")]
    DbLoad(String),
}

/// A resolved lens (from auto-match or the manual picker).
#[derive(Clone, Debug, PartialEq)]
pub struct LensMatch {
    /// Stable Lensfun lens key (the model string we persist + re-resolve on open).
    pub lens_id: String,
    /// Human label for the panel.
    pub display_name: String,
    /// Crop factor of the matched camera (from the DB), fed to the Modifier.
    pub crop_factor: f32,
}

/// EXIF-derived query used to auto-match a lens.
#[derive(Clone, Debug, PartialEq)]
pub struct LensQuery {
    pub camera_make: String,
    pub camera_model: String,
    pub lens_model: Option<String>,
    pub focal_len: f32,
    pub aperture: f32,
}

/// Coarse per-channel source-coordinate grid (normalized [0,1] image space).
/// `coords[y*n + x] = [rU,rV, gU,gV, bU,bV]` — R/G/B differ only for TCA.
#[derive(Clone, Debug, PartialEq)]
pub struct WarpGrid {
    pub n: u32,
    pub coords: Vec<[f32; 6]>,
    /// Max |source − dest| over the grid, in pixels at the baked dims → halo.
    pub max_disp: f32,
}

/// Radial vignette-correction gain: `radial[i]` is the multiplier at
/// normalized radius `i/(len-1)` from the image center.
#[derive(Clone, Debug, PartialEq)]
pub struct VignetteMap {
    pub radial: Vec<f32>,
}
```

- [ ] **Step 5: Write a minimal `ferrolite-lens/src/lib.rs`**

```rust
//! Lens-correction adapter over the pure-Rust `lensfun` crate. Photo tier.
//! Isolates the pre-alpha dependency behind our own types (`types`) and a
//! `LensDb` trait, so the pipeline/app never name `lensfun`.

mod backend;
mod types;

pub use types::{LensError, LensMatch, LensQuery, VignetteMap, WarpGrid, GRID_N, VIGNETTE_LEN};
```

- [ ] **Step 6: Add a placeholder `backend.rs` so it compiles**

```rust
//! The only module that names `lensfun`. Filled in Tasks 2–4.
#![allow(dead_code)]
```

- [ ] **Step 7: Verify the workspace builds**

Run: `cargo build -p ferrolite-lens`
Expected: PASS (downloads + compiles `lensfun`; confirms the pure-Rust dep needs no C toolchain — if it demands a C/C++ compiler, STOP and report; the spec's premise fails and we escalate to the C-bindings fallback).

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml ferrolite-lens
git commit -m "feat(lens): scaffold ferrolite-lens crate + pin lensfun + pure types"
```

---

### Task 2: `load_bundled` + `LensDb` trait + `match_lens`

**Files:**
- Modify: `ferrolite-lens/src/backend.rs`, `ferrolite-lens/src/lib.rs`
- Test: `ferrolite-lens/src/backend.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `LensQuery`, `LensMatch` (Task 1).
- Produces:
  - `pub trait LensDb { fn match_lens(&self, q: &LensQuery) -> Option<LensMatch>; fn find_lenses(&self, camera_hint: &str, needle: &str) -> Vec<LensMatch>; fn bake_geometry(&self, m: &LensMatch, focal: f32, n: u32) -> Option<WarpGrid>; fn bake_vignetting(&self, m: &LensMatch, focal: f32, aperture: f32, len: u32) -> Option<VignetteMap>; }`
  - `pub fn load_bundled() -> Result<LensfunDb, LensError>`
  - `pub struct LensfunDb` (opaque; wraps `lensfun::Database`)

- [ ] **Step 1: SPIKE — pin the real `lensfun` 0.7 API**

The crate is pre-alpha; method names/signatures must be confirmed against the pinned version before writing the wrapper. Write a throwaway test that exercises the API and run it with `--nocapture`:

```rust
#[cfg(test)]
mod spike {
    #[test]
    fn print_api() {
        let db = lensfun::Database::load_bundled().expect("bundled db");
        // Adjust these calls to the real API surface reported by `cargo doc --open`
        // for the pinned version; this test's job is to CONFIRM the exact names.
        let cams = db.find_cameras("Canon", "Canon EOS 5D Mark III");
        println!("cameras: {}", cams.len());
    }
}
```

Run: `cargo test -p ferrolite-lens spike -- --nocapture`
Record the confirmed names for: database load, camera lookup, lens lookup, crop factor accessor, `Modifier` constructor, `enable_*` methods, and the `apply_*` outputs. Use them verbatim in the steps below (the names in this plan follow the documented C++-mirroring API and may need small adjustment). Delete the spike test after.

- [ ] **Step 2: Write the failing test for matching**

Append to `backend.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LensQuery, GRID_N};

    fn db() -> LensfunDb {
        load_bundled().expect("bundled lens db loads")
    }

    #[test]
    fn matches_a_well_known_lens() {
        // A lens certain to be in the bundled DB. Adjust the strings to a lens
        // the pinned DB version actually contains (verify via the spike).
        let q = LensQuery {
            camera_make: "Canon".into(),
            camera_model: "Canon EOS 5D Mark III".into(),
            lens_model: Some("Canon EF 24-70mm f/2.8L II USM".into()),
            focal_len: 50.0,
            aperture: 8.0,
        };
        let m = db().match_lens(&q).expect("known lens matches");
        assert!(m.display_name.to_lowercase().contains("24-70"));
        assert!(m.crop_factor > 0.9 && m.crop_factor < 1.1, "full-frame ≈ 1.0");
    }

    #[test]
    fn unknown_lens_is_none() {
        let q = LensQuery {
            camera_make: "Nonexistent".into(),
            camera_model: "No Such Camera 9000".into(),
            lens_model: Some("Imaginary 999mm f/0.5".into()),
            focal_len: 50.0,
            aperture: 8.0,
        };
        assert!(db().match_lens(&q).is_none());
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p ferrolite-lens tests::matches_a_well_known_lens`
Expected: FAIL — `load_bundled`/`match_lens` not defined.

- [ ] **Step 4: Implement `LensDb` + `load_bundled` + `match_lens`**

In `backend.rs` (adjust method names to the spike's findings):

```rust
use crate::types::{LensError, LensMatch, LensQuery, VignetteMap, WarpGrid};

pub trait LensDb {
    fn match_lens(&self, q: &LensQuery) -> Option<LensMatch>;
    fn find_lenses(&self, camera_hint: &str, needle: &str) -> Vec<LensMatch>;
    fn bake_geometry(&self, m: &LensMatch, focal: f32, n: u32) -> Option<WarpGrid>;
    fn bake_vignetting(&self, m: &LensMatch, focal: f32, aperture: f32, len: u32)
        -> Option<VignetteMap>;
}

pub struct LensfunDb {
    db: lensfun::Database,
}

pub fn load_bundled() -> Result<LensfunDb, LensError> {
    let db = lensfun::Database::load_bundled().map_err(|e| LensError::DbLoad(format!("{e:?}")))?;
    Ok(LensfunDb { db })
}

impl LensfunDb {
    /// Resolve the camera (for crop factor) then the lens; returns both or None.
    fn resolve<'a>(&'a self, q: &LensQuery) -> Option<(lensfun::Lens<'a>, f32)> {
        let cam = self
            .db
            .find_cameras(&q.camera_make, &q.camera_model)
            .into_iter()
            .next()?;
        let crop = cam.crop_factor();
        let needle = q.lens_model.as_deref()?;
        let lens = self.db.find_lenses(&cam, needle).into_iter().next()?;
        Some((lens, crop))
    }
}

impl LensDb for LensfunDb {
    fn match_lens(&self, q: &LensQuery) -> Option<LensMatch> {
        let (lens, crop) = self.resolve(q)?;
        Some(LensMatch {
            lens_id: lens.model().to_string(),
            display_name: lens.model().to_string(),
            crop_factor: crop,
        })
    }

    fn find_lenses(&self, _camera_hint: &str, _needle: &str) -> Vec<LensMatch> {
        Vec::new() // Task 4
    }
    fn bake_geometry(&self, _m: &LensMatch, _focal: f32, _n: u32) -> Option<WarpGrid> {
        None // Task 3
    }
    fn bake_vignetting(&self, _m: &LensMatch, _f: f32, _a: f32, _l: u32) -> Option<VignetteMap> {
        None // Task 4
    }
}
```

Then in `lib.rs` add `pub use backend::{load_bundled, LensDb, LensfunDb};`.

- [ ] **Step 5: Run to verify both tests pass**

Run: `cargo test -p ferrolite-lens`
Expected: PASS. If the bundled DB lacks the exact lens strings, adjust the test's strings to a lens the pinned DB contains (confirmed via the spike) — do not weaken the assertions.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-lens
git commit -m "feat(lens): bundled DB load + LensDb trait + EXIF lens matching"
```

---

### Task 3: `bake_geometry` (per-channel warp grid) + `lens_halo`

**Files:**
- Modify: `ferrolite-lens/src/backend.rs`, `ferrolite-lens/src/lib.rs`
- Test: `ferrolite-lens/src/backend.rs`

**Interfaces:**
- Consumes: `LensMatch`, `WarpGrid`, `GRID_N`.
- Produces: `LensDb::bake_geometry` (real); `pub fn lens_halo(g: &WarpGrid) -> u32`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn bake_geometry_produces_grid_with_disp_for_distorting_lens() {
    let q = LensQuery {
        camera_make: "Canon".into(),
        camera_model: "Canon EOS 5D Mark III".into(),
        lens_model: Some("Canon EF 24-70mm f/2.8L II USM".into()),
        focal_len: 24.0, // wide end distorts more
        aperture: 8.0,
    };
    let db = db();
    let m = db.match_lens(&q).unwrap();
    let g = db.bake_geometry(&m, 24.0, GRID_N).expect("distortion model exists");
    assert_eq!(g.n, GRID_N);
    assert_eq!(g.coords.len() as u32, GRID_N * GRID_N);
    // The center node maps ≈ to itself; corners displace outward for barrel.
    let center = g.coords[(GRID_N * GRID_N / 2) as usize];
    assert!((center[0] - 0.5).abs() < 0.02 && (center[1] - 0.5).abs() < 0.02);
    assert!(g.max_disp > 0.0, "a distorting lens has non-zero displacement");
    // All coords finite and roughly in-bounds (bilinear edge-clamp handles the rest).
    assert!(g.coords.iter().flatten().all(|v| v.is_finite()));
}

#[test]
fn lens_halo_is_ceil_capped() {
    let g = WarpGrid { n: 2, coords: vec![[0.0; 6]; 4], max_disp: 12.3 };
    assert_eq!(crate::lens_halo(&g), 13);
    let big = WarpGrid { n: 2, coords: vec![[0.0; 6]; 4], max_disp: 9999.0 };
    assert_eq!(crate::lens_halo(&big), MAX_LENS_HALO);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-lens bake_geometry_produces_grid`
Expected: FAIL — `bake_geometry` returns `None` / `lens_halo` undefined.

- [ ] **Step 3: Implement the bake + halo**

In `backend.rs`. The `lensfun` `Modifier` fills per-pixel/per-row coordinate arrays; construct it at **grid resolution** `n×n` so we get a coarse grid cheaply, then read the remapped coords. Adjust `apply_*` to the spike's confirmed API:

```rust
/// Max halo (px) a tiled lens-corrected pass over-fetches (mirrors MAX_SHARPEN_RADIUS).
pub const MAX_LENS_HALO: u32 = 256;

fn bake_geometry_impl(lens: &lensfun::Lens, crop: f32, focal: f32, n: u32) -> Option<WarpGrid> {
    // Build a modifier at coarse grid dims; enable distortion + subpixel (TCA).
    let mut modifier = lensfun::Modifier::new(lens, focal, crop, n, n);
    let has_dist = modifier.enable_distortion_correction();
    let has_tca = modifier.enable_tca_correction();
    if !has_dist && !has_tca {
        return None; // no geometric model for this lens
    }
    // apply_subpixel_geometry_distortion fills, per output pixel, the source (u,v)
    // for R,G,B. Falls back to distortion-only (same coord in all channels) if no TCA.
    let remap = modifier.apply_subpixel_geometry_distortion(0.0, 0.0, n, n)?; // Vec of rows

    let mut coords = Vec::with_capacity((n * n) as usize);
    let mut max_disp = 0.0f32;
    let denom = (n - 1).max(1) as f32;
    for y in 0..n {
        for x in 0..n {
            // remap layout: 6 floats per pixel = (rx,ry, gx,gy, bx,by) in PIXELS.
            let px = remap_at(&remap, x, y, n); // -> [f32;6] in pixel space
            let dest = [x as f32, y as f32];
            // normalize to [0,1]
            let norm = [
                px[0] / denom, px[1] / denom, px[2] / denom,
                px[3] / denom, px[4] / denom, px[5] / denom,
            ];
            // displacement of the green channel from identity, in grid-pixel units
            let d = ((px[2] - dest[0]).powi(2) + (px[3] - dest[1]).powi(2)).sqrt();
            max_disp = max_disp.max(d);
            coords.push(norm);
        }
    }
    // max_disp is in grid-pixel units; scale to a conservative full-res halo estimate
    // as a fraction of image extent. Keep it simple: fraction * a reference extent.
    let frac = max_disp / denom; // fraction of image dimension
    let max_disp_px = frac * MAX_LENS_HALO as f32 * 4.0; // conservative; capped downstream
    Some(WarpGrid { n, coords, max_disp: max_disp_px })
}
```

> **Note for the implementer:** `remap_at` and the exact buffer layout of `apply_subpixel_geometry_distortion` come from the spike (Step 1 of Task 2). If the pinned API only exposes distortion (not subpixel), fill R=G=B from `apply_geometry_distortion` and leave TCA identity — the test above still passes (it asserts on the green channel). The `max_disp → halo` scaling is deliberately conservative and capped; refine only if the tile-seam golden (Task 10) shows seams.

Wire it into the trait impl (`bake_geometry` calls `self.resolve`-style lookup by `lens_id`; add a `lens_by_id` helper mirroring `resolve`), and add to `lib.rs`:

```rust
pub use backend::MAX_LENS_HALO;

/// Halo (pixels) a tiled lens-corrected pass must over-fetch. Ceil + capped.
pub fn lens_halo(g: &WarpGrid) -> u32 {
    (g.max_disp.ceil() as u32).min(MAX_LENS_HALO)
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ferrolite-lens`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-lens
git commit -m "feat(lens): bake per-channel warp grid + lens_halo"
```

---

### Task 4: `bake_vignetting` + `find_lenses` (picker search)

**Files:**
- Modify: `ferrolite-lens/src/backend.rs`
- Test: `ferrolite-lens/src/backend.rs`

**Interfaces:**
- Produces: `LensDb::bake_vignetting` (real), `LensDb::find_lenses` (real).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn bake_vignetting_falls_off_toward_edges() {
    let q = LensQuery {
        camera_make: "Canon".into(),
        camera_model: "Canon EOS 5D Mark III".into(),
        lens_model: Some("Canon EF 24-70mm f/2.8L II USM".into()),
        focal_len: 24.0,
        aperture: 2.8, // wide open vignettes most
    };
    let db = db();
    let m = db.match_lens(&q).unwrap();
    if let Some(v) = db.bake_vignetting(&m, 24.0, 2.8, VIGNETTE_LEN) {
        assert_eq!(v.radial.len() as u32, VIGNETTE_LEN);
        assert!(v.radial.iter().all(|g| g.is_finite() && *g > 0.0));
        // Correction gain grows toward the edge (brightens the darkened corners).
        assert!(v.radial[VIGNETTE_LEN as usize - 1] >= v.radial[0]);
    }
}

#[test]
fn find_lenses_search_returns_matches() {
    let hits = db().find_lenses("Canon", "24-70");
    assert!(hits.iter().any(|m| m.display_name.contains("24-70")));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-lens bake_vignetting`
Expected: FAIL — returns `None`/empty.

- [ ] **Step 3: Implement vignetting bake + lens search**

```rust
fn bake_vignetting_impl(
    lens: &lensfun::Lens, crop: f32, focal: f32, aperture: f32, len: u32,
) -> Option<VignetteMap> {
    // Sample the correction gain along the center→corner radius. Build a modifier
    // over a 1×len strip; enable vignetting; read the per-pixel gain.
    let mut modifier = lensfun::Modifier::new(lens, focal, crop, len, 1);
    if !modifier.enable_vignetting_correction(aperture, 1000.0) {
        return None;
    }
    let gains = modifier.apply_color_modification_row()?; // per-pixel multiplier, len entries
    let radial: Vec<f32> = gains.into_iter().filter(|g| g.is_finite()).collect();
    if radial.len() != len as usize {
        return None;
    }
    Some(VignetteMap { radial })
}
```

`find_lenses`: iterate the DB's lenses filtered by a case-insensitive substring of `needle` (and `camera_hint` if the API supports it), map to `LensMatch`. Wire both into the trait impl. Adjust `apply_color_modification_row` / lens enumeration to the spike's API.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ferrolite-lens`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-lens
git commit -m "feat(lens): bake radial vignette gain + lens search for the picker"
```

---

## Plan B — `Op::LensCorrection` model + serialization

### Task 5: Add the op variant, `OpKind` slot, accessor

**Files:**
- Modify: `ferrolite-pipeline/src/op.rs`
- Test: `ferrolite-pipeline/src/op.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `Correction { enabled: bool, amount: f32 }`, `LensCorrection { lens_id, focal_len, aperture, crop_factor, distortion, tca, vignetting }`, `Op::LensCorrection`, `OpKind::LensCorrection`, `OpStack::lens_correction()`.

- [ ] **Step 1: Write the failing test**

Add to `op.rs` tests:

```rust
#[test]
fn lens_correction_sits_before_geometry_in_canonical_order() {
    let lc = LensCorrection {
        lens_id: Some("Canon EF 24-70mm f/2.8L II USM".into()),
        focal_len: 50.0, aperture: 8.0, crop_factor: 1.0,
        distortion: Correction { enabled: true, amount: 1.0 },
        tca: Correction::default(),
        vignetting: Correction::default(),
    };
    let s = OpStack::default()
        .set_op(Op::Geometry(Geometry { crop: CropRect::full(), angle_deg: 0.0, aspect: Aspect::Original }))
        .set_op(Op::LensCorrection(lc.clone()));
    let kinds: Vec<OpKind> = s.ops.iter().map(|o| o.kind()).collect();
    assert_eq!(kinds, vec![OpKind::LensCorrection, OpKind::Geometry]);
    assert_eq!(s.lens_correction(), Some(lc));
}

#[test]
fn correction_default_is_off_at_full_amount() {
    assert_eq!(Correction::default(), Correction { enabled: false, amount: 1.0 });
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-pipeline lens_correction_sits_before_geometry`
Expected: FAIL — types undefined.

- [ ] **Step 3: Add the types + variant + accessor**

In `op.rs`, after `Sharpen` (near line 75) add:

```rust
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Correction {
    /// Whether this correction is applied at all.
    pub enabled: bool,
    /// Strength multiplier [0..], 1.0 = full DB correction. Applied as a shader lerp.
    pub amount: f32,
}

impl Default for Correction {
    fn default() -> Self {
        Self { enabled: false, amount: 1.0 }
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct LensCorrection {
    /// Resolved Lensfun lens key; None = unmatched (identity). Re-resolved on open.
    pub lens_id: Option<String>,
    /// Capture context used for the bake (EXIF; user-overridable).
    pub focal_len: f32,
    pub aperture: f32,
    pub crop_factor: f32,
    pub distortion: Correction,
    pub tca: Correction,
    pub vignetting: Correction,
}
```

Add `LensCorrection(LensCorrection)` to `enum Op` (before `Geometry`). In `enum OpKind`, renumber so LensCorrection precedes Geometry (serde uses variant **names**, so changing discriminant values does not affect persisted JSON — only the sort order):

```rust
    Sharpen = 5,
    LensCorrection = 6,
    Geometry = 7,
```

Add the `Op::kind()` arm (`Op::LensCorrection(_) => OpKind::LensCorrection`) and the accessor:

```rust
    pub fn lens_correction(&self) -> Option<LensCorrection> {
        self.ops.iter().find_map(|o| match o {
            Op::LensCorrection(l) => Some(l.clone()),
            _ => None,
        })
    }
```

Re-export `Correction` + `LensCorrection` from `ferrolite-pipeline/src/lib.rs` (find the `pub use ...op::{...}` line and add them).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS (existing `full_seven_op_stack_is_in_canonical_order` still passes — Geometry is still last).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-pipeline/src/op.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): Op::LensCorrection variant before Geometry + accessor"
```

---

### Task 6: Serialization round-trip + version tolerance

**Files:**
- Test: `ferrolite-pipeline/src/serialize.rs` (`#[cfg(test)]` only — no code change)

- [ ] **Step 1: Write the failing test**

Add to `serialize.rs` tests (extend the `use` line with `Correction, LensCorrection`):

```rust
#[test]
fn round_trips_lens_correction() {
    let s = OpStack::default().set_op(Op::LensCorrection(LensCorrection {
        lens_id: Some("Canon EF 24-70mm f/2.8L II USM".into()),
        focal_len: 35.0, aperture: 5.6, crop_factor: 1.0,
        distortion: Correction { enabled: true, amount: 0.8 },
        tca: Correction { enabled: true, amount: 1.0 },
        vignetting: Correction { enabled: false, amount: 1.0 },
    }));
    assert_eq!(deserialize(&serialize(&s)), Some(s));
}

#[test]
fn old_sidecar_without_lens_correction_still_loads() {
    // A stack written before this feature has no LensCorrection op.
    let json = r#"{"version":1,"ops":[{"Exposure":{"ev":0.5}}]}"#;
    let s = deserialize(json).unwrap();
    assert!(s.lens_correction().is_none());
    assert_eq!(s.exposure(), Some(crate::op::Exposure { ev: 0.5 }));
}
```

- [ ] **Step 2: Run to verify it passes immediately (serde is derived)**

Run: `cargo test -p ferrolite-pipeline round_trips_lens_correction old_sidecar_without_lens_correction`
Expected: PASS — this task is a **regression guard**, confirming the derived serde + version tolerance already handle the new variant additively.

- [ ] **Step 3: Commit**

```bash
git add ferrolite-pipeline/src/serialize.rs
git commit -m "test(pipeline): frl:ops round-trips LensCorrection; old sidecars still load"
```

---

## Plan C — GPU application (fused warp + vignette gain)

### Task 7: GPU resource wrappers + `LensUniform`

**Files:**
- Create: `ferrolite-pipeline/src/lens_gpu.rs`
- Modify: `ferrolite-pipeline/src/uniforms.rs`, `ferrolite-pipeline/src/lib.rs`
- Test: `ferrolite-pipeline/src/uniforms.rs`

**Interfaces:**
- Consumes: `ferrolite_lens::{WarpGrid, VignetteMap}`.
- Produces:
  - `WarpGridTexture { fn upload(ctx, &WarpGrid) -> Self; fn identity(ctx) -> Self; view()/sampler() }` (an `n×n` `Rgba32Float` texture holding R,G packed + a second for B, or an `n×n` `Rgba16Float` with rg=green-coord and a companion — see step).
  - `VignetteTexture` (a `len×1` `R32Float`).
  - `LensUniform { dist_amount: f32, tca_amount: f32, vig_amount: f32, use_warp: u32 }` (16-byte aligned).
  - `lens_halo_px(lc: Option<&LensCorrection>, grid: Option<&WarpGrid>) -> u32`.

- [ ] **Step 1: Add `ferrolite-lens` as a dep of `ferrolite-pipeline`**

In `ferrolite-pipeline/Cargo.toml` `[dependencies]` add `ferrolite-lens.workspace = true`.

- [ ] **Step 2: Write the failing test for `LensUniform` + `lens_halo_px`**

In `uniforms.rs` tests:

```rust
#[test]
fn lens_uniform_is_16_byte_aligned() {
    assert_eq!(std::mem::size_of::<LensUniform>() % 16, 0);
}

#[test]
fn lens_halo_zero_when_disabled_or_absent() {
    assert_eq!(lens_halo_px(None, None), 0);
    let lc = crate::op::LensCorrection {
        lens_id: Some("x".into()), focal_len: 24.0, aperture: 8.0, crop_factor: 1.0,
        distortion: crate::op::Correction { enabled: false, amount: 1.0 },
        tca: crate::op::Correction::default(),
        vignetting: crate::op::Correction::default(),
    };
    // Distortion disabled → no geometric halo even if a grid exists.
    let g = ferrolite_lens::WarpGrid { n: 2, coords: vec![[0.0;6];4], max_disp: 30.0 };
    assert_eq!(lens_halo_px(Some(&lc), Some(&g)), 0);
    let lc_on = crate::op::LensCorrection {
        distortion: crate::op::Correction { enabled: true, amount: 1.0 }, ..lc
    };
    assert_eq!(lens_halo_px(Some(&lc_on), Some(&g)), 30);
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p ferrolite-pipeline lens_halo_zero_when_disabled`
Expected: FAIL — `LensUniform`/`lens_halo_px` undefined.

- [ ] **Step 4: Implement `LensUniform` + `lens_halo_px`**

In `uniforms.rs`:

```rust
use ferrolite_lens::{lens_halo, WarpGrid};
use crate::op::LensCorrection;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LensUniform {
    /// Distortion/TCA/vignette lerp factors (0 when the correction is disabled).
    pub dist_amount: f32,
    pub tca_amount: f32,
    pub vig_amount: f32,
    /// 1 when a real warp grid is bound; 0 = identity (skip the grid sample).
    pub use_warp: u32,
}

/// The geometric halo (px) a tiled lens-corrected pass over-fetches. Zero unless
/// distortion or TCA is enabled AND a grid is present.
pub fn lens_halo_px(lc: Option<&LensCorrection>, grid: Option<&WarpGrid>) -> u32 {
    match (lc, grid) {
        (Some(l), Some(g)) if l.distortion.enabled || l.tca.enabled => lens_halo(g),
        _ => 0,
    }
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ferrolite-pipeline lens_halo_zero_when_disabled lens_uniform_is_16`
Expected: PASS.

- [ ] **Step 6: Implement `lens_gpu.rs` (GPU upload wrappers)**

Create `ferrolite-pipeline/src/lens_gpu.rs`. The warp grid needs R,G,B source coords per node = 6 floats. Store as **two** `n×n` textures: `rg_gb` won't fit 6 in one RGBA. Use one `Rgba32Float` `n×n` holding `[rU,rV,gU,gV]` and a second `Rg32Float` `n×n` holding `[bU,bV]`. Provide an `identity(ctx)` (1×1) default so bind groups are valid before any bake. Vignette = a `len×1` `R32Float`. Follow the texture-creation idiom in `nodes.rs::GeometryHeadNode::ensure_out` (device.create_texture + write_texture via `ctx.queue`). Expose `view()`/`sampler()` and an `upload(ctx, &WarpGrid)` / `upload(ctx, &VignetteMap)`.

Add `mod lens_gpu;` + re-exports to `lib.rs` (`pub use lens_gpu::{WarpGridTexture, VignetteTexture};`, `pub use uniforms::{LensUniform, lens_halo_px};`).

- [ ] **Step 7: Verify it builds**

Run: `cargo build -p ferrolite-pipeline`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add ferrolite-pipeline
git commit -m "feat(pipeline): LensUniform + warp-grid/vignette GPU wrappers + lens_halo_px"
```

---

### Task 8: Fuse the warp grid into the geometry resample (distortion + TCA)

**Files:**
- Modify: `ferrolite-pipeline/src/shaders/geometry.wgsl`, `ferrolite-pipeline/src/nodes.rs`, `ferrolite-pipeline/src/pipeline.rs`
- Test: golden (Task 10); this task ends with a build + a headless-safe smoke assertion.

**Interfaces:**
- Consumes: `WarpGridTexture`, `LensUniform` (Task 7).
- Produces: geometry passes that, given a bound warp grid + `LensUniform`, sample per-channel; identity when `use_warp == 0`.

- [ ] **Step 1: Extend `geometry.wgsl` with warp bindings + per-channel sample**

Add bindings 4–7 (a warp RG texture, a warp B texture, a sampler, a `LensUniform`) and replace the single sample with a per-channel path. The output-pixel → source-pixel transform (crop/rotate) is unchanged; the warp maps the **undistorted normalized coord** to the **source normalized coord** per channel:

```wgsl
@group(0) @binding(4) var warp_rg : texture_2d<f32>; // [rU,rV,gU,gV]
@group(0) @binding(5) var warp_b  : texture_2d<f32>; // [bU,bV,_,_]
@group(0) @binding(6) var warp_samp : sampler;
struct Lens { dist_amount: f32, tca_amount: f32, vig_amount: f32, use_warp: u32 };
@group(0) @binding(7) var<uniform> lens : Lens;

fn warp_uv(base_uv: vec2<f32>, chan_lo: f32) -> vec2<f32> {
    // chan_lo selects the channel pair within the warp textures via a helper below.
    // base_uv is the crop/rotate source uv in [0,1].
    if (lens.use_warp == 0u) { return base_uv; }
    let rg = textureSampleLevel(warp_rg, warp_samp, base_uv, 0.0);
    let b  = textureSampleLevel(warp_b,  warp_samp, base_uv, 0.0);
    // green = geometric distortion reference; r/b add TCA offset from green.
    let g_uv = rg.zw;
    let r_uv = mix(g_uv, rg.xy, lens.tca_amount);
    let bch  = mix(g_uv, b.xy,  lens.tca_amount);
    // distortion amount lerps green between identity (base_uv) and full (g_uv).
    let g_full = mix(base_uv, g_uv, lens.dist_amount);
    // pick by chan_lo: 0=r,1=g,2=b
    if (chan_lo < 0.5) { return mix(base_uv, r_uv, lens.dist_amount); }
    if (chan_lo < 1.5) { return g_full; }
    return mix(base_uv, bch, lens.dist_amount);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ow = u32(p.out_dims.x); let oh = u32(p.out_dims.y);
    if (gid.x >= ow || gid.y >= oh) { return; }
    let po = p.out_origin + vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5);
    let sx = p.m.x * po.x + p.m.y * po.y + p.off.x;
    let sy = p.m.z * po.x + p.m.w * po.y + p.off.y;
    let base_uv = vec2<f32>(sx, sy) / p.src_dims;
    let r = textureSampleLevel(src, samp, warp_uv(base_uv, 0.0), 0.0).r;
    let g = textureSampleLevel(src, samp, warp_uv(base_uv, 1.0), 0.0).g;
    let b = textureSampleLevel(src, samp, warp_uv(base_uv, 2.0), 0.0).b;
    let a = textureSampleLevel(src, samp, warp_uv(base_uv, 1.0), 0.0).a;
    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(r, g, b, a));
}
```

> Note: when `use_warp == 0` (no lens correction), `warp_uv` returns `base_uv` for every channel → the three samples collapse to the original single-sample behavior (byte-identical result). This is the regression guarantee tested in Task 10.

- [ ] **Step 2: Extend the geometry bind-group layout + both geometry nodes**

In `nodes.rs`, the shared `geometry_bgl` gains bindings 4–7. `GeometryHeadNode` (and the preview geometry node in `pipeline.rs`) must own a `WarpGridTexture` (defaulting to `identity`) + a `LensUniform` buffer + the warp sampler, and add the four `BindGroupEntry`s. Add setters: `set_warp(&WarpGridTexture)` and `set_lens_uniform(LensUniform)` that write the buffer (no pipeline rebuild). Default `LensUniform { use_warp: 0, .. }` so an un-corrected image is unchanged.

- [ ] **Step 3: Build + run the full pipeline test suite**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS (existing geometry goldens unchanged because `use_warp` defaults to 0). If a geometry golden shifts, the identity path regressed — fix before proceeding.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-pipeline/src/shaders/geometry.wgsl ferrolite-pipeline/src/nodes.rs ferrolite-pipeline/src/pipeline.rs
git commit -m "feat(pipeline): fuse per-channel warp-grid sample into geometry resample"
```

---

### Task 9: Vignetting gain pass

**Files:**
- Create: `ferrolite-pipeline/src/shaders/vignette.wgsl`
- Modify: `ferrolite-pipeline/src/nodes.rs`, `ferrolite-pipeline/src/pipeline.rs`, `ferrolite-pipeline/src/lib.rs`
- Test: golden (Task 10) + build.

**Interfaces:**
- Consumes: `VignetteTexture`, `LensUniform.vig_amount` (Task 7).
- Produces: a `VignetteNode` (point op) inserted near the head of the preview chain; identity when `vig_amount == 0`.

- [ ] **Step 1: Write `vignette.wgsl`**

A point pass: for each pixel, compute normalized radius from the image center, sample the radial gain LUT, multiply `rgb` by `mix(1.0, gain, vig_amount)`:

```wgsl
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct V { vig_amount: f32, _pad: vec3<f32> };
@group(0) @binding(2) var<uniform> v: V;
@group(0) @binding(3) var lut: texture_2d<f32>;   // len×1, R = gain
@group(0) @binding(4) var lut_samp: sampler;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5) / vec2<f32>(f32(dims.x), f32(dims.y));
    let d = uv - vec2<f32>(0.5, 0.5);
    let r = length(d) / length(vec2<f32>(0.5, 0.5)); // 0 center → 1 corner
    let gain = textureSampleLevel(lut, lut_samp, vec2<f32>(r, 0.5), 0.0).r;
    let c = textureLoad(src, vec2<i32>(i32(gid.x), i32(gid.y)), 0);
    let g = mix(1.0, gain, v.vig_amount);
    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(c.rgb * g, c.a));
}
```

- [ ] **Step 2: Add `VignetteNode` + insert it near the head**

In `nodes.rs`, add a `VignetteNode` mirroring an existing point-op node (bind layout: src, dst, uniform, lut, sampler), owning a `VignetteTexture` (default identity 1×1 gain=1) + a small uniform. In `pipeline.rs`, insert it in the preview node chain **right after `SourceNode`/color_matrix and before exposure** (scene-linear), per spec §6.2. For the tiled producer, add the same pass after the geometry head (it is point-wise; order within the per-tile color chain is fine as long as it is in scene-linear before exposure). Add `set_vignette(&VignetteTexture)` + `set_vig_amount(f32)` setters (buffer writes, no rebuild). Default `vig_amount = 0.0` → identity.

- [ ] **Step 3: Pre-warm the vignette pipeline**

In `lib.rs` prewarm hook (the list that precompiles the 9 shaders at startup), add `vignette`. Confirm the count is updated.

- [ ] **Step 4: Build + test**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS (vignette defaults to identity → existing goldens unchanged).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-pipeline/src/shaders/vignette.wgsl ferrolite-pipeline/src/nodes.rs ferrolite-pipeline/src/pipeline.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): vignetting radial-gain pass (scene-linear, identity by default)"
```

---

### Task 10: Halo wiring in the tile producer + GPU goldens

**Files:**
- Modify: `ferrolite-pipeline/src/tile_edit.rs`, `ferrolite-app/src/develop/ops_edit.rs`, `ferrolite-app/src/viewer/edit_producer.rs`
- Test: `ferrolite-pipeline/tests/` (golden) + `ops_edit.rs` unit test

**Interfaces:**
- Consumes: `lens_halo_px` (Task 7), `WarpGridTexture`/`VignetteTexture` setters (Tasks 8–9).
- Produces: `TileEditPipeline` bakes `halo = max(sharpen_halo, lens_halo_px)` at construction and binds the warp grid; `needs_full_rebuild` also triggers on lens geometry change.

- [ ] **Step 1: Extend `needs_full_rebuild` (failing test first)**

In `ops_edit.rs` tests:

```rust
#[test]
fn needs_full_rebuild_on_lens_enable_and_lens_change() {
    let base = OpStack::default();
    let lc = |dist_on: bool, id: &str| ferrolite_pipeline::LensCorrection {
        lens_id: Some(id.into()), focal_len: 24.0, aperture: 8.0, crop_factor: 1.0,
        distortion: ferrolite_pipeline::Correction { enabled: dist_on, amount: 1.0 },
        tca: ferrolite_pipeline::Correction::default(),
        vignetting: ferrolite_pipeline::Correction::default(),
    };
    let on = base.set_op(Op::LensCorrection(lc(true, "A")));
    assert!(needs_full_rebuild(&base, &on), "enabling distortion changes the halo");
    // Amount-only change must NOT rebuild:
    let mut lc2 = on.lens_correction().unwrap();
    lc2.distortion.amount = 0.5;
    let amt = on.set_op(Op::LensCorrection(lc2));
    assert!(!needs_full_rebuild(&on, &amt), "Amount is uniform-only");
    // Different lens id → rebuild (new grid + halo):
    let other = base.set_op(Op::LensCorrection(lc(true, "B")));
    assert!(needs_full_rebuild(&on, &other));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-app needs_full_rebuild_on_lens_enable`
Expected: FAIL.

- [ ] **Step 3: Implement the `needs_full_rebuild` extension**

The halo depends on lens_id + which geometric corrections are enabled (not on Amount). Add a pure helper describing the rebuild-relevant lens key:

```rust
fn lens_rebuild_key(s: &OpStack) -> (Option<String>, bool, bool, f32, f32, f32) {
    match s.lens_correction() {
        Some(l) => (l.lens_id, l.distortion.enabled, l.tca.enabled, l.focal_len, l.aperture, l.crop_factor),
        None => (None, false, false, 0.0, 0.0, 0.0),
    }
}

pub fn needs_full_rebuild(old: &OpStack, new: &OpStack) -> bool {
    old.geometry() != new.geometry()
        || sharpen_halo(old.sharpen()) != sharpen_halo(new.sharpen())
        || lens_rebuild_key(old) != lens_rebuild_key(new)
}
```

(Note: `focal_len`/`aperture`/`crop_factor` are in the key because they change the baked grid, hence the halo.)

- [ ] **Step 4: Bake the lens halo + bind the grid in `TileEditPipeline`**

In `tile_edit.rs`, where `let halo = sharpen_halo(stack.sharpen());` is computed, change to:

```rust
let halo = sharpen_halo(stack.sharpen()).max(lens_halo_px(stack.lens_correction().as_ref(), warp_grid));
```

where `warp_grid: Option<&WarpGrid>` is a new construction parameter threaded from the app's current bake (Task 11). Bind the `WarpGridTexture`/`VignetteTexture` (or their identity defaults) into the geometry head + vignette node, and set the `LensUniform` from the op's amounts + `use_warp`. `edit_producer.rs` passes the current bake products through.

- [ ] **Step 5: Author the GPU goldens**

Add to `ferrolite-pipeline/tests/` (mirroring the existing golden harness; auto-skip when `GpuContext::headless()` is `None`):
1. **corrections-off ≡ geometry-only** — a stack with `LensCorrection { all disabled }` renders byte-identical to the same stack without the op (the identity guarantee).
2. **fixture-lens corrected render** — a small synthetic source + a hand-built `WarpGrid` (barrel) + `VignetteMap`, distortion+TCA+vignette enabled at amount 1.0, vs a committed reference authored on the dev GPU.
3. **tile-seam** — the corrected image via the haloed per-tile producer matches the whole-image result within tolerance at tile borders (halo-correctness proof; mirrors the Spec 2 sharpen tile-seam golden).

Author/verify the reference images locally on the dev GPU; commit them.

- [ ] **Step 6: Run tests**

Run: `cargo test -p ferrolite-app -p ferrolite-pipeline`
Expected: PASS (goldens run on the dev GPU; auto-skip headless).

- [ ] **Step 7: Commit**

```bash
git add ferrolite-pipeline ferrolite-app/src/develop/ops_edit.rs ferrolite-app/src/viewer/edit_producer.rs
git commit -m "feat(pipeline): bake lens halo into the tile producer + corrected/tile-seam goldens"
```

---

## Plan D — App: match, off-thread bake, UI, persistence

### Task 11: Shared DB handle, `LensQuery` from `Metadata`, off-thread bake job

**Files:**
- Create: `ferrolite-app/src/develop/lens_match.rs`, `ferrolite-app/src/develop/lens_bake.rs`
- Modify: `ferrolite-app/src/events.rs`, `ferrolite-app/Cargo.toml`, develop state/`app.rs`
- Test: `lens_match.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `ferrolite_lens::{LensDb, load_bundled, LensQuery, WarpGrid, VignetteMap}`, `ferrolite_decode::Metadata`, `LensCorrection`.
- Produces:
  - `pub fn query_from_metadata(m: &Metadata) -> Option<LensQuery>`
  - `pub struct LensBakeResult { pub warp: Option<WarpGrid>, pub vignette: Option<VignetteMap>, pub resolved_name: Option<String> }`
  - `AppEvent::LensBaked { image_id: i64, result: LensBakeResult }`
  - `pub fn spawn_lens_bake(jobs, db, tx, ctx, image_id, lc: LensCorrection)`

- [ ] **Step 1: Add the dep**

`ferrolite-app/Cargo.toml` `[dependencies]`: `ferrolite-lens.workspace = true`.

- [ ] **Step 2: Write the failing test for `query_from_metadata`**

`lens_match.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_decode::Metadata;
    use ferrolite_image::Orientation;

    fn meta(lens: Option<&str>, focal: Option<f32>, ap: Option<f32>) -> Metadata {
        Metadata {
            make: "Canon".into(), model: "Canon EOS 5D Mark III".into(),
            width: 5760, height: 3840, orientation: Orientation::Normal,
            iso: Some(100), aperture: ap, shutter: Some(0.01), focal_length: focal,
            capture_time: None, lens: lens.map(String::from),
        }
    }

    #[test]
    fn builds_query_when_focal_and_aperture_present() {
        let q = query_from_metadata(&meta(Some("EF 24-70"), Some(50.0), Some(8.0))).unwrap();
        assert_eq!(q.camera_make, "Canon");
        assert_eq!(q.focal_len, 50.0);
        assert_eq!(q.lens_model.as_deref(), Some("EF 24-70"));
    }

    #[test]
    fn none_without_focal_length() {
        // Focal length is required to build the correction model.
        assert!(query_from_metadata(&meta(Some("EF 24-70"), None, Some(8.0))).is_none());
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p ferrolite-app query_from_metadata builds_query`
Expected: FAIL.

- [ ] **Step 4: Implement `lens_match.rs`**

```rust
//! Build a lens-match query from decode metadata + hold the shared DB handle.
use ferrolite_decode::Metadata;
use ferrolite_lens::LensQuery;

/// A query needs at least camera make/model + a focal length (the correction
/// model is focal-dependent). Aperture defaults to a mid value when absent
/// (vignetting only; distortion/TCA don't use it).
pub fn query_from_metadata(m: &Metadata) -> Option<LensQuery> {
    let focal = m.focal_length?;
    Some(LensQuery {
        camera_make: m.make.clone(),
        camera_model: m.model.clone(),
        lens_model: m.lens.clone(),
        focal_len: focal,
        aperture: m.aperture.unwrap_or(8.0),
    })
}
```

The shared `LensfunDb` handle: load once via `ferrolite_lens::load_bundled()` behind a `OnceCell`/stored in app state; on failure, log and disable the section (store `None`). Wrap in `Arc` for the bake job.

- [ ] **Step 5: Implement `lens_bake.rs` (off-thread, mirrors `ops_persist.rs`)**

```rust
//! Off-thread lens bake: DB → warp grid + vignette map. Never on the UI thread.
use crate::events::AppEvent;
use ferrolite_jobs::{JobSystem, Priority};
use ferrolite_lens::{LensDb, LensfunDb};
use ferrolite_pipeline::LensCorrection;
use std::sync::mpsc::Sender;
use std::sync::Arc;

pub struct LensBakeResult {
    pub warp: Option<ferrolite_lens::WarpGrid>,
    pub vignette: Option<ferrolite_lens::VignetteMap>,
    pub resolved_name: Option<String>,
}

pub fn spawn_lens_bake(
    jobs: &Arc<JobSystem>,
    db: &Arc<LensfunDb>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    lc: LensCorrection,
) {
    let db = Arc::clone(db);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Visible, move |cancel| {
        if cancel.is_cancelled() { return; }
        let result = match lc.lens_id.as_deref().and_then(|id| db.match_by_id(id)) {
            Some(m) => LensBakeResult {
                warp: if lc.distortion.enabled || lc.tca.enabled {
                    db.bake_geometry(&m, lc.focal_len, ferrolite_lens::GRID_N)
                } else { None },
                vignette: if lc.vignetting.enabled {
                    db.bake_vignetting(&m, lc.focal_len, lc.aperture, ferrolite_lens::VIGNETTE_LEN)
                } else { None },
                resolved_name: Some(m.display_name),
            },
            None => LensBakeResult { warp: None, vignette: None, resolved_name: None },
        };
        let _ = tx.send(AppEvent::LensBaked { image_id, result });
        ctx.request_repaint();
    });
}
```

(Add a `match_by_id(&self, lens_id: &str) -> Option<LensMatch>` to `LensDb`/`LensfunDb` in `ferrolite-lens` — a lookup by the persisted key, mirroring `find_lenses`. Add its unit test in Task 2's module.)

Add `AppEvent::LensBaked { image_id: i64, result: crate::develop::lens_bake::LensBakeResult }` to `events.rs`.

- [ ] **Step 6: Handle the event — upload + rebuild**

In the app event loop (where `OpsLoaded`/`OpsSaved` are handled), on `LensBaked` for the current image: build `WarpGridTexture`/`VignetteTexture` from the result (or identity when `None`), store them, set the current-image lens uniform, and trigger a tile-producer rebuild (reuse the `needs_full_rebuild` path). Guard on `image_id == current` (a superseded bake for a navigated-away image is dropped).

- [ ] **Step 7: Test + build**

Run: `cargo test -p ferrolite-app`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add ferrolite-app Cargo.toml
git commit -m "feat(app): shared lens DB + LensQuery-from-metadata + off-thread bake job"
```

---

### Task 12: The "Lens Corrections" panel section (toggles + Amount + per-control reset)

**Files:**
- Modify: `ferrolite-app/src/develop/adjustment_panel.rs`, `ferrolite-app/src/develop/ops_edit.rs`
- Test: `ops_edit.rs` (`set_lens_correction`)

**Interfaces:**
- Consumes: `LensCorrection`, `Correction`, the `EguiSlider` + `draw_reset_arrow` widgets, `set_lens_correction`.
- Produces: `set_lens_correction(&OpStack, LensCorrection) -> OpStack` (removes the op when fully disabled + no lens).

- [ ] **Step 1: Write the failing test for `set_lens_correction`**

In `ops_edit.rs` tests:

```rust
#[test]
fn set_lens_correction_removes_when_unmatched_and_all_off() {
    use ferrolite_pipeline::{Correction, LensCorrection};
    let off = LensCorrection {
        lens_id: None, focal_len: 24.0, aperture: 8.0, crop_factor: 1.0,
        distortion: Correction::default(), tca: Correction::default(), vignetting: Correction::default(),
    };
    let s = set_lens_correction(&OpStack::default(), off);
    assert!(s.lens_correction().is_none(), "no lens + all off = identity");

    let on = LensCorrection {
        lens_id: Some("EF 24-70".into()),
        distortion: Correction { enabled: true, amount: 1.0 },
        ..off
    };
    let s2 = set_lens_correction(&OpStack::default(), on);
    assert!(s2.lens_correction().is_some());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-app set_lens_correction_removes`
Expected: FAIL.

- [ ] **Step 3: Implement `set_lens_correction`**

```rust
use ferrolite_pipeline::LensCorrection;

/// A LensCorrection with no lens AND every correction disabled is identity → remove it.
pub fn set_lens_correction(s: &OpStack, lc: LensCorrection) -> OpStack {
    let identity = lc.lens_id.is_none()
        && !lc.distortion.enabled && !lc.tca.enabled && !lc.vignetting.enabled;
    if identity {
        s.reset(ferrolite_pipeline::OpKind::LensCorrection)
    } else {
        s.set_op(ferrolite_pipeline::Op::LensCorrection(lc))
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ferrolite-app set_lens_correction_removes`
Expected: PASS.

- [ ] **Step 5: Add the panel section**

In `adjustment_panel.rs`, add a `CollapsingHeader("Lens Corrections")` after the Detail section (mirror the existing Basic/Detail sections' structure). It reads the current `LensCorrection` (or a default seeded from the matched lens + EXIF focal/aperture) and renders:
- A **matched-lens label** (`resolved_name` or "No lens matched"); a "Choose lens…" button opening the picker (Task 13).
- Three rows — Distortion / Transverse CA / Vignetting — each: a `Checkbox` (toggle `enabled`) + an `EguiSlider { label: "Amount", value: &mut amount, min: 0.0, max: 2.0, .. }` **with its reset column** (the shared `EguiSlider` reset arrow → resets amount to `1.0`; use the same pattern as existing sliders so `draw_reset_arrow` is wired). Disable the Amount slider when the toggle is off.
- An advanced sub-area (collapsed) with editable focal length + aperture.
- A **section reset** button clearing the whole `LensCorrection` op (reuse the section-reset pattern).

On any change: emit `set_lens_correction(&stack, new_lc)` as the `EditOutcome` (mirrors how Basic emits `set_exposure(...)`), and — for a change other than Amount — schedule the bake (Task 11) via the develop state's callback. Amount-only changes update the `LensUniform` directly (no bake).

- [ ] **Step 6: Build + clippy**

Run: `cargo build -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: PASS. (egui rendering is verified by the author's visual test, not automated.)

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/develop/adjustment_panel.rs ferrolite-app/src/develop/ops_edit.rs
git commit -m "feat(app): Lens Corrections panel section (toggles + Amount + per-control reset)"
```

---

### Task 13: Searchable camera + lens picker

**Files:**
- Create: `ferrolite-app/src/develop/lens_picker.rs`
- Modify: `ferrolite-app/src/develop/adjustment_panel.rs`, develop state

**Interfaces:**
- Consumes: `LensDb::find_lenses`, `LensMatch`.
- Produces: a modal/popup that returns a chosen `LensMatch` → sets `lens_id`/`crop_factor` on the op and schedules a bake.

- [ ] **Step 1: Implement the picker widget**

`lens_picker.rs`: a popup with a search `TextEdit` (the needle) + a scrollable result list from `db.find_lenses(camera_hint, needle)` (camera_hint = current image's make/model). Selecting a row returns its `LensMatch`. Keep the list virtualized/capped (e.g. first 200 hits) and note the cap in a label if truncated (CLAUDE.md: no silent caps). `find_lenses` runs on the DB handle synchronously (it is an in-memory string filter — cheap; if profiling shows otherwise, move to a job, but a substring filter over the DB is trivial).

- [ ] **Step 2: Wire it into the panel**

The "Choose lens…" button (Task 12) opens the picker; on selection, set `lens_id`/`crop_factor`/`display name`, emit `set_lens_correction`, and schedule a bake. A "clear" affordance sets `lens_id = None` (→ identity if all off).

- [ ] **Step 3: Build + clippy**

Run: `cargo build -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/develop/lens_picker.rs ferrolite-app/src/develop/adjustment_panel.rs
git commit -m "feat(app): searchable camera+lens picker for manual override"
```

---

### Task 14: Match-on-open + persistence re-bake wiring

**Files:**
- Modify: develop open path (`app.rs` / develop state), the `OpsLoaded` handler
- Test: manual (visual) + existing round-trip tests cover persistence

**Interfaces:**
- Consumes: `query_from_metadata`, `match_lens`, `spawn_lens_bake`, the `OpsLoaded` event.

- [ ] **Step 1: Auto-match on open**

When an image opens in Develop and its `Metadata` is available: if the loaded `OpStack` has **no** `LensCorrection` op, run `query_from_metadata` → `db.match_lens` (cheap, in-memory; on the UI thread is acceptable, or fold into the metadata job). Store the candidate `resolved_name` + `lens_id` + `crop_factor` for the panel's default seed — **without** adding an op (opt-in: nothing is enabled, so no op is created and `has_edits` stays false).

- [ ] **Step 2: Re-bake on open when a persisted correction exists**

In the `OpsLoaded` handler: if the loaded stack has a `LensCorrection` with `lens_id.is_some()` and any correction enabled, call `spawn_lens_bake` so the warp/vignette textures are rebuilt from the persisted selection (the grids are not persisted — spec §7.4). Until the bake returns, the `LensUniform` stays `use_warp = 0` (identity) so nothing is shown wrong.

- [ ] **Step 3: Verify persistence round-trip end-to-end (headless-safe parts)**

Run: `cargo test --workspace`
Expected: PASS. (The op already round-trips via Task 6; this step confirms the wiring compiles and no pure tests regressed.)

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app
git commit -m "feat(app): auto-match lens on open + re-bake persisted corrections"
```

---

## Final gate (before finishing the branch)

- [ ] **Run the full workspace gate:**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all green. GPU goldens run on the dev GPU and auto-skip headless.

- [ ] **STOP and hold for the author's hands-on visual test** (CLAUDE.md "Finishing a branch" rule). Open real RAW files with known lenses; confirm: auto-match shows the right lens name; enabling each correction visibly corrects (barrel/pincushion straightens, corners brighten, colored fringing at edges reduces); Amount scales smoothly with instant response (no re-bake stutter); 1:1 zoom shows no tile seams; corrections persist across reopen; unmatched lenses degrade gracefully. Address any issues, then present finish options.

---

## Self-Review (author's checklist, completed)

**Spec coverage:** §2 crates → Plans A (ferrolite-lens), B (op+serialize), C (GPU), D (app) all present. §4 adapter → Tasks 1–4. §5 op/order → Task 5. §6 fused warp + vignetting + halo + Amount-as-uniform → Tasks 7–10. §7 UI + bake + persistence → Tasks 11–14. §8 error handling → identity defaults + `Option` bakes throughout + graceful DB-load failure (Task 11) + `use_warp=0` until bake (Task 14). §9 tests → pure tests each task + GPU goldens (Task 10). §10 contracts → Global Constraints + each task honors them (bake is a job; catalog stores only the op; executor untouched; VT reused; engine crates not touched). §11 decisions → reflected. §12 decomposition → this plan's four plans mirror it.

**Placeholder scan:** No "TODO/TBD". The two SPIKE steps (Task 2 Step 1, and API-name adjustments) are explicit, bounded investigations of a pre-alpha dependency — not deferred work — and every dependent step names the exact fallback if the API differs (distortion-only if no subpixel; C-bindings escalation if a C toolchain is required).

**Type consistency:** `LensCorrection`/`Correction` fields, `WarpGrid`/`VignetteMap` shapes, `LensUniform` fields, `lens_halo`/`lens_halo_px`, `set_lens_correction`, `needs_full_rebuild`, `spawn_lens_bake`, `AppEvent::LensBaked`, `query_from_metadata` are named identically across the tasks that define and consume them. `match_by_id` is introduced in Task 11 and back-filled into `ferrolite-lens` (noted inline).

**Deviations from the spec (intentional, minor, more correct):** `crop_factor` is sourced from the matched Lensfun camera (Task 2) rather than added to decode `Metadata` (the spec §2 listed a `Metadata` field; rawler does not expose crop factor, and the DB carries it) — the decode change is therefore dropped and `LensQuery` is built from existing fields. Recorded here so the deviation is explicit.
