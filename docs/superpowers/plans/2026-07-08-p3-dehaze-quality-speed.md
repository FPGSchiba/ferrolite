# Dehaze Quality + Speed — Guided-Filter Refinement & Separable Min Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Eliminate the Dark Channel Prior halo/blocky artifacts (guided-filter transmission refinement) and make the Dehaze slider responsive (separable O(r) min-filter + a two-node split so an amount drag re-runs only a single cheap recovery pass), on the existing `feat/p3-dehaze` branch.

**Architecture:** Replace the single O(r²) dehaze `PointOpNode` with **two retained-graph nodes**: `DehazeTransmissionNode` (image → refined transmission map: per-pixel dark channel → **separable** box-min over the patch radius → **guided-filter** edge-aware refinement) and `DehazeRecoveryNode` (image + transmission → recovered image: `J=(I−A)/max(t,t₀)+A`, blended by `amount`). Because the transmission does not depend on `amount`, the `Graph`'s existing dirty-propagation caches it: an **amount** drag dirties only the recovery node (one cheap pass); the transmission recomputes only when the upstream image, the `radius`, or `A` changes. Guided filter and separable min are pure math (He et al.), no new dependencies.

**Tech Stack:** Rust, `wgpu` + WGSL compute, `bytemuck` Pod uniforms. Photo tier (`ferrolite-pipeline`) + minimal app/export follow-through.

## Global Constraints

_Binding requirements — every task's requirements implicitly include this section._

- **Two problems, one restructure (author-approved, visual-test driven):**
  1. **Quality:** the current single-pass patch-min DCP produces bright halos and rectangular fringes around dark objects (the square patch *dilates* dark regions by the radius; the transmission is coarse and not edge-aware). Fix = **guided-filter refinement** of the transmission map (He et al. 2013), guide = the input image's luma. This was deferred as a non-goal in the P3 spec §5.2; the author has explicitly approved adding it now.
  2. **Speed:** the naïve `(2r+1)²`-tap min-filter re-run over the whole preview every amount-drag frame is the lag. Fix = **separable min** (O(2r), horizontal then vertical) **plus** the two-node split so an amount drag re-runs only the recovery pass (transmission is cached by the graph).
- **No behavior/UX change to the op model or UI:** `Dehaze { amount, radius }`, the Effects tab (Dehaze + Radius sliders), `set_dehaze`, per-control reset, `OpKind::Dehaze` position — all UNCHANGED. This plan changes only *how* the dehaze node computes, not the op or its controls.
- **Reusable-math (§2.5):** the per-pixel recovery stays the pure `dehaze_recover(px, dark, a, amount)` (unchanged). Add a pure CPU `transmission_map(...)` reference (block-min + guided filter) so the GPU passes are golden-verified against it. No transform logic may live only in a shader.
- **Contracts:** `ferrolite-gpu`'s generic `Graph<PipelineImage>` executor is NOT modified — the two dehaze nodes are supplied by `ferrolite-pipeline` (contract 4). Dehaze remains a halo consumer on the source-agnostic VT (contract 5); the halo is now `radius + 2·guided_radius`. `A` estimate stays the cached, once-per-image whole-image estimate (unchanged from the base feature). No engine-tier edits, no copyleft, no new deps (guided filter = box filters + arithmetic).
- **Responsiveness / build-once GPU (CLAUDE.md load-bearing):** every compute pipeline built ONCE per node in `new` and reused; add the new passes to `prewarm_shaders`. No per-frame CPU work. The amount-drag path must NOT recompute the transmission (verified by a rebuild-count test hook, like `local_rebuild_count`).
- **Tiling parity:** the tiled full-res render must match the whole-image render within tolerance across tile seams, with the enlarged halo. The existing strengthened parity golden (`dehaze_tiled_matches_whole_image`, sawtooth-in-R fixture) must keep passing; extend its radius so the guided-filter halo is exercised.
- **Rust:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean; `#[repr(C)]` uniform field order MIRRORS each WGSL `struct P`.

**Branch:** continue on `feat/p3-dehaze` (base feature already implemented + reviewed).

