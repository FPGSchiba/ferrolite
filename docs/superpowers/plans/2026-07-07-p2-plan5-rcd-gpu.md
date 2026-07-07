# P2 Plan 5 — RCD demosaic (WGSL GPU) + full-res tier Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Demosaic the raw CFA with a WGSL GPU RCD compute pass (matching the Plan-4 CPU reference), and make it the on-screen full-resolution tier so zooming to 1:1 shows RCD detail — with QuadBin remaining the fast preview/reveal tier, and no UI-thread freeze.

**Architecture (Option W — whole-image, chosen with the author):** A single-channel CFA is uploaded to the GPU and demosaiced by a two-pass WGSL compute (Hamilton-Adams directional green → constant-hue colour-difference chroma + WB), producing a full-res `LinearRgbaF32`. This runs **inside the existing full-decode job** (off the UI thread) on a cloned `Arc<GpuContext>`, replacing QuadBin's half-res output as the source of the existing GPU pyramid/tiled producer — so the whole downstream tiled render, orientation, and two-tier reveal are reused **unchanged**. The generic executor and the VT are untouched (contract §4/§5): RCD is a photo-tier `ferrolite-pipeline` function, not an executor/VT change.

**Tech Stack:** Rust + WGSL, `wgpu` compute. Crates touched: `ferrolite-pipeline` (GPU RCD + shaders + golden), `ferrolite-app` (wire the full-decode job to GPU RCD). Depends on Plans 1–4 (merged): reuses the Plan-4 CPU `Rcd` as the golden reference and the unclamped-carry convention.

## Global Constraints