**Workspace gate (after every task; must stay green except the 5 pre-existing `ferrolite-decode` fixture failures, which this branch never touches):**
```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## Algorithm reference (the spec every task implements)

Let `I` = the input RGB image (post-Contrast, display-linear), `A` = the whole-image atmospheric light `[f32;3]` (already estimated + cached), `r` = patch radius (`Dehaze::radius`), `gr` = guided radius, `ε` = guided regularization.

1. **Normalized dark channel** (per pixel): `dc0(p) = min(I_r/A_r, I_g/A_g, I_b/A_b)`.
2. **Block min** (the neighbourhood/halo step), computed **separably**: `dcH(p) = min over dx∈[-r,r] of dc0(p+dx)`, then `dc(p) = min over dy∈[-r,r] of dcH(p+dy)`. `dc` = the min over the `(2r+1)²` patch, in O(2r) not O(r²).
3. **Raw transmission:** `praw(p) = clamp(1 − ω·dc(p), 0, 1)` (`ω = DEHAZE_OMEGA = 0.95`).
4. **Guide:** `g(p) = luma(I(p))` (Rec.709: `0.2126R+0.7152G+0.0722B`), used as the single-channel guided-filter guide.
5. **Guided-filter refinement** (He et al. 2013), window radius `gr`, regularization `ε`:
   - `mean_g = box(g)`, `mean_p = box(praw)`
   - `corr_g = box(g·g)`, `corr_gp = box(g·praw)`
   - `var_g = corr_g − mean_g²`, `cov_gp = corr_gp − mean_g·mean_p`
   - `a = cov_gp / (var_g + ε)`, `b = mean_p − a·mean_g`
   - `mean_a = box(a)`, `mean_b = box(b)`
   - `q(p) = mean_a·g(p) + mean_b`  ← the **refined transmission** (edge-aware; no dilation halo)
   where `box(x)` is a normalized box average of radius `gr`, computed **separably** (H then V).
6. **Recovery + blend** (per pixel; the existing `dehaze_recover`, but `dark` is now derived from the refined `q`): with `t = q(p)` clamped, `te = max(t, t₀)` (`t₀ = 0.1`):
   - remove-haze `J_c = (I_c − A_c)/te + A_c`
   - add-haze `hazed_c = A_c + (I_c − A_c)·t`
   - `out_c = amount ≥ 0 ? I_c + amount·(J_c − I_c) : I_c + (−amount)·(hazed_c − I_c)`

> `dehaze_recover(px, dark, a, amount)` already implements step 6 given `dark` such that `t = 1 − ω·dark`. To reuse it unchanged, the recovery pass converts the refined transmission `q` back to an effective `dark' = (1 − q)/ω` and calls the same formula — OR the recovery shader takes `q` directly. **Decision (Task 3):** the recovery shader takes the refined transmission `q` directly (cleaner); the pure `dehaze_recover` stays the CPU reference for the *blend* math with `dark` derived as `(1−q)/ω` so the existing unit tests are untouched. Both are algebraically identical.

**Halo:** a pixel's `q` depends on inputs within `r` (block min) + `gr` (var/cov box) + `gr` (mean_a/mean_b box) = **`r + 2·gr`**. `dehaze_halo` returns this.

**Parameters (constants, Task 1):** `DEHAZE_GUIDED_RADIUS_FACTOR` → `gr = r` (guided radius equals patch radius; keeps one knob). `DEHAZE_GUIDED_EPS = 1e-3` (edge sensitivity; tuned in the visual test). `MAX_DEHAZE_RADIUS` stays 64, but the effective halo `r+2·gr = 3r` (cap 192) — verify the haloed tile extent stays within limits (Task 5); if not, clamp `gr` so `r+2gr` ≤ a documented max.

---

## File Structure

**`ferrolite-pipeline`:**
- `src/dehaze.rs` — add pure CPU `box_blur_separable`, `min_filter_separable`, `transmission_map`, guided-filter helpers, constants; update `dehaze_halo`. Keep `dehaze_recover` unchanged. *(Task 1)*
- `src/shaders/` — new: `dehaze_dark_channel.wgsl`, `dehaze_min_h.wgsl`, `dehaze_min_v.wgsl`, `dehaze_box_h.wgsl`, `dehaze_box_v.wgsl`, `dehaze_guided_ab.wgsl`, `dehaze_guided_combine.wgsl` (or a smaller consolidated set — see Task 2), `dehaze_recovery.wgsl`. Delete `dehaze.wgsl` (the old single pass). *(Tasks 2–3)*
- `src/dehaze_node.rs` *(new)* — `DehazeTransmissionNode` (multi-pass) + `DehazeRecoveryNode`. *(Tasks 2–3)*
- `src/pipeline.rs` (`EditPipeline`) + `src/tile_edit.rs` (`TileEditPipeline`) — replace the dehaze `PointOpNode` with the two nodes; wire params; node_count; halo. *(Tasks 4–5)*
- `src/lib.rs` — module decl, exports, `prewarm_shaders`. *(Tasks 2–5)*
- `tests/golden.rs` — quality golden (no halo) + updated tiled parity. *(Tasks 4–5)*

**`ferrolite-app` / `ferrolite-export`:** only if `dehaze_halo`'s larger value or a signature change ripples; the op/UI/`set_dehaze`/`needs_full_rebuild` logic is unchanged (radius still changes the halo → rebuild). *(Task 6)*

---

## Task 1: Pure CPU reference — separable min, box blur, guided-filter transmission

**Files:**
- Modify: `ferrolite-pipeline/src/dehaze.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (exports)

**Interfaces:**
- Consumes: `crate::op::Dehaze`, `DEHAZE_OMEGA`, `MAX_DEHAZE_RADIUS`, `ferrolite_image::LinearRgbaF32`.
- Produces (pure, no GPU):
  - `const DEHAZE_GUIDED_EPS: f32 = 1e-3;` and `fn guided_radius(r: u32) -> u32 { r }` (gr = r).
  - `pub fn transmission_map(img: &[[f32;3]], w: usize, h: usize, a: [f32;3], radius: u32) -> Vec<f32>` — steps 1–5 of the reference; returns refined transmission `q` per pixel. Pure, deterministic.
  - crate-internal helpers `min_filter_separable`, `box_blur_separable` (both take a scalar plane + radius, clamp-to-edge).
  - `dehaze_halo(op) -> u32` updated to return `r + 2·gr = 3·r` (clamped) when active.

- [ ] **Step 1: Write the failing tests** (append to `dehaze.rs` `#[cfg(test)] mod tests`)

```rust
    #[test]
    fn min_filter_separable_matches_naive_patch_min() {
        // 6x5 plane, radius 2: separable (H then V) min == naive (2r+1)^2 patch min.
        let (w, h) = (6usize, 5usize);
        let plane: Vec<f32> = (0..w * h).map(|i| ((i * 37) % 11) as f32 / 11.0).collect();
        let sep = min_filter_separable(&plane, w, h, 2);
        // naive reference
        let mut naive = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut m = f32::INFINITY;
                for dy in -2i32..=2 {
                    for dx in -2i32..=2 {
                        let qx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                        let qy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                        m = m.min(plane[qy * w + qx]);
                    }
                }
                naive[y * w + x] = m;
            }
        }
        for (a, b) in sep.iter().zip(naive.iter()) {
            assert!((a - b).abs() < 1e-6, "separable min must equal naive patch min");
        }
    }

    #[test]
    fn transmission_identity_on_flat_image_has_no_structure() {
        // A flat grey image → transmission is spatially constant (no halos, no NaN).
        let (w, h) = (16usize, 16usize);
        let img = vec![[0.5f32, 0.5, 0.5]; w * h];
        let q = transmission_map(&img, w, h, [0.9, 0.9, 0.9], 4);
        let first = q[0];
        for &v in &q {
            assert!(v.is_finite());
            assert!((v - first).abs() < 1e-4, "flat image → flat transmission");
        }
    }

    #[test]
    fn guided_transmission_follows_the_luma_edge_not_a_dilated_block() {
        // Left half dark, right half bright (a vertical edge at x=w/2). The refined
        // transmission must transition SHARPLY at the edge (guided by luma), NOT be
        // dilated by the patch radius the way an un-refined block-min transmission is.
        let (w, h) = (32usize, 8usize);
        let mut img = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.05 } else { 0.9 };
                img[y * w + x] = [v, v, v];
            }
        }
        let a = [0.9, 0.9, 0.9];
        let radius = 6;
        let q = transmission_map(&img, w, h, a, radius);
        // Sample a row: the transmission on the bright side, just past the edge +
        // (radius) px, must be close to the deep-bright transmission (i.e. the dark
        // side did NOT bleed `radius` px into the bright side). Compare the value at
        // x = w/2 + radius + 1 to the far-bright value at x = w-1.
        let row = (h / 2) * w;
        let near = q[row + w / 2 + radius as usize + 1];
        let far = q[row + w - 1];
        assert!(
            (near - far).abs() < 0.15,
            "guided transmission must not dilate the dark region across the edge \
             (near={near}, far={far}) — this is the halo the refinement removes"
        );
    }

    #[test]
    fn dehaze_halo_includes_guided_window() {
        // Halo now covers the block-min radius PLUS the two guided-filter box windows.
        assert_eq!(dehaze_halo(Some(Dehaze { amount: 0.5, radius: 8 })), 8 + 2 * 8);
        assert_eq!(dehaze_halo(Some(Dehaze { amount: 0.0, radius: 8 })), 0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline --lib dehaze::`
Expected: FAIL — `min_filter_separable`, `transmission_map`, `guided_radius`, `DEHAZE_GUIDED_EPS` undefined; `dehaze_halo` returns the old `r`.

- [ ] **Step 3: Implement the pure math** (add to `dehaze.rs`, above the tests)