- **Generic executor untouched (contract §4).** Do NOT modify `ferrolite-gpu/src/executor.rs`. RCD is a `ferrolite-pipeline` function/compute pass.
- **VT untouched / source-agnostic (contract §5).** No `ferrolite-vt` changes; the CFA is a pipeline-tier GPU resource, no photo concepts leak into the engine tier.
- **No UI-thread block (CLAUDE.md §1).** The GPU RCD + readback runs inside the existing `Visible`-priority full-decode job (a worker thread), never on the UI/update thread. The `GpuContext` (`Arc<Device>`/`Arc<Queue>`, `Send+Sync`) is cloned into the job.
- **Build-once discipline (CLAUDE.md §2).** GPU RCD runs **once per image open** (not per frame / per edit / per tile) — analogous to the existing one-shot `color_convert`. The downstream `GpuPyramidSource` + `TileEditPipeline` are still built once and reused for every tile/edit, unchanged.
- **Match the CPU RCD reference exactly.** The WGSL must reproduce Plan-4 `ferrolite_decode::Rcd`: normalized CFA `c = ((raw - black_levels[pos]) / span).max(0.0)`, `span = white_level - black_levels[0]`, `pos = (y%2)*2 + (x%2)`; Hamilton-Adams green; constant-hue colour-difference chroma; WB (`wb_coeffs[0..2]`) applied per channel **after** interpolation; **unclamped** output (carries >1 / negatives, P2 §5.3). GPU golden vs CPU within tolerance.
- **RGGB only.** GPU RCD handles the RGGB pattern `[0,1,1,2]`; the caller (`spawn_full`) gates and falls back to `QuadBin` for any other pattern (matching `Rcd`'s fallback).
- **Two-tier is automatic (S5), no new controls / no persisted state.** Tier-1 (embedded preview / reveal) is unchanged and instant; tier-2 (the full decode) becomes RCD full-res. No zoom slider, no toggle, no sidecar change.
- **Photo tier only.** `ferrolite-pipeline`, `ferrolite-app`. `ferrolite-pipeline` gains a **dev-dependency** on `ferrolite-decode` (golden's CPU reference only) — no runtime dependency (the GPU fn takes plain CFA fields via `CfaInput`).
- **Gate (per branch):** `cargo fmt --check` + `cargo clippy --workspace --all-targets --all-features -- -D warnings` + `cargo test --workspace` green → **then STOP and hold for the author's (Jann's) visual test** (CLAUDE.md "Finishing a branch"). This plan HAS a visual test (below).

## Resolved design questions (spec §8 + the Option-W decision)

- **CFA GPU format:** uploaded as a **storage buffer `array<f32>`** of the CPU-normalized `c` (not a texture) — this mirrors the CPU `sample()` indexing exactly (clamp-to-edge integer addressing, no sampler/filter concerns) and sidesteps `R32Float`-storage-texture/`FLOAT32_FILTERABLE` questions. The intermediate green plane is likewise a storage buffer; only the final RGB output is an `rgba16float` storage **texture** (the pipeline's native format).
- **Pattern-offset uniform (§8):** N/A for whole-image RCD — the CFA phase is fixed at `(0,0)` (top-left), so `pos` is computed from absolute `(x,y)`, exactly matching the CPU. (A per-tile phase offset would only be needed by the deferred tiled-CFA approach — Option T.)
- **Halo / VT halo (§8, contract §5):** N/A here — RCD runs once over the whole image (not per-tile), so there is no RCD tile seam to halo. Downstream neighborhood ops (sharpen) keep their existing halo via the pyramid, unchanged.
- **Export:** stays on the Plan-4 **CPU** `Rcd` (already correct, runs on the export worker) — the visual test's "export → RCD applied" is already satisfied. Not re-touched here.
- **Deferred (not in this plan):** the tiled CFA + halo-consuming RCD head (Option T, VRAM-bounded per §6) — a future optimization if the full-res `Rgba16Float` level-0 VRAM cost becomes a problem on low-VRAM GPUs.

---

## File Structure

- `ferrolite-pipeline/src/shaders/rcd_green.wgsl` **(new)** — pass 1: normalized CFA buffer → green buffer (Hamilton-Adams directional).
- `ferrolite-pipeline/src/shaders/rcd_chroma.wgsl` **(new)** — pass 2: CFA + green buffers → `rgba16float` output texture (constant-hue chroma + per-channel WB, unclamped).
- `ferrolite-pipeline/src/rcd_gpu.rs` **(new)** — `CfaInput` + `demosaic_rcd_gpu(ctx, &CfaInput) -> LinearRgbaF32` (upload, two passes, readback) + the GPU golden test vs CPU `Rcd`.
- `ferrolite-pipeline/src/lib.rs` **(modify)** — declare `mod rcd_gpu;`, re-export `CfaInput` + `demosaic_rcd_gpu`.
- `ferrolite-pipeline/Cargo.toml` **(modify)** — add `ferrolite-decode` as a **dev-dependency** (golden reference only).
- `ferrolite-app/src/viewer/load.rs` **(modify)** — `spawn_full` gains an `Arc<GpuContext>` param; RGGB RAWs demosaic via `demosaic_rcd_gpu` (else `QuadBin`), keeping the orientation step.
- `ferrolite-app/src/app.rs` **(modify)** — at the `spawn_full` call site in `drive_viewer`, build `Arc<GpuContext>` from the render state and pass it in.

---

## Task 1: WGSL GPU RCD demosaic + golden vs CPU (`ferrolite-pipeline`)

**Files:**
- Create: `ferrolite-pipeline/src/shaders/rcd_green.wgsl`, `ferrolite-pipeline/src/shaders/rcd_chroma.wgsl`, `ferrolite-pipeline/src/rcd_gpu.rs`
- Modify: `ferrolite-pipeline/src/lib.rs`, `ferrolite-pipeline/Cargo.toml`
- Test: inline `#[cfg(test)] mod tests` in `rcd_gpu.rs` (GPU golden, auto-skips headless)

**Interfaces:**
- Consumes: `ferrolite_gpu::GpuContext`, `ferrolite_image::LinearRgbaF32`, `crate::image::PIPELINE_FORMAT`, `half::f16`, `wgpu`, `bytemuck`. Golden dev-dep: `ferrolite_decode::{Rcd, DemosaicToRgb16f, RawDecoded, color::ColorProfile}`.
- Produces:
  - `pub struct CfaInput<'a> { pub pixels: &'a [u16], pub width: u32, pub height: u32, pub cfa_pattern: [u8;4], pub black_levels: [f32;4], pub white_level: f32, pub wb_coeffs: [f32;4] }`
  - `pub fn demosaic_rcd_gpu(ctx: &GpuContext, cfa: &CfaInput) -> LinearRgbaF32` — full-res RGGB GPU RCD, WB'd, unclamped. **Assumes RGGB** (`cfa_pattern == [0,1,1,2]`); the caller gates.

- [ ] **Step 1: Add the dev-dependency**

In `ferrolite-pipeline/Cargo.toml`, under `[dev-dependencies]`, add:

```toml
ferrolite-decode = { workspace = true }
```

- [ ] **Step 2: Write the two WGSL shaders**

Create `ferrolite-pipeline/src/shaders/rcd_green.wgsl`:

```wgsl
// RCD pass 1: Hamilton-Adams directional green interpolation.
// Input: normalized single-channel CFA (storage buffer, row-major w*h).
// Output: full green plane (storage buffer, row-major w*h).
// Mirrors ferrolite_decode::rcd::interpolate_green exactly (RGGB, phase (0,0)).
struct Params { width: u32, height: u32, pad0: u32, pad1: u32, wb: vec4<f32> };
@group(0) @binding(0) var<storage, read> cfa: array<f32>;
@group(0) @binding(1) var<storage, read_write> green: array<f32>;
@group(0) @binding(2) var<uniform> p: Params;

fn s(x: i32, y: i32) -> f32 {
    let w = i32(p.width);
    let h = i32(p.height);
    let xc = clamp(x, 0, w - 1);
    let yc = clamp(y, 0, h - 1);
    return cfa[u32(yc) * p.width + u32(xc)];
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height) { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);
    let idx = gid.y * p.width + gid.x;
    let pos = (gid.y % 2u) * 2u + (gid.x % 2u);
    if (pos == 1u || pos == 2u) {
        green[idx] = s(x, y); // G site: measured
        return;
    }
    let center = s(x, y);
    let gh = abs(s(x - 1, y) - s(x + 1, y)) + abs(2.0 * center - s(x - 2, y) - s(x + 2, y));
    let gv = abs(s(x, y - 1) - s(x, y + 1)) + abs(2.0 * center - s(x, y - 2) - s(x, y + 2));
    let gh_est = 0.5 * (s(x - 1, y) + s(x + 1, y)) + 0.25 * (2.0 * center - s(x - 2, y) - s(x + 2, y));
    let gv_est = 0.5 * (s(x, y - 1) + s(x, y + 1)) + 0.25 * (2.0 * center - s(x, y - 2) - s(x, y + 2));
    var g: f32;
    if (gh < gv) { g = gh_est; } else if (gv < gh) { g = gv_est; } else { g = 0.5 * (gh_est + gv_est); }
    green[idx] = g;
}
```

Create `ferrolite-pipeline/src/shaders/rcd_chroma.wgsl`:

```wgsl
// RCD pass 2: constant-hue (colour-difference) red/blue, then per-channel WB.
// Inputs: normalized CFA + interpolated green (storage buffers).
// Output: rgba16float storage texture (WB'd, UNCLAMPED — carries >1/negatives).
// Mirrors ferrolite_decode::rcd::reconstruct_rgb + the caller's WB multiply.
struct Params { width: u32, height: u32, pad0: u32, pad1: u32, wb: vec4<f32> };
@group(0) @binding(0) var<storage, read> cfa: array<f32>;
@group(0) @binding(1) var<storage, read> green: array<f32>;
@group(0) @binding(2) var out_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var<uniform> p: Params;

fn cs(x: i32, y: i32) -> f32 {
    let w = i32(p.width);
    let h = i32(p.height);
    return cfa[u32(clamp(y, 0, h - 1)) * p.width + u32(clamp(x, 0, w - 1))];
}
fn gs(x: i32, y: i32) -> f32 {
    let w = i32(p.width);
    let h = i32(p.height);
    return green[u32(clamp(y, 0, h - 1)) * p.width + u32(clamp(x, 0, w - 1))];
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height) { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);
    let pos = (gid.y % 2u) * 2u + (gid.x % 2u);
    let g_here = green[gid.y * p.width + gid.x];
    var r: f32;
    var g: f32;
    var b: f32;
    if (pos == 0u) {
        // R site: R measured; B from 4 diagonal B neighbours.
        r = cs(x, y);
        g = g_here;
        b = g_here + 0.25 * ((cs(x - 1, y - 1) - gs(x - 1, y - 1))
            + (cs(x + 1, y - 1) - gs(x + 1, y - 1))
            + (cs(x - 1, y + 1) - gs(x - 1, y + 1))
            + (cs(x + 1, y + 1) - gs(x + 1, y + 1)));
    } else if (pos == 3u) {
        // B site: B measured; R from 4 diagonal R neighbours.
        b = cs(x, y);
        g = g_here;
        r = g_here + 0.25 * ((cs(x - 1, y - 1) - gs(x - 1, y - 1))
            + (cs(x + 1, y - 1) - gs(x + 1, y - 1))
            + (cs(x - 1, y + 1) - gs(x - 1, y + 1))
            + (cs(x + 1, y + 1) - gs(x + 1, y + 1)));
    } else if (pos == 1u) {
        // G site (even row, odd col): R horizontal, B vertical.
        g = cs(x, y);
        r = g + 0.5 * ((cs(x - 1, y) - gs(x - 1, y)) + (cs(x + 1, y) - gs(x + 1, y)));
        b = g + 0.5 * ((cs(x, y - 1) - gs(x, y - 1)) + (cs(x, y + 1) - gs(x, y + 1)));
    } else {
        // pos == 2: G site (odd row, even col): B horizontal, R vertical.
        g = cs(x, y);
        b = g + 0.5 * ((cs(x - 1, y) - gs(x - 1, y)) + (cs(x + 1, y) - gs(x + 1, y)));
        r = g + 0.5 * ((cs(x, y - 1) - gs(x, y - 1)) + (cs(x, y + 1) - gs(x, y + 1)));
    }
    textureStore(out_tex, vec2<i32>(x, y), vec4<f32>(r * p.wb.x, g * p.wb.y, b * p.wb.z, 1.0));
}
```

- [ ] **Step 3: Write the failing golden test (module + test, no impl yet)**

Create `ferrolite-pipeline/src/rcd_gpu.rs`:

```rust
//! GPU RCD demosaic (P2 Plan 5, Option W): a two-pass WGSL compute that
//! reproduces the Plan-4 CPU `ferrolite_decode::Rcd` (Hamilton-Adams directional
//! green + constant-hue colour-difference chroma), producing a full-res,
//! white-balanced, UNCLAMPED `LinearRgbaF32`. RGGB only (caller gates). Runs once
//! per image open, off the UI thread, on a cloned `GpuContext`. Generic executor
//! and VT untouched (contract §4/§5).

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use half::f16;
use wgpu::util::DeviceExt;

use crate::image::PIPELINE_FORMAT;

/// Plain CFA inputs for the GPU demosaic (avoids a runtime dep on ferrolite-decode).
pub struct CfaInput<'a> {
    pub pixels: &'a [u16],
    pub width: u32,
    pub height: u32,
    pub cfa_pattern: [u8; 4],
    pub black_levels: [f32; 4],
    pub white_level: f32,
    pub wb_coeffs: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RcdParams {
    width: u32,
    height: u32,
    pad0: u32,
    pad1: u32,
    wb: [f32; 4],
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_decode::{DemosaicToRgb16f, Rcd};

    /// Build a synthetic RGGB `RawDecoded` (black 0, white 65535) for the CPU reference.
    fn raw_rggb(w: u32, h: u32, pixels: Vec<u16>, wb: [f32; 4]) -> ferrolite_decode::RawDecoded {
        ferrolite_decode::RawDecoded {
            width: w,
            height: h,
            cpp: 1,
            pixels,
            cfa_pattern: [0, 1, 1, 2],
            black_levels: [0.0; 4],
            white_level: 65535.0,
            wb_coeffs: wb,
            color_profile: ferrolite_decode::ColorProfile::srgb_fallback(),
            orientation: ferrolite_image::Orientation::Normal,
        }
    }

    #[test]
    fn gpu_rcd_matches_cpu_reference() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        // A SMOOTH 64x64 ramp with distinct horizontal/vertical slopes: the
        // Hamilton-Adams gh-vs-gv choice is then unambiguous everywhere (gh > gv),
        // so CPU and GPU pick the SAME direction and agree within f16 — a
        // high-frequency/random image would let f32 rounding flip the direction at
        // near-tie edges, diverging far beyond tolerance. WB pushes channels >1 too.
        let (w, h) = (64u32, 64u32);
        let pixels: Vec<u16> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                (2000 + x * 600 + y * 200) as u16 // max 52400 < 65535; h-slope > v-slope
            })
            .collect();
        let wb = [1.9, 1.0, 1.5, 1.0];
        let raw = raw_rggb(w, h, pixels.clone(), wb);
        let cpu = Rcd.to_linear_rgba_f32(&raw);

        let cfa = CfaInput {
            pixels: &pixels,
            width: w,
            height: h,
            cfa_pattern: [0, 1, 1, 2],
            black_levels: [0.0; 4],
            white_level: 65535.0,
            wb_coeffs: wb,
        };
        let gpu = demosaic_rcd_gpu(&ctx, &cfa);

        assert_eq!((gpu.width, gpu.height), (w, h));
        assert_eq!(gpu.pixels.len(), cpu.pixels.len());
        // f16 output + f32 compute: compare within a small tolerance.
        let mut max_d = 0.0f32;
        for (a, b) in gpu.pixels.iter().zip(cpu.pixels.iter()) {
            max_d = max_d.max((a - b).abs());
        }
        assert!(max_d < 2e-3, "GPU RCD drifted from CPU reference: max abs diff {max_d}");
    }

    #[test]
    fn gpu_rcd_preserves_values_above_one() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        // Uniform white field + WB 2.0 on red → R channel ~2.0, carried unclamped.
        let (w, h) = (8u32, 8u32);
        let cfa = CfaInput {
            pixels: &vec![65535u16; (w * h) as usize],
            width: w,
            height: h,
            cfa_pattern: [0, 1, 1, 2],
            black_levels: [0.0; 4],
            white_level: 65535.0,
            wb_coeffs: [2.0, 1.0, 1.0, 1.0],
        };
        let gpu = demosaic_rcd_gpu(&ctx, &cfa);
        // Pixel 0 is an R site: R = 1.0 * 2.0 = 2.0 (f16-rounded), unclamped.
        assert!((gpu.pixels[0] - 2.0).abs() < 2e-3, "R must carry >1 (got {})", gpu.pixels[0]);
    }
}
```

Add to `ferrolite-pipeline/src/lib.rs`: declare `mod rcd_gpu;` (near the other `mod` lines) and re-export:

```rust
pub use rcd_gpu::{demosaic_rcd_gpu, CfaInput};
```

- [ ] **Step 4: Run the golden to verify it fails**

Run: `cargo test -p ferrolite-pipeline --lib rcd_gpu`
Expected: FAIL to compile — `cannot find function demosaic_rcd_gpu`.

- [ ] **Step 5: Write `demosaic_rcd_gpu`**

Insert into `ferrolite-pipeline/src/rcd_gpu.rs`, between the `RcdParams` struct and the `#[cfg(test)]` module:

```rust
/// Full-res RGGB GPU RCD demosaic → white-balanced, unclamped `LinearRgbaF32`.
/// Runs two compute passes (green, then chroma+WB) over storage buffers and reads
/// the `rgba16float` result back. RGGB only — the caller must gate on the pattern.
pub fn demosaic_rcd_gpu(ctx: &GpuContext, cfa: &CfaInput) -> LinearRgbaF32 {
    let w = cfa.width;
    let h = cfa.height;
    let n = (w * h) as usize;
    let device = &ctx.device;

    // CPU-normalized single-channel CFA `c` (matches ferrolite_decode::Rcd exactly):
    // black-subtract per CFA position, /span, floor at 0. NOT white-balanced (WB is
    // applied per-channel in the chroma pass, after interpolation).
    let span = (cfa.white_level - cfa.black_levels[0]).max(1.0);
    let c: Vec<f32> = (0..n)
        .map(|i| {
            let (x, y) = (i as u32 % w, i as u32 / w);
            let pos = ((y % 2) * 2 + (x % 2)) as usize;
            ((cfa.pixels[i] as f32 - cfa.black_levels[pos]) / span).max(0.0)
        })
        .collect();

    // Buffers: CFA (read), green (read_write in pass 1 → read in pass 2).
    let cfa_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("rcd-cfa"),
        contents: bytemuck::cast_slice(&c),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let green_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rcd-green"),
        size: (n * std::mem::size_of::<f32>()) as u64,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let params = RcdParams {
        width: w,
        height: h,
        pad0: 0,
        pad1: 0,
        wb: cfa.wb_coeffs,
    };
    let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("rcd-params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    // Output rgba16float storage texture (COPY_SRC for readback).
    let out_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rcd-out"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: PIPELINE_FORMAT,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });

    // --- Pass 1: green ---
    let green_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("rcd-green-bgl"),
        entries: &[
            storage_entry(0, true),  // cfa read
            storage_entry(1, false), // green read_write
            uniform_entry(2),
        ],
    });
    let green_pipe = compute_pipeline(ctx, &green_bgl, "rcd-green", include_str!("shaders/rcd_green.wgsl"));
    let green_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rcd-green-bind"),
        layout: &green_bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: cfa_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: green_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: params_buf.as_entire_binding() },
        ],
    });

    // --- Pass 2: chroma + WB ---
    let out_view = out_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let chroma_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("rcd-chroma-bgl"),
        entries: &[
            storage_entry(0, true), // cfa read
            storage_entry(1, true), // green read
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: PIPELINE_FORMAT,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            uniform_entry(3),
        ],
    });
    let chroma_pipe = compute_pipeline(ctx, &chroma_bgl, "rcd-chroma", include_str!("shaders/rcd_chroma.wgsl"));
    let chroma_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rcd-chroma-bind"),
        layout: &chroma_bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: cfa_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: green_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&out_view) },
            wgpu::BindGroupEntry { binding: 3, resource: params_buf.as_entire_binding() },
        ],
    });

    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rcd") });
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("rcd-green"), timestamp_writes: None });
        pass.set_pipeline(&green_pipe);
        pass.set_bind_group(0, &green_bind, &[]);
        pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
    }
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("rcd-chroma"), timestamp_writes: None });
        pass.set_pipeline(&chroma_pipe);
        pass.set_bind_group(0, &chroma_bind, &[]);
        pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
    }
    ctx.queue.submit([enc.finish()]);

    read_rgba16f_texture(ctx, &out_tex, w, h)
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn compute_pipeline(
    ctx: &GpuContext,
    bgl: &wgpu::BindGroupLayout,
    label: &str,
    wgsl: &str,
) -> wgpu::ComputePipeline {
    let module = ctx.shader_module(label, wgsl);
    let layout = ctx.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    ctx.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        module: &module,
        entry_point: "main",
        compilation_options: Default::default(),
        cache: None,
    })
}

/// Read an `rgba16float` texture back to a display-linear `LinearRgbaF32`
/// (row-unpadded, f16→f32). Blocks on the device — runs off the UI thread.
fn read_rgba16f_texture(ctx: &GpuContext, tex: &wgpu::Texture, w: u32, h: u32) -> LinearRgbaF32 {
    let bpp = 8u32; // rgba16float
    let bpr_unpadded = w * bpp;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bpr_padded = bpr_unpadded.div_ceil(align) * align;
    let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rcd-readback"),
        size: (bpr_padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::ImageCopyTexture { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::ImageCopyBuffer {
            buffer: &buf,
            layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(bpr_padded), rows_per_image: Some(h) },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    ctx.queue.submit([enc.finish()]);

    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    ctx.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();

    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let start = (row * bpr_padded) as usize;
        let end = start + bpr_unpadded as usize;
        let row_u16: &[u16] = bytemuck::cast_slice(&data[start..end]);
        for &hbits in row_u16 {
            px.push(f16::from_bits(hbits).to_f32());
        }
    }
    drop(data);
    buf.unmap();
    LinearRgbaF32::new(w, h, px).expect("rcd gpu readback length matches dims")
}
```

- [ ] **Step 6: Run the golden to verify it passes**

Run: `cargo test -p ferrolite-pipeline --lib rcd_gpu`
Expected on a GPU box: PASS (2 tests: `gpu_rcd_matches_cpu_reference`, `gpu_rcd_preserves_values_above_one`). On headless CI: both print "skipping" and pass trivially. If `gpu_rcd_matches_cpu_reference` FAILS with a large diff, the WGSL diverged from the CPU algorithm — debug the shader (pos mapping, neighbour offsets, WB), do NOT loosen the 2e-3 tolerance.

- [ ] **Step 7: Confirm clippy is clean**

Run: `cargo clippy -p ferrolite-pipeline --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add ferrolite-pipeline/src/rcd_gpu.rs ferrolite-pipeline/src/shaders/rcd_green.wgsl ferrolite-pipeline/src/shaders/rcd_chroma.wgsl ferrolite-pipeline/src/lib.rs ferrolite-pipeline/Cargo.toml
git commit -m "feat(pipeline): WGSL GPU RCD demosaic (two-pass; golden vs CPU Rcd)"
```

---

## Task 2: Engage GPU RCD as the on-screen full-res tier (`ferrolite-app`)

**Files:**
- Modify: `ferrolite-app/src/viewer/load.rs` (`spawn_full`)
- Modify: `ferrolite-app/src/app.rs` (the `spawn_full` call site in `drive_viewer`, ~line 3645)

**Interfaces:**
- Consumes: `ferrolite_pipeline::{demosaic_rcd_gpu, CfaInput}` (Task 1), `ferrolite_gpu::GpuContext`, existing `QuadBin`, `apply_orientation_linear`.
- Produces: RAW full decode now demosaics RGGB sensors with GPU RCD (full-res) off the UI thread; the resulting `LinearRgbaF32` flows through the unchanged `AppEvent::FullDecoded` / `apply_full_decoded` / pyramid / two-tier path, so 1:1 shows RCD detail. Non-RGGB → `QuadBin` (unchanged).

> **TDD note:** This is UI/job wiring over the Task-1 GPU function (already goldened) and the existing decode/pyramid path. It is not unit-testable in isolation (it runs a GPU job feeding the live viewer). Per CLAUDE.md its correctness is confirmed by `cargo build` + the workspace gate + the **author's visual test** (Task 3). Full before/after shown.

- [ ] **Step 1: Give `spawn_full` a `GpuContext` and use GPU RCD for RGGB**

In `ferrolite-app/src/viewer/load.rs`, change the imports at the top:

```rust
use ferrolite_decode::{DemosaicToRgb16f, QuadBin};
```
to:
```rust
use ferrolite_decode::{DemosaicToRgb16f, QuadBin};
use ferrolite_gpu::GpuContext;
use std::sync::Arc;
```

Replace the `spawn_full` signature + body's demosaic step. Change the signature:

```rust
pub fn spawn_full(
    jobs: &std::sync::Arc<JobSystem>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    path: PathBuf,
) -> JobHandle {
```
to (add `gpu`):
```rust
pub fn spawn_full(
    jobs: &std::sync::Arc<JobSystem>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    path: PathBuf,
    gpu: Arc<GpuContext>,
) -> JobHandle {
```

Inside the job closure, replace the demosaic line:

```rust
                let image = ferrolite_decode::apply_orientation_linear(
                    QuadBin.to_linear_rgba_f32(&raw),
                    raw.orientation,
                );
```
with (GPU RCD full-res for RGGB, off the UI thread; QuadBin for other patterns):

```rust
                // Full-res demosaic: GPU RCD for RGGB sensors (P2 Plan 5), QuadBin
                // otherwise. Runs here on the job worker thread (never the UI thread,
                // CLAUDE.md §1); RCD runs once per open (CLAUDE.md §2).
                let demosaiced = if raw.cfa_pattern == [0, 1, 1, 2] {
                    ferrolite_pipeline::demosaic_rcd_gpu(
                        &gpu,
                        &ferrolite_pipeline::CfaInput {
                            pixels: &raw.pixels,
                            width: raw.width,
                            height: raw.height,
                            cfa_pattern: raw.cfa_pattern,
                            black_levels: raw.black_levels,
                            white_level: raw.white_level,
                            wb_coeffs: raw.wb_coeffs,
                        },
                    )
                } else {
                    QuadBin.to_linear_rgba_f32(&raw)
                };
                let image = ferrolite_decode::apply_orientation_linear(demosaiced, raw.orientation);
```

(Update the doc comment on `spawn_full` line ~73 from "`QuadBin.to_linear_rgba_f32` (display-linear half-res)" to "GPU RCD (RGGB) / QuadBin (else), full-res".)

- [ ] **Step 2: Pass a `GpuContext` at the call site**

In `ferrolite-app/src/app.rs`, at the `spawn_full` call in `drive_viewer` (~line 3645). `drive_viewer(&mut self, ui, frame)` has `frame: &eframe::Frame`. Replace:

```rust
                    } else if !v.full_requested && v.cache_resolved {
                        let h = viewer::load::spawn_full(
                            &self.state.jobs,
                            &self.state.tx,
                            ctx,
                            v.image_id,
                            v.path.clone(),
                        );
                        v.full_handle = Some(h);
                        v.full_requested = true;
                    }
```
with (build a shared `GpuContext` from the render state and pass it in; skip this frame if the render state isn't ready yet — it will be on the next):

```rust
                    } else if !v.full_requested && v.cache_resolved {
                        if let Some(rs) = frame.wgpu_render_state() {
                            let gpu = std::sync::Arc::new(
                                ferrolite_gpu::GpuContext::from_render_state(rs),
                            );
                            let h = viewer::load::spawn_full(
                                &self.state.jobs,
                                &self.state.tx,
                                ctx,
                                v.image_id,
                                v.path.clone(),
                                gpu,
                            );
                            v.full_handle = Some(h);
                            v.full_requested = true;
                        }
                    }
```

- [ ] **Step 3: Build + confirm no stale call**

Run: `cargo build -p ferrolite-app 2>&1 | tail -20`
Expected: builds clean.

Run: `grep -rn 'spawn_full' ferrolite-app/src/ | grep -v 'fn spawn_full'`
Expected: the single call site shown above (now passing `gpu`); no other caller left without the new arg. (If any test or other caller exists, update it to pass a `GpuContext`.)

- [ ] **Step 4: Clippy**

Run: `cargo clippy -p ferrolite-app --all-targets --all-features -- -D warnings 2>&1 | tail -20`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/viewer/load.rs ferrolite-app/src/app.rs
git commit -m "feat(app): full-res tier demosaics RGGB via GPU RCD (QuadBin fallback)"
```

---

## Task 3: Workspace green gate + visual-test handoff

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all && cargo fmt --check`
Expected: no diff.

- [ ] **Step 2: Clippy (CI-equivalent gate)**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: no warnings/errors.

- [ ] **Step 3: Full workspace test**

Run: `cargo test --workspace`
Expected: PASS — on a GPU box the two new `rcd_gpu` goldens run; headless skips them; all existing tests stay green (QuadBin, CPU `Rcd`, color/gamut goldens unchanged).

- [ ] **Step 4: Commit any formatting fixups**

```bash
git add -A && git commit -m "chore: cargo fmt for P2 plan 5" || echo "nothing to format"
```

- [ ] **Step 5: STOP — hand the author the visual test**

Per CLAUDE.md "Finishing a branch", present this and **hold** for Jann's hands-on results before merging/PR-ing:

**Visual test (real surface this plan):**
1. **Open the RGGB fixture `fixtures/raw/sample.rw2`** (or any RGGB RAW). Let the full decode settle (the brief debounce after open).
2. **Zoom to 1:1 / 100%.** The on-screen image should now show **full-resolution RCD detail** — noticeably sharper, with finer texture and fewer stair-step/zipper artifacts on diagonal edges than before (the on-screen tier was previously half-res QuadBin). Colour/brightness should match the Develop view as before (only the demosaic changed).
   - **Failure signatures:** looks soft / half-res / unchanged (GPU RCD not engaging — check the RGGB gate / job); **maze or zipper artifacts** worse than QuadBin (WGSL diverged from the CPU algorithm); a **colour cast** vs the previous view (chroma/WB bug); **stutter/freeze on open or while zooming/navigating** (the GPU RCD must run in the job, not the UI thread — a freeze means it leaked onto the UI thread).
3. **Navigate between images (arrow keys).** Each RGGB RAW should reveal (tier-1 preview instantly), then sharpen to RCD full-res shortly after — **no freeze or hitch** on open/navigation.
4. **Export** the image — it already applies RCD (Plan-4 CPU path); confirm the exported detail still looks right (unchanged by this plan).
5. **Non-RGGB sanity (optional):** a non-RGGB RAW (X-Trans / BGGR) should still open fine via the QuadBin fallback (no crash, no regression).

Do NOT merge/PR until Jann confirms 1:1 shows RCD detail with no artifacts/colour shift and no freeze. Address any issue found, then re-run the gate.

---

## Self-Review

**Spec coverage (§5.2 + §9 contracts + §10 + the visual test):**
- "RCD WGSL compute pass as a photo-tier `ferrolite-pipeline` node; generic executor untouched (§4)" → Task 1 (two WGSL compute passes in `ferrolite-pipeline`; no `ferrolite-gpu`/executor change). ✓
- "raw CFA uploaded as a single-channel GPU source (format + pattern-offset uniform §8)" → Task 1 uploads the normalized CFA as a single-channel storage buffer; pattern offset is `(0,0)` for whole-image (documented in Resolved design questions). ✓ (deviation noted)
- "halo-consuming via the VT halo for tiled full-res/1:1/export (§5)" → **deviation (Option W, author-approved):** RCD runs whole-image once, so no per-tile RCD halo; the VT and downstream halo are untouched. 1:1/full-res detail is delivered via the full-res pyramid; export via Plan-4 CPU RCD. Documented in Resolved design questions.
- "automatic two-tier (QuadBin preview / RCD full-res)" → Task 2: tier-1 reveal unchanged (instant), tier-2 full decode = RCD full-res; automatic, no control, no persisted state. ✓
- "GPU golden vs the Plan-4 CPU reference" → Task 1 `gpu_rcd_matches_cpu_reference` (+ `gpu_rcd_preserves_values_above_one`). ✓
- §10 GPU goldens (auto-skip headless) ✓; unclamped carry preserved ✓; RGGB-only + fallback ✓.
- CLAUDE.md §1 (GPU RCD in the job, off UI thread) ✓; §2 (once per open; downstream built-once unchanged) ✓.

**Placeholder scan:** No TBD/TODO/"handle edge cases"; complete WGSL + Rust + test code; every run step states expected output; the one non-TDD task (Task 2) is shown in full before/after and gated by build + visual test. The Option-W deviations from the spec's tiled-CFA letter are called out explicitly (author-approved), not hidden.

**Type consistency:** `CfaInput` fields match the app's `RawDecoded` fields threaded in Task 2 (`pixels`/`width`/`height`/`cfa_pattern`/`black_levels`/`white_level`/`wb_coeffs`). `demosaic_rcd_gpu(&GpuContext, &CfaInput) -> LinearRgbaF32` matches its call in `spawn_full` and its golden test. `RcdParams` layout (`width,height,pad0,pad1,wb`) matches the WGSL `Params` struct in both shaders. Normalization (`span`, `pos`, black floor, WB-after) matches `ferrolite_decode::Rcd` verbatim so the golden holds. `spawn_full`'s new `gpu: Arc<GpuContext>` param matches the single call site.