```rust
/// Guided-filter regularization ε (design step 5): larger = smoother/less edge-aware.
pub const DEHAZE_GUIDED_EPS: f32 = 1e-3;

/// Guided-filter window radius as a function of the patch radius (one knob: gr = r).
pub fn guided_radius(r: u32) -> u32 {
    r
}

/// Separable clamp-to-edge min over a `(2r+1)²` window: horizontal min pass then
/// vertical min pass. Equals the naïve patch min but O(2r) per pixel, not O(r²).
pub(crate) fn min_filter_separable(plane: &[f32], w: usize, h: usize, r: i32) -> Vec<f32> {
    let idx = |x: i32, y: i32| -> usize {
        (y.clamp(0, h as i32 - 1) as usize) * w + x.clamp(0, w as i32 - 1) as usize
    };
    let mut horiz = vec![0.0f32; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut m = f32::INFINITY;
            for dx in -r..=r {
                m = m.min(plane[idx(x + dx, y)]);
            }
            horiz[idx(x, y)] = m;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut m = f32::INFINITY;
            for dy in -r..=r {
                m = m.min(horiz[idx(x, y + dy)]);
            }
            out[idx(x, y)] = m;
        }
    }
    out
}

/// Separable clamp-to-edge normalized box average of radius `r` (H then V).
pub(crate) fn box_blur_separable(plane: &[f32], w: usize, h: usize, r: i32) -> Vec<f32> {
    let idx = |x: i32, y: i32| -> usize {
        (y.clamp(0, h as i32 - 1) as usize) * w + x.clamp(0, w as i32 - 1) as usize
    };
    let n = (2 * r + 1) as f32;
    let mut horiz = vec![0.0f32; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut s = 0.0;
            for dx in -r..=r {
                s += plane[idx(x + dx, y)];
            }
            horiz[idx(x, y)] = s / n;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut s = 0.0;
            for dy in -r..=r {
                s += horiz[idx(x, y + dy)];
            }
            out[idx(x, y)] = s / n;
        }
    }
    out
}

/// Rec.709 luma of a display-linear RGB triple.
fn luma709_px(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Refined dehaze transmission map `q` (design steps 1–5): normalized dark channel
/// → separable block-min over `radius` → guided-filter refinement (guide = luma).
/// Pure CPU reference the WGSL passes are golden-tested against. Deterministic.
pub fn transmission_map(
    img: &[[f32; 3]],
    w: usize,
    h: usize,
    a: [f32; 3],
    radius: u32,
) -> Vec<f32> {
    let n = w * h;
    let af = [a[0].max(DEHAZE_ATMOS_MIN), a[1].max(DEHAZE_ATMOS_MIN), a[2].max(DEHAZE_ATMOS_MIN)];
    // 1. normalized dark channel; 4. guide (luma)
    let mut dc0 = vec![0.0f32; n];
    let mut guide = vec![0.0f32; n];
    for i in 0..n {
        let c = img[i];
        dc0[i] = (c[0] / af[0]).min(c[1] / af[1]).min(c[2] / af[2]);
        guide[i] = luma709_px(c);
    }
    // 2. block min (separable)
    let dc = min_filter_separable(&dc0, w, h, radius as i32);
    // 3. raw transmission
    let praw: Vec<f32> = dc.iter().map(|&d| (1.0 - DEHAZE_OMEGA * d).clamp(0.0, 1.0)).collect();
    // 5. guided filter (guide = luma), window gr, eps
    let gr = guided_radius(radius) as i32;
    let gg: Vec<f32> = guide.iter().map(|&g| g * g).collect();
    let gp: Vec<f32> = guide.iter().zip(&praw).map(|(&g, &p)| g * p).collect();
    let mean_g = box_blur_separable(&guide, w, h, gr);
    let mean_p = box_blur_separable(&praw, w, h, gr);
    let corr_g = box_blur_separable(&gg, w, h, gr);
    let corr_gp = box_blur_separable(&gp, w, h, gr);
    let mut av = vec![0.0f32; n];
    let mut bv = vec![0.0f32; n];
    for i in 0..n {
        let var_g = corr_g[i] - mean_g[i] * mean_g[i];
        let cov_gp = corr_gp[i] - mean_g[i] * mean_p[i];
        av[i] = cov_gp / (var_g + DEHAZE_GUIDED_EPS);
        bv[i] = mean_p[i] - av[i] * mean_g[i];
    }
    let mean_a = box_blur_separable(&av, w, h, gr);
    let mean_b = box_blur_separable(&bv, w, h, gr);
    (0..n).map(|i| (mean_a[i] * guide[i] + mean_b[i]).clamp(0.0, 1.0)).collect()
}
```

Update `dehaze_halo`:

```rust
pub fn dehaze_halo(op: Option<Dehaze>) -> u32 {
    match op {
        Some(d) if d.amount != 0.0 => {
            let r = d.radius.min(MAX_DEHAZE_RADIUS);
            r + 2 * guided_radius(r)
        }
        _ => 0,
    }
}
```

- [ ] **Step 4: Export the new public items** (`lib.rs`)

Add `transmission_map`, `guided_radius`, `DEHAZE_GUIDED_EPS` to the `pub use dehaze::{...}` line (keep `min_filter_separable`/`box_blur_separable`/`luma709_px` crate-internal). `dehaze_halo` is already exported.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --lib dehaze::`
Expected: PASS (including the existing base-feature dehaze tests — `dehaze_recover`, atmosphere, uniform — which are unchanged).

- [ ] **Step 6: Workspace gate + commit**

The changed `dehaze_halo` value will change `needs_full_rebuild`/halo behavior downstream; confirm the workspace still builds (the app/export consume `dehaze_halo` but only compare it — no signature change). Run the gate; then:
```bash
git add ferrolite-pipeline/src/dehaze.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): guided-filter transmission reference + separable min/box + r+2gr halo"
```

---

## Task 2: `DehazeTransmissionNode` — multi-pass GPU transmission (separable min + guided filter)

**Files:**
- Create: `ferrolite-pipeline/src/dehaze_node.rs`
- Create WGSL: `src/shaders/dehaze_dark_channel.wgsl`, `dehaze_min_h.wgsl`, `dehaze_min_v.wgsl`, `dehaze_box_h.wgsl`, `dehaze_box_v.wgsl`, `dehaze_guided_ab.wgsl`, `dehaze_guided_q.wgsl`
- Modify: `src/lib.rs` (module decl; `prewarm_shaders`)

**Interfaces:**
- Consumes: the input `PipelineImage` (rgba16float), a shared `Rc<Cell<TransmissionParams>>` (`{ radius: i32, atmos: [f32;4], eps: f32, omega: f32 }`), `GpuContext`, `Node<PipelineImage>`.
- Produces: `pub(crate) struct DehazeTransmissionNode` implementing `Node<PipelineImage>`; its `evaluate` returns a `PipelineImage` carrying the refined transmission `q` in **all RGBA channels** (so downstream can read `.r`). Built once; pipelines cached; intermediate single-channel `R32Float` textures cached + reallocated on dim change. `impl Node for Rc<DehazeTransmissionNode>` delegate.

**Design notes (follow `local_node.rs` for the multi-pass structure):**
- All intermediate planes are `R32Float` storage textures (dark channel, praw, guide, gg, gp, means, a, b, q). Use a small helper to build them once and reallocate on `(w,h)` change (mirror `local_node.rs::alloc_out`/`ensure_out`, but keyed on dims only).
- **Passes** (each a compute pipeline built once; workgroup 8×8; clamp-to-edge sampling via `textureLoad` + `clamp`):
  1. `dehaze_dark_channel`: in = src rgba16float; out = 2× R32Float: `dc0 = min(rgb/A)` and `guide = luma(rgb)`. (Two write targets, or two passes — a single pass writing two storage textures is fine.)
  2. `dehaze_min_h` then `dehaze_min_v`: separable min of `dc0` over `radius` → `dc`. Then compute `praw = clamp(1-ω·dc,0,1)` (fold into `min_v`'s output).
  3. products `gg = guide²`, `gp = guide·praw` (fold into the box-H input stage or a tiny pass).
  4. box filter (separable `dehaze_box_h`/`dehaze_box_v`, radius `gr`) applied to `guide, praw, gg, gp` → `mean_g, mean_p, corr_g, corr_gp`. (The box passes are generic — parameterize by input/output binding; reuse the same two shaders for all four planes.)
  5. `dehaze_guided_ab`: `a = (corr_gp - mean_g·mean_p)/((corr_g - mean_g²) + ε)`, `b = mean_p - a·mean_g` → `a`, `b` planes.
  6. box filter `a, b` (radius `gr`) → `mean_a, mean_b`.
  7. `dehaze_guided_q`: `q = clamp(mean_a·guide + mean_b, 0, 1)`; write `q` into an rgba16float output (all channels = q).
- Each WGSL pass mirrors the corresponding CPU step in `transmission_map` EXACTLY (the golden in Task 4 checks GPU-vs-CPU within f16 tolerance). Keep box/min passes generic + reused to minimize shader count.
- `TransmissionParams` `#[repr(C)]` Pod uniform: `{ radius: i32, pad: i32, atmos: [f32;4], eps: f32, omega: f32, pad2: [f32;2] }` (16-aligned) — mirror in each shader's `struct P` that needs it.

- [ ] **Step 1: Write a node-level golden test first** (in `dehaze_node.rs` `#[cfg(test)]`, mirroring `local_node.rs`'s readback test)

Test `transmission_node_matches_cpu_reference`: upload a small synthetic edge image (the vertical dark/bright edge from Task 1), build the node, `evaluate`, read back the `.r` channel, and compare to `transmission_map(...)` within `2e-2` (f16 + float drift). Skip on no GPU (`GpuContext::headless()` guard, like the sibling tests).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p ferrolite-pipeline --lib dehaze_node::`
Expected: FAIL — `DehazeTransmissionNode`/module undefined.

- [ ] **Step 3: Implement the WGSL passes + the node**

Write each shader per the Design notes (mirror `transmission_map`). Implement `DehazeTransmissionNode` following `local_node.rs`: build all pipelines + intermediate textures once; `evaluate` runs the pass sequence into the cached intermediates and returns the `q` output image. Read params from the shared `Cell`. (Full per-shader code is the CPU reference translated to WGSL — the golden is the gate; the box/min shaders are ~15 lines each like `sharpen.wgsl`'s loop but 1-D and separable.)

Add `mod dehaze_node;` to `lib.rs` and the new shader labels to `prewarm_shaders`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p ferrolite-pipeline --lib dehaze_node::transmission_node_matches_cpu_reference`
Expected: PASS on GPU (or headless-skip line). If the edge transmission diverges from CPU, a pass mis-mirrors the reference — debug that pass against its CPU step (do not loosen tolerance beyond f16 reality).

- [ ] **Step 5: Workspace gate + commit**

```bash
git add ferrolite-pipeline/src/dehaze_node.rs ferrolite-pipeline/src/shaders/dehaze_*.wgsl ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): DehazeTransmissionNode (separable min + guided-filter refinement, GPU=CPU golden)"
```

---

## Task 3: `DehazeRecoveryNode` — two-input recovery + blend

**Files:**
- Modify: `ferrolite-pipeline/src/dehaze_node.rs`
- Create WGSL: `src/shaders/dehaze_recovery.wgsl`
- Delete: `src/shaders/dehaze.wgsl` (old single pass — after Tasks 4–5 stop referencing it; delete here and fix the include in Task 4)
- Modify: `src/lib.rs` (`prewarm_shaders`)

**Interfaces:**
- Consumes: two inputs — `inputs[0]` = the image `I` (rgba16float), `inputs[1]` = the transmission `q` (rgba16float, `q` in `.r`); a shared `Rc<Cell<RecoveryParams>>` (`{ amount: f32, t0: f32, atmos: [f32;4] }`).
- Produces: `pub(crate) struct DehazeRecoveryNode: Node<PipelineImage>` — one compute pass; `out_c = blend(I_c, q, amount, A)` per the reference step 6. `impl Node for Rc<DehazeRecoveryNode>` delegate. Bind layout: 0 = I texture, 1 = q texture, 2 = dst storage, 3 = uniform.

- [ ] **Step 1: Write the failing test**

`recovery_node_matches_dehaze_recover`: upload a small `I` and a constant-`q` transmission texture; `evaluate`; read back; compare each pixel to `dehaze_recover(px, (1.0-q)/DEHAZE_OMEGA, A, amount)` within `2e-3`. Covers amount 0 (identity), +, −.

- [ ] **Step 2: Verify it fails** — `cargo test -p ferrolite-pipeline --lib dehaze_node::recovery_node`

- [ ] **Step 3: Implement `dehaze_recovery.wgsl` + `DehazeRecoveryNode`**

Shader (mirrors `dehaze_recover`, taking `q` directly):
```wgsl
@group(0) @binding(0) var img: texture_2d<f32>;
@group(0) @binding(1) var trans: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;
struct P { amount: f32, t0: f32, pad0: f32, pad1: f32, atmos: vec4<f32> };
@group(0) @binding(3) var<uniform> p: P;
@compute @workgroup_size(8,8,1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(img));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(img, xy, 0);
    if (p.amount == 0.0) { textureStore(dst, xy, c); return; }
    let a = p.atmos.rgb;
    let t = clamp(textureLoad(trans, xy, 0).r, 0.0, 1.0);
    let te = max(t, p.t0);
    let j = (c.rgb - a) / te + a;
    let hazed = a + (c.rgb - a) * t;
    var out = c.rgb;
    if (p.amount >= 0.0) { out = c.rgb + p.amount * (j - c.rgb); }
    else { out = c.rgb + (-p.amount) * (hazed - c.rgb); }
    textureStore(dst, xy, vec4<f32>(out, c.a));
}
```
Node mirrors `PointOpNode` but with two input textures. Add `dehaze-recovery` to `prewarm_shaders`.

- [ ] **Step 4: Verify it passes** — the recovery-node test on GPU.

- [ ] **Step 5: Workspace gate + commit**

```bash
git add ferrolite-pipeline/src/dehaze_node.rs ferrolite-pipeline/src/shaders/dehaze_recovery.wgsl ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): DehazeRecoveryNode (two-input recovery+blend, mirrors dehaze_recover)"
```

---

## Task 4: Wire the two nodes into `EditPipeline` (whole-image) + quality golden + amount-drag cache proof

**Files:**
- Modify: `ferrolite-pipeline/src/pipeline.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (drop the old `dehaze` single-pass entry from `prewarm_shaders`; delete `shaders/dehaze.wgsl` include)
- Modify: `ferrolite-pipeline/tests/golden.rs`

**Interfaces:**
- Consumes: `DehazeTransmissionNode`, `DehazeRecoveryNode`, `TransmissionParams`, `RecoveryParams`.
- Produces: `EditPipeline` replaces the single dehaze `PointOpNode` with `transmission` (input `contrast_id`) + `recovery` (inputs `[contrast_id, transmission_id]`); `tone_curve` input becomes `recovery_id`. Fields: `dehaze_transmission_id/node`, `dehaze_recovery_id`, `transmission_params: Rc<Cell<TransmissionParams>>`, `recovery_params: Rc<Cell<RecoveryParams>>`, `dehaze_atmos`. `node_count` +1 vs current. `set_stack` routes `amount`→recovery params only, `radius`/`atmos`→transmission params (+ dirty transmission).

- [ ] **Step 1: Write the failing tests**

(a) **Quality — no halo** (`dehaze_no_halo_on_dark_edge`): a synthetic image with a dark bar on a bright hazy field; run through `EditPipeline` with dehaze `amount=1, radius=8`; read back; assert the bright field pixels within `[edge+radius/2 .. edge+radius]` of the bar are NOT significantly brighter than the far-field (i.e. no bright halo ring of width ~radius). Precise check: max deviation of the near-edge bright band from the far-field mean < a small threshold; and contrast this with the KNOWN-BAD single-pass behavior in the assertion message. (This is the artifact the author saw.)

(b) **Amount-drag caches transmission** (`amount_change_does_not_recompute_transmission`): add a `transmission_rebuild_count()` test hook to `DehazeTransmissionNode` (a `Cell<u32>` bumped each `evaluate`, exactly like `local_rebuild_count`). Build `EditPipeline` with dehaze, `evaluate()` once (count=1). `set_stack` with a changed **amount only** (same radius), `evaluate()` → count still **1** (transmission reused; recovery re-ran). Then `set_stack` with a changed **radius**, `evaluate()` → count **2** (transmission recomputed).

- [ ] **Step 2: Verify they fail** (`EditPipeline` still has the old single node; hooks missing).

- [ ] **Step 3: Wire the nodes**

In `EditPipeline::new`, replace the dehaze `PointOpNode` block with the transmission + recovery nodes (transmission input `contrast_id`; recovery inputs `vec![contrast_id, transmission_id]`; retain `Rc<DehazeTransmissionNode>` for the rebuild-count hook, like `local_node`). Repoint `tone_curve` to `recovery_id`. Update `node_count`. In `set_stack`: compute the new op; set `recovery_params.amount` always (dirty recovery on change); when `radius` or `dehaze_atmos` changed, update `transmission_params` and `mark_dirty(transmission_id)`. Keep the `A`-from-source estimate at construction (unchanged). Delete the old `shaders/dehaze.wgsl` include + `prewarm` entry; add the new shader labels if not already (Task 2/3 added them).

- [ ] **Step 4: Verify they pass** (both goldens + the cache-count test on GPU).

- [ ] **Step 5: Workspace gate + commit**

```bash
git add ferrolite-pipeline/src/pipeline.rs ferrolite-pipeline/src/lib.rs ferrolite-pipeline/tests/golden.rs ferrolite-pipeline/src/shaders/dehaze.wgsl
git commit -m "feat(pipeline): EditPipeline uses transmission+recovery nodes (halo-free dehaze; amount drag skips transmission)"
```

---

## Task 5: Wire into `TileEditPipeline` + enlarged halo + tiled parity

**Files:**
- Modify: `ferrolite-pipeline/src/tile_edit.rs`
- Modify: `ferrolite-pipeline/tests/golden.rs`

**Interfaces:**
- Produces: `TileEditPipeline` uses the same two nodes (contrast→transmission→recovery→tone_curve); `halo` folds in the new `dehaze_halo` (`r+2gr`); `set_dehaze_atmos` routes to the transmission params (+ dirty transmission); `set_stack` routes amount→recovery, radius/atmos→transmission.

- [ ] **Step 1: Update the parity golden first**

Extend `dehaze_tiled_matches_whole_image` to use `radius: 12` (so `r+2gr = 36` halo is exercised across the x=256 seam) and estimate + set the SAME `A` on both tiers (already done). It must PASS (seamless) with the new nodes and enlarged halo. Keep the sensitivity guarantee: verify (temporarily) that dropping the `dehaze_halo` fold-in still makes it FAIL, then restore.

- [ ] **Step 2: Verify it fails** (tiled tier still on the old node / halo).

- [ ] **Step 3: Wire the tiled tier**

Mirror Task 4 in `tile_edit.rs`: transmission + recovery nodes between contrast and tone_curve; `let halo = sharpen_halo(...).max(lens_halo_px(...)).max(dehaze_halo(stack.dehaze()));` (dehaze_halo now returns `r+2gr`); `set_dehaze_atmos` updates transmission params + dirties transmission; `set_stack` routes amount/radius/atmos as in Task 4. Confirm the haloed tile extent (`haloed_tile_extent(halo)`) stays within texture limits at `MAX_DEHAZE_RADIUS` — if `3·MAX = 192` is too large for the tile+halo buffer, clamp `guided_radius` so `r+2gr` ≤ a documented safe bound and note it.

- [ ] **Step 4: Verify it passes** (parity golden seamless on GPU).

- [ ] **Step 5: Workspace gate + commit**

```bash
git add ferrolite-pipeline/src/tile_edit.rs ferrolite-pipeline/tests/golden.rs
git commit -m "feat(pipeline): TileEditPipeline transmission+recovery nodes + r+2gr halo (tiled parity holds)"
```

---

## Task 6: App/export follow-through + cleanup

**Files:**
- Modify (only if needed): `ferrolite-app/src/develop/ops_edit.rs` (verify `needs_full_rebuild` still correct with the larger halo), `ferrolite-pipeline/src/lib.rs` (`prewarm_pipelines` still builds a dehaze-carrying dummy? — the default stack has no dehaze, so the transmission/recovery nodes exist but are identity; confirm `prewarm_pipelines` still evaluates cleanly).

**Interfaces:** no API changes expected; this task is verification + any compile follow-through from the node restructure.

- [ ] **Step 1: Confirm `needs_full_rebuild` semantics**

`needs_full_rebuild` compares `dehaze_halo(old) != dehaze_halo(new)`. With `dehaze_halo = r+2gr`, a radius change still changes the halo → rebuild (correct); amount-only → same halo → no rebuild → handled by `set_stack` on the retained producer (which now routes amount→recovery only). Add/confirm a test in `ops_edit.rs` that an amount-only change does NOT rebuild and a radius change does (the existing `needs_full_rebuild_on_dehaze_halo_change` already asserts this — verify it still holds with the new halo values; update the expected halo numbers if the test hard-codes them).

- [ ] **Step 2: Confirm prewarm + startup**

`prewarm_shaders` includes all new dehaze passes and no longer includes the deleted single `dehaze`. `prewarm_pipelines` builds a dummy `EditPipeline` + `TileEditPipeline` with the default (no-dehaze) stack — confirm the transmission/recovery nodes evaluate without error at identity (amount 0 → recovery passthrough; transmission still computes but its output is unused when amount 0 — acceptable, or short-circuit: when amount 0 the recovery node passthrough already ignores `q`; the transmission node still runs once at prewarm, which is fine). Run `prewarm`-covering tests if any.

- [ ] **Step 3: Full workspace gate**

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
All green except the 5 pre-existing `ferrolite-decode` fixture failures.

- [ ] **Step 4: Commit (if any changes)**

```bash
git commit -am "chore(app): verify dehaze rebuild/prewarm after transmission+recovery split"
```

---

## Self-Review (against the two reported problems + constraints)

- **Quality (halos/rectangular fringes)** → guided-filter refinement (Task 1 CPU reference + Task 2 GPU node), verified by `guided_transmission_follows_the_luma_edge_not_a_dilated_block` (unit) and `dehaze_no_halo_on_dark_edge` (golden). The transmission follows luma edges instead of dilating dark blocks by the radius.
- **Speed (sluggish amount drag)** → separable min (O(2r), Task 1/2) + two-node split so amount drags skip the transmission entirely (Task 4 `amount_change_does_not_recompute_transmission`, via the graph's dirty caching). Radius drags recompute at O(r).
- **Responsiveness/build-once** → pipelines built once per node (Task 2/3), prewarmed (Task 2/3/6), no per-frame CPU; the amount-drag hot path is one recovery pass.
- **Tiling** → halo `r+2gr` (Task 1 `dehaze_halo`), tiled parity seamless with the enlarged halo and still sensitive to the fold-in (Task 5).
- **§2.5** → `dehaze_recover` unchanged; `transmission_map` is the new pure reference; no logic lives only in a shader.
- **No op/UI change** → `Dehaze`, Effects tab, `set_dehaze`, per-control reset, op order all untouched.
- **Placeholder scan:** the box/min WGSL passes are specified by the CPU reference + the generic-box design note rather than 7 verbatim shaders — the GPU=CPU golden (Task 2) is the completeness gate; the two non-trivial shaders whose exact code matters (recovery, and the params layout) are given in full. Implementers must translate each CPU step to WGSL and let the golden verify.
- **Type consistency:** `transmission_map(&[[f32;3]],usize,usize,[f32;3],u32)->Vec<f32>`, `dehaze_halo->r+2gr`, `TransmissionParams`/`RecoveryParams` used identically across Tasks 2–5.

## Visual test (after green gate — the real acceptance)

Re-run the author's checklist, focused on the two fixes: (1) drag **Dehaze** amount up/down on a hazy shot — must be **smooth/responsive** now (no lag) and show **no bright halo / rectangular fringe** around the dark branches; (2) drag **Radius** — recompute is visible but bounded; (3) zoom to 1:1 — no seams with the larger halo; (4) tune is subjectively good (if halos persist, raise `gr`/`ε`; if too soft, lower `ε`).
