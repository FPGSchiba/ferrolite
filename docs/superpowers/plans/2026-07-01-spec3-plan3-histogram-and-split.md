# Spec 3 Plan 3 — GPU histogram (compute + async readback + widget) & before/after split (draggable divider + toolbar toggle)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a live GPU histogram — a compute pass over the on-screen preview texture that fills a `256 × {R,G,B,luma}` bin buffer via atomics in **display-referred** space, read back asynchronously (~4 KB) over the app event channel and drawn in the Develop adjustment panel — and a **before/after split** view: `OpStack::default()` (before) rendered left of a **draggable vertical divider**, the current stack right, toggled by a Develop-toolbar button (the `\` momentary full-before key stays).

**Architecture:** The histogram is a generic engine-tier `ferrolite_vt::HistogramPipeline` (built once, pre-warmed) that reads any `Rgba16Float` texture, applies a `working→display` 3×3 (row-major `[[f32;3];3]`, same as the display tail) + the sRGB OETF, and `atomicAdd`s into a 1024-`u32` storage buffer; only that buffer is copied to a `MAP_READ` staging buffer and mapped async. The app debounces recompute, dispatches on the render thread, and delivers `AppEvent::HistogramReady` back to the UI. The split reuses the existing rung-1 display path: the "after" is the normal preview callback; a **second** paint callback wrapping a `color_convert(preview_source, sRGB→working)` "before" texture is drawn with an egui **clip rect** to the left of the divider (same callback `rect` ⇒ identical image geometry, different scissor). All divider position/hit-test/drag math is a pure `f32` unit. The generic `Graph<PipelineImage>` executor and `ferrolite-color`/`ferrolite-decode` are **not** touched; `ferrolite-vt` stays photo-agnostic (it receives a plain `[[f32;3];3]`).

**Tech Stack:** Rust, `wgpu` 22 (WGSL compute + atomics, `map_async` readback, `clear_buffer`), `bytemuck` Pod uniforms, `egui`/`eframe`/`egui_wgpu` (paint callbacks + `Painter::with_clip_rect`), `ferrolite-color` (`working_to_display`), `ferrolite-pipeline` (`color_convert`, `PipelineImage`), `half::f16`.

## Global Constraints

- **Never block the UI/update thread (CLAUDE.md §1):** the histogram reads back only the ~4 KB (`256 × 4 × u32 = 4096 B`) bin buffer via `map_async` — **NO whole-image GPU→CPU readback**. The per-pixel decode already happened; the compute runs on the GPU. Mapping is polled non-blocking (`Maintain::Poll`) on the render thread, never `Maintain::Wait` in the app (only in tests).
- **Build GPU pipelines once (CLAUDE.md §2):** the histogram compute pipeline is built **once** in `HistogramPipeline::new` and pre-warmed at startup in `FerroliteApp::new` alongside `DisplayPipelines`/`prewarm_shaders`. Never rebuilt per image/open/edit. Recompute is a `queue.write_buffer` + dispatch, **debounced** (`HIST_DEBOUNCE`), triggered on preview recompute.
- **Licensing tiers (spec §3):** `ferrolite-vt` is engine-transferable — it MUST NOT depend on `ferrolite-color` or carry a photo concept. The histogram pass is generic: it takes the transform as a plain **row-major `[[f32; 3]; 3]`** and hardcodes the sRGB OETF (same as `display.wgsl`). Only `ferrolite-app` (photo tier) composes the matrix via `ferrolite_color::working_to_display`.
- **Matrix packing:** reuse `ferrolite_vt::pipelines::pack_display_matrix` (row-major `[[f32;3];3]` → WGSL column-major padded `[[f32;4];3]`, so in-shader `M * v == m · v`). Same rule as Plan 2.
- **Display-referred histogram (spec §7.1):** bin **after** applying `working→display` **and** the sRGB OETF, so the histogram matches exactly what the display shader puts on screen. The preview texture is already working-space linear (Plan 2). Luma = Rec.709 weights `(0.2126, 0.7152, 0.0722)` on the display-referred RGB.
- **Read-only histogram (CLAUDE.md per-component reset):** the histogram is display-only; **no** per-control reset applies and it is not an editable op.
- **Split is preview-tier (spec §7.2):** the split renders only while the preview (rung-1) is shown; at 1:1 (`show_full`) the after-view is shown and the split is skipped (logged once), never blocks. Before = `OpStack::default()` through the same DAG (identity ops + `sRGB→working` = exactly `ferrolite_pipeline::color_convert(preview_source, preview_to_working())`).
- **`\` unchanged (spec §7.2):** the Backslash key stays the momentary full-before toggle (`ViewerState.before_after`, a full swap). The split is a **separate** state (`split_compare`) toggled by the Develop-toolbar button.
- **Divider math is a pure tested unit (spec §7.2):** position clamp, hit-test, and drag live in `ferrolite-app/src/develop/split.rs` as plain `f32` functions (no egui types); egui only routes pointer events into them.
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → then STOP and hold for Jann's hands-on visual test before finishing the branch (CLAUDE.md "Finishing a branch").
- **GPU goldens auto-skip headless:** every GPU test starts with `let Some(ctx) = GpuContext::headless() else { return; };` (spec §10). `cargo test --workspace` stays green on headless CI.
- **Branch:** continue on `feat/color-and-export`. Conventional-commit messages, no attribution footer (disabled globally).

---

## File Structure

**`ferrolite-vt`** (engine tier — NO `ferrolite-color`):
- Create `ferrolite-vt/src/histogram.rs` — `HistogramPipeline` (built once), `HistParams` uniform, `bin_index`, `HIST_BINS`/`HIST_CHANNELS`/`HIST_LEN` consts, `#[cfg(test)]` unit tests.
- Create `ferrolite-vt/src/shaders/histogram.wgsl` — the compute pass (matrix + sRGB OETF + atomic binning).
- Modify `ferrolite-vt/src/lib.rs` — `mod histogram;` + re-exports.
- Modify `ferrolite-vt/src/view.rs` — `VirtualTexture::single_texture_arc()` accessor.
- Create `ferrolite-vt/tests/histogram_golden.rs` — GPU compute vs CPU reference (auto-skip headless).

**`ferrolite-app`** (photo tier):
- Modify `ferrolite-app/src/viewer/callback.rs` — `ViewerPipelines.histogram`, `PreviewWhich`, `ViewerCallback.which`, `ViewerGpu.preview_before`.
- Modify `ferrolite-app/src/viewer/mod.rs` — `HistogramState`; `ViewerState.histogram`/`split_compare`/`split_pos`/`split_full_logged`.
- Modify `ferrolite-app/src/events.rs` — `AppEvent::HistogramReady`.
- Modify `ferrolite-app/src/app.rs` — pre-warm the histogram pipeline; `maybe_update_histogram` (debounced dispatch + async readback); `HistogramReady` handling; mark histogram dirty on preview recompute; `ensure_before_view`; split rendering + divider interaction in `drive_viewer`; `apply_working_space` invalidates `preview_before`; `apply_preview_ready` seeds `preview_before: None`.
- Create `ferrolite-app/src/develop/histogram_widget.rs` — the panel widget + pure `peak_bin`/`channel_norm` (+ tests).
- Create `ferrolite-app/src/develop/split.rs` — pure divider math (+ tests).
- Modify `ferrolite-app/src/develop/mod.rs` — `pub mod histogram_widget;` + `pub mod split;`.
- Modify `ferrolite-app/src/develop/adjustment_panel.rs` — draw the histogram widget below the working-space selector.
- Modify `ferrolite-app/src/library/develop_filter_bar.rs` — the split-compare toolbar toggle button.

---

## Task 1: `ferrolite-vt` histogram compute pipeline (engine tier)

Build the generic, once-built histogram compute pass and its texture accessor, with a pure `bin_index` unit test and a GPU golden vs a CPU reference.

**Files:**
- Create: `ferrolite-vt/src/shaders/histogram.wgsl`
- Create: `ferrolite-vt/src/histogram.rs`
- Modify: `ferrolite-vt/src/lib.rs`
- Modify: `ferrolite-vt/src/view.rs` (add `single_texture_arc`)
- Create: `ferrolite-vt/tests/histogram_golden.rs`

**Interfaces:**
- Produces: `pub const HIST_BINS: usize = 256;`, `pub const HIST_CHANNELS: usize = 4;`, `pub const HIST_LEN: usize = 1024;`
- Produces: `pub fn bin_index(v: f32) -> u32` (clamp `[0,1]` → `[0,255]`).
- Produces: `pub struct HistogramPipeline` with `pub fn new(ctx: &GpuContext) -> Self`, `pub fn dispatch(&self, ctx: &GpuContext, texture: &wgpu::Texture, dims: (u32, u32), display_matrix: [[f32; 3]; 3])`, `pub fn read_async(&self, on_ready: impl FnOnce(Vec<u32>) + Send + 'static)`.
- Produces: `VirtualTexture::single_texture_arc(&self) -> Option<std::sync::Arc<wgpu::Texture>>`.
- Consumes: `crate::pipelines::pack_display_matrix` (Plan 2, already `pub`); `ferrolite_gpu::GpuContext` (`.device`, `.queue`).

- [ ] **Step 1: Write the failing `bin_index` unit test**

Create `ferrolite-vt/src/histogram.rs` with just the test module (the impl comes in Step 3):

```rust
//! Generic GPU histogram compute pass over an `Rgba16Float` texture. Engine-tier:
//! it takes the working→display transform as a plain row-major `[[f32;3];3]` and
//! hardcodes the sRGB OETF (matching `display.wgsl`) — no photo concepts. Fills a
//! `256 × {R,G,B,luma}` bin buffer via atomics in display-referred space and reads
//! back only the ~4 KB buffer (never the image), per CLAUDE.md §1.

#[cfg(test)]
mod tests {
    use super::bin_index;

    #[test]
    fn bin_index_maps_range_and_clamps() {
        assert_eq!(bin_index(0.0), 0);
        assert_eq!(bin_index(1.0), 255);
        assert_eq!(bin_index(-0.5), 0, "below range clamps to 0");
        assert_eq!(bin_index(2.0), 255, "above range clamps to 255");
        // Round-to-nearest: 0.5 * 255 = 127.5 -> 128.
        assert_eq!(bin_index(0.5), 128);
    }
}
```

- [ ] **Step 2: Run it to verify failure**

Run: `cargo test -p ferrolite-vt bin_index -- --nocapture`
Expected: FAIL — `cannot find function bin_index` (module not yet wired into `lib.rs`; compile error is the failure).

- [ ] **Step 3: Implement `bin_index`, `HistParams`, and `HistogramPipeline`**

Prepend to `ferrolite-vt/src/histogram.rs` (before the `#[cfg(test)]` module):

```rust
use std::sync::Arc;

use ferrolite_gpu::GpuContext;
use wgpu::util::DeviceExt;

use crate::pipelines::pack_display_matrix;

pub const HIST_BINS: usize = 256;
pub const HIST_CHANNELS: usize = 4; // R, G, B, luma
pub const HIST_LEN: usize = HIST_BINS * HIST_CHANNELS; // 1024
const BINS_BYTES: u64 = (HIST_LEN * std::mem::size_of::<u32>()) as u64; // 4096

/// Quantize a display-referred `[0,1]` value to a `[0,255]` bin (round-to-nearest,
/// clamped). MUST match the WGSL `bin_index` in `histogram.wgsl`.
pub fn bin_index(v: f32) -> u32 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5).clamp(0.0, 255.0) as u32
}

/// Uniform for the histogram pass: the working→display 3×3 (WGSL column-major,
/// padded) + the image dims. 64 bytes: mat3x3 (48) + vec2<u32> (8, at offset 48)
/// + 8 pad, matching WGSL `struct Params { m: mat3x3<f32>, dims: vec2<u32> }`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HistParams {
    m: [[f32; 4]; 3],
    dims: [u32; 2],
    _pad: [u32; 2],
}

/// Once-built histogram compute pipeline + its 4 KB bin/staging buffers. Reused
/// for every recompute (CLAUDE.md: build pipelines once). `dispatch` submits its
/// own command buffer; `read_async` maps the staging buffer without blocking.
pub struct HistogramPipeline {
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    bins: wgpu::Buffer,
    staging: Arc<wgpu::Buffer>,
    params: wgpu::Buffer,
}

impl HistogramPipeline {
    pub fn new(ctx: &GpuContext) -> Self {
        let device = &ctx.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vt-histogram"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/histogram.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vt-histogram-bgl"),
            entries: &[
                // 0: source texture (read via textureLoad; non-filterable).
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
                // 1: atomic bins storage (read_write).
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(BINS_BYTES),
                    },
                    count: None,
                },
                // 2: params uniform (matrix + dims).
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<HistParams>() as u64,
                        ),
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vt-histogram-layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("vt-histogram"),
            layout: Some(&layout),
            module: &module,
            entry_point: "bin",
            compilation_options: Default::default(),
            cache: None,
        });
        let bins = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-histogram-bins"),
            size: BINS_BYTES,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let staging = Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-histogram-staging"),
            size: BINS_BYTES,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        }));
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-histogram-params"),
            size: std::mem::size_of::<HistParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, bgl, bins, staging, params }
    }

    /// Zero the bins, bin `texture` (display-referred, via `display_matrix` +
    /// sRGB OETF), then copy the 4 KB bin buffer to the staging buffer. Submits
    /// its own command buffer. Call `read_async` afterwards to fetch the result.
    pub fn dispatch(
        &self,
        ctx: &GpuContext,
        texture: &wgpu::Texture,
        dims: (u32, u32),
        display_matrix: [[f32; 3]; 3],
    ) {
        let params = HistParams {
            m: pack_display_matrix(display_matrix),
            dims: [dims.0, dims.1],
            _pad: [0, 0],
        };
        ctx.queue
            .write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-histogram-bind"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.bins.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vt-histogram-enc"),
            });
        enc.clear_buffer(&self.bins, 0, None);
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vt-histogram-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(dims.0.div_ceil(8), dims.1.div_ceil(8), 1);
        }
        enc.copy_buffer_to_buffer(&self.bins, 0, &self.staging, 0, BINS_BYTES);
        ctx.queue.submit([enc.finish()]);
    }

    /// Map the staging buffer asynchronously and hand the 1024-entry bin vector to
    /// `on_ready` when the GPU work completes and the device is polled. Never
    /// blocks. The caller must keep the device polled (`Maintain::Poll`) until the
    /// callback fires (the app does this while a readback is in flight).
    pub fn read_async(&self, on_ready: impl FnOnce(Vec<u32>) + Send + 'static) {
        let staging = self.staging.clone();
        self.staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |res| {
                if res.is_err() {
                    return;
                }
                let data = staging.slice(..).get_mapped_range();
                let bins: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&data).to_vec();
                drop(data);
                staging.unmap();
                on_ready(bins);
            });
    }
}
```

- [ ] **Step 4: Write the histogram WGSL shader**

Create `ferrolite-vt/src/shaders/histogram.wgsl`:

```wgsl
// Generic display-referred histogram: bin the display value (working->display 3x3
// + sRGB OETF, matching display.wgsl) into 256 x {R,G,B,luma} atomic bins.
struct Params {
    m: mat3x3<f32>,
    dims: vec2<u32>,
};
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>, 1024u>;
@group(0) @binding(2) var<uniform> p: Params;

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

fn bin_index(v: f32) -> u32 {
    let x = clamp(v, 0.0, 1.0);
    return u32(clamp(x * 255.0 + 0.5, 0.0, 255.0));
}

@compute @workgroup_size(8, 8, 1)
fn bin(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.dims.x || gid.y >= p.dims.y) { return; }
    let c = textureLoad(src, vec2<i32>(i32(gid.x), i32(gid.y)), 0);
    let disp = linear_to_srgb(clamp(p.m * c.rgb, vec3(0.0), vec3(1.0)));
    let luma = dot(disp, vec3<f32>(0.2126, 0.7152, 0.0722));
    atomicAdd(&bins[0u * 256u + bin_index(disp.r)], 1u);
    atomicAdd(&bins[1u * 256u + bin_index(disp.g)], 1u);
    atomicAdd(&bins[2u * 256u + bin_index(disp.b)], 1u);
    atomicAdd(&bins[3u * 256u + bin_index(luma)], 1u);
}
```

- [ ] **Step 5: Wire the module + accessor + re-exports**

In `ferrolite-vt/src/lib.rs`, add `mod histogram;` after `mod source;` (keep alpha-ish order) and extend the re-exports:
```rust
mod histogram;
```
and after the existing `pub use` lines add:
```rust
pub use histogram::{bin_index, HistogramPipeline, HIST_BINS, HIST_CHANNELS, HIST_LEN};
```

In `ferrolite-vt/src/view.rs`, add the accessor right after `single_dims` (line ~274):
```rust
    /// The current rung-1 texture (`Rgba16Float`) as an `Arc`, for a compute pass
    /// (e.g. the histogram) that reads what is on screen. `None` on a non-single VT.
    pub fn single_texture_arc(&self) -> Option<std::sync::Arc<wgpu::Texture>> {
        self.single.as_ref().map(|s| s.texture.clone())
    }
```

- [ ] **Step 6: Run the `bin_index` test to green**

Run: `cargo test -p ferrolite-vt bin_index -- --nocapture`
Expected: PASS (`bin_index_maps_range_and_clamps`).

- [ ] **Step 7: Write the GPU golden (compute vs CPU reference)**

Create `ferrolite-vt/tests/histogram_golden.rs`:

```rust
//! GPU histogram compute vs a CPU reference over the same image. Auto-skips when
//! no GPU adapter is present (headless CI).

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_vt::{bin_index, DisplayPipelines, HistogramPipeline, VirtualTexture, HIST_LEN};
use half::f16;

fn srgb_oetf(l: f32) -> f32 {
    if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// CPU reference: round through f16 (the texture is Rgba16Float), apply the sRGB
/// OETF (identity matrix), bin R,G,B,luma. Must mirror histogram.wgsl exactly.
fn cpu_histogram(img: &LinearRgbaF32) -> Vec<u32> {
    let mut bins = vec![0u32; HIST_LEN];
    let n = (img.width * img.height) as usize;
    for i in 0..n {
        let r = f16::from_f32(img.pixels[i * 4]).to_f32();
        let g = f16::from_f32(img.pixels[i * 4 + 1]).to_f32();
        let b = f16::from_f32(img.pixels[i * 4 + 2]).to_f32();
        let dr = srgb_oetf(r.clamp(0.0, 1.0));
        let dg = srgb_oetf(g.clamp(0.0, 1.0));
        let db = srgb_oetf(b.clamp(0.0, 1.0));
        let luma = 0.2126 * dr + 0.7152 * dg + 0.0722 * db;
        bins[bin_index(dr) as usize] += 1;
        bins[256 + bin_index(dg) as usize] += 1;
        bins[512 + bin_index(db) as usize] += 1;
        bins[768 + bin_index(luma) as usize] += 1;
    }
    bins
}

/// Values chosen to sit well away from bin midpoints so f16 rounding + the OETF
/// land GPU and CPU in the same bin (exact equality, no tolerance needed).
fn probe_image() -> LinearRgbaF32 {
    let px = vec![
        0.20, 0.40, 0.60, 1.0, //
        0.50, 0.10, 0.30, 1.0, //
        0.05, 0.25, 0.45, 1.0, //
        0.80, 0.55, 0.15, 1.0, //
    ];
    LinearRgbaF32::new(2, 2, px).unwrap()
}

#[test]
fn histogram_compute_matches_cpu_reference() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let img = probe_image();
    // Upload via the real rung-1 path (f16), then bin the resulting texture.
    let pipelines = DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    let vt = VirtualTexture::single_texture(&ctx, &img, &pipelines);
    let tex = vt.single_texture_arc().expect("single texture");
    let hist = HistogramPipeline::new(&ctx);
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    hist.dispatch(&ctx, &tex, (img.width, img.height), identity);

    let (tx, rx) = std::sync::mpsc::channel();
    hist.read_async(move |bins| {
        let _ = tx.send(bins);
    });
    ctx.device.poll(wgpu::Maintain::Wait); // block in-test only; app uses Poll
    let gpu_bins = rx.recv().expect("readback delivered");

    let cpu_bins = cpu_histogram(&img);
    assert_eq!(gpu_bins.len(), HIST_LEN);
    // Conservation: each channel counts every pixel exactly once.
    for ch in 0..4 {
        let sum: u32 = gpu_bins[ch * 256..ch * 256 + 256].iter().sum();
        assert_eq!(sum, 4, "channel {ch} must total the pixel count");
    }
    assert_eq!(gpu_bins, cpu_bins, "GPU histogram must match the CPU reference");
}
```

Ensure `half` is available to the test — `ferrolite-vt/Cargo.toml` already depends on `half` (used in `view.rs`); if it is only a normal dependency it is still visible to integration tests. If `cargo test` reports `unresolved import half`, add `half = { workspace = true }` under `[dev-dependencies]`.

- [ ] **Step 8: Run the golden (local GPU)**

Run: `cargo test -p ferrolite-vt --test histogram_golden -- --nocapture`
Expected: PASS on the dev GPU; prints "skipping" and passes on headless CI.

- [ ] **Step 9: Build + clippy the crate**

Run: `cargo clippy -p ferrolite-vt --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 10: Commit**

```bash
git add ferrolite-vt/src/histogram.rs ferrolite-vt/src/shaders/histogram.wgsl ferrolite-vt/src/lib.rs ferrolite-vt/src/view.rs ferrolite-vt/tests/histogram_golden.rs ferrolite-vt/Cargo.toml
git commit -m "feat(vt): generic GPU histogram compute pass + async 4KB readback"
```

---

## Task 2: App wiring — pre-warm, debounced dispatch, async delivery

Pre-warm the histogram pipeline at startup, add the `HistogramReady` event, keep per-viewer histogram state, and drive a debounced compute over the on-screen preview texture each frame, delivering the bins back over the app channel.

**Files:**
- Modify: `ferrolite-app/src/viewer/callback.rs` (`ViewerPipelines.histogram`)
- Modify: `ferrolite-app/src/viewer/mod.rs` (`HistogramState` + `ViewerState.histogram`)
- Modify: `ferrolite-app/src/events.rs` (`AppEvent::HistogramReady`)
- Modify: `ferrolite-app/src/app.rs` (pre-warm, `maybe_update_histogram`, event handling, dirty marking)

**Interfaces:**
- Produces: `HistogramState { pub bins: Option<Vec<u32>>, dirty: bool, inflight: bool, since_dirty: f32 }` with `HistogramState::new()`, `mark_dirty(&mut self)`, `tick(&mut self, dt: f32)`, `should_dispatch(&self) -> bool`.
- Produces: `AppEvent::HistogramReady { image_id: i64, bins: Vec<u32> }`.
- Produces: `FerroliteApp::maybe_update_histogram(&mut self, ctx: &egui::Context, frame: &eframe::Frame)`; `FerroliteApp::mark_histogram_dirty(&mut self)`.
- Consumes: `ferrolite_vt::HistogramPipeline`, `VirtualTexture::single_texture_arc`, `ferrolite_color::working_to_display`.

- [ ] **Step 1: Add the histogram pipeline to `ViewerPipelines`**

In `ferrolite-app/src/viewer/callback.rs`, extend the holder (line ~18):
```rust
pub struct ViewerPipelines {
    pub pipelines: DisplayPipelines,
    /// Once-built histogram compute pipeline (pre-warmed at startup, reused).
    pub histogram: ferrolite_vt::HistogramPipeline,
}
```

- [ ] **Step 2: Pre-warm it at startup**

In `ferrolite-app/src/app.rs` `FerroliteApp::new` (the `if let Some(rs)` block, ~line 33), change the `ViewerPipelines` insertion:
```rust
            let pipelines = ferrolite_vt::DisplayPipelines::new(&gpu, rs.target_format);
            let histogram = ferrolite_vt::HistogramPipeline::new(&gpu);
            rs.renderer
                .write()
                .callback_resources
                .insert(viewer::ViewerPipelines {
                    pipelines,
                    histogram,
                });
```

- [ ] **Step 3: Add `HistogramState` + fields to `ViewerState`**

In `ferrolite-app/src/viewer/mod.rs`, add near the top (after `CROSSFADE_SECS`):
```rust
/// Debounce (seconds) between a preview recompute and the histogram dispatch, so
/// a slider drag coalesces into one compute rather than one per frame.
pub const HIST_DEBOUNCE: f32 = 0.10;

/// Per-viewer histogram state: the last delivered bins (1024 = 256×{R,G,B,luma}),
/// plus debounce + in-flight bookkeeping. `bins` is drawn read-only in the panel.
pub struct HistogramState {
    pub bins: Option<Vec<u32>>,
    dirty: bool,
    inflight: bool,
    since_dirty: f32,
}

impl HistogramState {
    pub fn new() -> Self {
        Self {
            bins: None,
            dirty: true, // compute once as soon as the preview is up
            inflight: false,
            since_dirty: 0.0,
        }
    }

    /// A preview recompute happened: recompute the histogram after the debounce.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.since_dirty = 0.0;
    }

    /// Advance the debounce timer (only while dirty).
    pub fn tick(&mut self, dt: f32) {
        if self.dirty {
            self.since_dirty += dt;
        }
    }

    /// True when a fresh compute should be dispatched now.
    pub fn should_dispatch(&self) -> bool {
        self.dirty && !self.inflight && self.since_dirty >= HIST_DEBOUNCE
    }
}

impl Default for HistogramState {
    fn default() -> Self {
        Self::new()
    }
}
```

Add the field to `struct ViewerState` (after `ops_read_handle`):
```rust
    /// Live GPU histogram of the on-screen preview (spec §7.1).
    pub histogram: HistogramState,
```
Initialize it in `ViewerState::open` (after `ops_read_handle: None,`):
```rust
            histogram: HistogramState::new(),
```

- [ ] **Step 4: Add the `HistogramReady` event**

In `ferrolite-app/src/events.rs`, add a variant to `enum AppEvent` (after `OpsSaved`):
```rust
    /// An off-thread (async `map_async`) histogram readback finished: 1024 bins
    /// (256 × {R,G,B,luma}). Handled in `app.rs` (stores into the viewer); the
    /// `apply` fold ignores it.
    HistogramReady { image_id: i64, bins: Vec<u32> },
```
Add the fold arm (with the other `.. => None` arms, near line 110):
```rust
            AppEvent::HistogramReady { .. } => None,
```

- [ ] **Step 5: Handle `HistogramReady` in the app event match**

In `ferrolite-app/src/app.rs`, find the event `match` (grep `AppEvent::FullFailed` — the arms live together, ~line 620–760). Add:
```rust
                crate::events::AppEvent::HistogramReady { image_id, bins } => {
                    if let Some(v) = self.state.viewer.as_mut() {
                        if v.image_id == *image_id {
                            v.histogram.bins = Some(bins.clone());
                            v.histogram.inflight = false;
                        }
                    }
                    ctx.request_repaint();
                }
```
(If the match is over an owned event rather than `&event`, drop the `*`/`.clone()` accordingly — match the neighbouring arms' style.)

- [ ] **Step 6: Add `mark_histogram_dirty` and mark on every preview recompute**

In `ferrolite-app/src/app.rs` `impl FerroliteApp`, add:
```rust
    /// Flag the histogram stale so the next frame recomputes it (debounced).
    fn mark_histogram_dirty(&mut self) {
        if let Some(v) = self.state.viewer.as_mut() {
            v.histogram.mark_dirty();
        }
    }
```
Call it at the end of each method that changes what the preview shows:
- `apply_preview_ready` — add `self.mark_histogram_dirty();` as the final statement (after the `ViewerGpu` insert; the `&mut self.state.viewer` borrow from `v` has ended by then — if the borrow checker complains, the insert block already dropped `v`, so this is fine at function end).
- `set_preview_and_full` — add `self.mark_histogram_dirty();` at the very end of the method.
- `apply_full_decoded` — add `self.mark_histogram_dirty();` at the very end.
- `apply_working_space` — add `self.mark_histogram_dirty();` just before the final `ctx.request_repaint();`.

- [ ] **Step 7: Implement `maybe_update_histogram`**

In `ferrolite-app/src/app.rs` `impl FerroliteApp`, add:
```rust
    /// Debounced GPU histogram recompute over the on-screen preview texture.
    /// While a readback is in flight, poll the device (non-blocking) so the
    /// `map_async` callback fires and keep repainting until it delivers. Never
    /// blocks the UI thread and never reads back the image (only the 4 KB bins).
    fn maybe_update_histogram(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let dt = ctx.input(|i| i.stable_dt);
        let (inflight, do_dispatch, image_id) = {
            let Some(v) = self.state.viewer.as_mut() else {
                return;
            };
            v.histogram.tick(dt);
            (v.histogram.inflight, v.histogram.should_dispatch(), v.image_id)
        };

        // A readback is pending: drive the map callback + keep the frame loop alive.
        if inflight {
            let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
            gpu.device.poll(wgpu::Maintain::Poll);
            ctx.request_repaint();
            return;
        }
        if !do_dispatch {
            return;
        }

        let matrix = ferrolite_color::working_to_display(self.state.working_space);
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
        let dispatched = {
            let renderer = rs.renderer.read();
            let Some(g) = renderer.callback_resources.get::<viewer::ViewerGpu>() else {
                return;
            };
            if g.image_id != image_id {
                return;
            }
            let (Some(tex), Some(dims)) = (g.preview.single_texture_arc(), g.preview.single_dims())
            else {
                return;
            };
            let Some(vp) = renderer.callback_resources.get::<viewer::ViewerPipelines>() else {
                return;
            };
            vp.histogram.dispatch(&gpu, &tex, dims, matrix);
            let tx = self.state.tx.clone();
            let egui_ctx = ctx.clone();
            vp.histogram.read_async(move |bins| {
                let _ = tx.send(crate::events::AppEvent::HistogramReady { image_id, bins });
                egui_ctx.request_repaint();
            });
            true
        };
        if dispatched {
            if let Some(v) = self.state.viewer.as_mut() {
                v.histogram.inflight = true;
                v.histogram.dirty = false;
            }
            // Poll now so the just-submitted work can complete promptly.
            gpu.device.poll(wgpu::Maintain::Poll);
            ctx.request_repaint();
        }
    }
```

- [ ] **Step 8: Call it each Develop frame**

In `ferrolite-app/src/app.rs`, in the Develop update path, call `maybe_update_histogram` once per frame while a viewer is open. Put it right after the `drive_viewer(ui, frame)` call site is not ideal (that is inside a `CentralPanel` closure borrowing `self`); instead call it in the Develop branch **before** the `CentralPanel` — next to the ops-read submission block (after line ~1185, the `if let Some(v) = self.state.viewer.as_mut()` decode-submit block closes). Add:
```rust
        if self.module == crate::module::Module::Develop && self.state.viewer.is_some() {
            self.maybe_update_histogram(ctx, frame);
        }
```
(Place this before the `if self.module == ... { ... SidePanel::right("develop_adjust") ... }` block at line ~1187 so the panel draws the freshest `bins`.)

- [ ] **Step 9: Build + clippy**

Run:
```bash
cargo build -p ferrolite-app
cargo clippy -p ferrolite-app --all-targets -- -D warnings
```
Expected: clean. (The dispatch/readback path is exercised by Jann's visual test; the numeric correctness is covered by Task 1's golden.)

- [ ] **Step 10: Commit**

```bash
git add ferrolite-app/src/viewer/callback.rs ferrolite-app/src/viewer/mod.rs ferrolite-app/src/events.rs ferrolite-app/src/app.rs
git commit -m "feat(app): debounced GPU histogram dispatch + async readback delivery"
```

---

## Task 3: Histogram widget in the Develop adjustment panel

Draw the four-channel histogram in the panel's histogram area (below the working-space selector, above Basic), read-only. A pure `peak_bin` + `channel_norm` unit is tested; the painting is visual-only.

**Files:**
- Create: `ferrolite-app/src/develop/histogram_widget.rs`
- Modify: `ferrolite-app/src/develop/mod.rs`
- Modify: `ferrolite-app/src/develop/adjustment_panel.rs`

**Interfaces:**
- Produces: `pub fn peak_bin(bins: &[u32]) -> u32`; `pub fn channel_norm(bins: &[u32], channel: usize, peak: u32) -> Vec<f32>`; `pub fn show(ui: &mut egui::Ui, bins: Option<&[u32]>)`.
- Consumes: `ferrolite_vt::{HIST_BINS, HIST_LEN}`.

- [ ] **Step 1: Write the failing pure-helper tests**

Create `ferrolite-app/src/develop/histogram_widget.rs`:

```rust
//! Read-only four-channel (R,G,B,luma) histogram drawn in the Develop adjustment
//! panel (design-system §6). The pixel counts come from the GPU compute pass
//! (`ferrolite_vt::HistogramPipeline`) via `AppEvent::HistogramReady`; this widget
//! only normalizes + paints them. Not an editable op → no per-control reset.

use ferrolite_vt::{HIST_BINS, HIST_LEN};

/// The largest bin count across all channels (min 1 to avoid divide-by-zero),
/// used as the common vertical scale so channels are comparable.
pub fn peak_bin(bins: &[u32]) -> u32 {
    bins.iter().copied().max().unwrap_or(1).max(1)
}

/// Normalize one channel's 256 bins to `[0,1]` heights against `peak`.
pub fn channel_norm(bins: &[u32], channel: usize, peak: u32) -> Vec<f32> {
    let base = channel * HIST_BINS;
    (0..HIST_BINS)
        .map(|i| bins[base + i] as f32 / peak as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_is_max_bin_min_one() {
        assert_eq!(peak_bin(&[0, 0, 0]), 1, "empty peak floors at 1");
        assert_eq!(peak_bin(&[3, 9, 4]), 9);
    }

    #[test]
    fn channel_norm_scales_against_peak() {
        let mut bins = vec![0u32; HIST_LEN];
        bins[0] = 5; // R bin 0
        bins[HIST_BINS] = 10; // G bin 0
        let peak = peak_bin(&bins);
        assert_eq!(peak, 10);
        let r = channel_norm(&bins, 0, peak);
        assert_eq!(r.len(), HIST_BINS);
        assert!((r[0] - 0.5).abs() < 1e-6, "R bin 0 is half the peak");
        let g = channel_norm(&bins, 1, peak);
        assert!((g[0] - 1.0).abs() < 1e-6, "G bin 0 is the peak");
    }
}
```

- [ ] **Step 2: Run the tests to verify failure**

Run: `cargo test -p ferrolite-app channel_norm -- --nocapture`
Expected: FAIL — module not declared yet (compile error).

- [ ] **Step 3: Declare the module**

In `ferrolite-app/src/develop/mod.rs`, add:
```rust
pub mod histogram_widget;
```

- [ ] **Step 4: Run the tests to green**

Run: `cargo test -p ferrolite-app channel_norm peak_is_max -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 5: Implement `show` (the painted widget)**

Append to `ferrolite-app/src/develop/histogram_widget.rs`:

```rust
const HIST_H: f32 = 96.0;

/// Draw the four-channel histogram into a fixed-height area. `None` (no data yet)
/// paints just the framed background.
pub fn show(ui: &mut egui::Ui, bins: Option<&[u32]>) {
    let (rect, _resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), HIST_H), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, crate::theme::BG_CANVAS);

    let Some(bins) = bins.filter(|b| b.len() == HIST_LEN) else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Histogram\u{2026}",
            egui::FontId::proportional(11.0),
            crate::theme::TEXT_FAINT,
        );
        return;
    };

    let peak = peak_bin(bins);
    // R, G, B, luma — additive-ish translucent fills so overlaps read naturally.
    let channels = [
        (0usize, egui::Color32::from_rgba_unmultiplied(230, 70, 70, 150)),
        (1, egui::Color32::from_rgba_unmultiplied(70, 200, 90, 150)),
        (2, egui::Color32::from_rgba_unmultiplied(80, 130, 235, 150)),
        (3, egui::Color32::from_rgba_unmultiplied(200, 200, 200, 90)),
    ];
    for (ch, color) in channels {
        let heights = channel_norm(bins, ch, peak);
        let mut pts: Vec<egui::Pos2> = Vec::with_capacity(HIST_BINS + 2);
        pts.push(egui::pos2(rect.left(), rect.bottom()));
        for (i, h) in heights.iter().enumerate() {
            let x = rect.left() + (i as f32 / (HIST_BINS - 1) as f32) * rect.width();
            let y = rect.bottom() - h * (rect.height() - 2.0);
            pts.push(egui::pos2(x, y));
        }
        pts.push(egui::pos2(rect.right(), rect.bottom()));
        painter.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
    }
    painter.rect_stroke(rect, 3.0, egui::Stroke::new(1.0, crate::theme::BORDER));
}
```

> `egui::Shape::convex_polygon` on a non-convex histogram outline still fills acceptably for this read-only overview; it avoids per-bar rects. If `theme::BORDER` does not exist, use `theme::TEXT_FAINT` (grep `pub const` in `theme.rs` for the available palette).

- [ ] **Step 6: Draw it in the adjustment panel**

In `ferrolite-app/src/develop/adjustment_panel.rs`, after the working-space `ComboBox` block (after line 78, before `// ── Basic ──`), add:
```rust
    // ── Histogram (spec §7.1) ── read-only; GPU-computed, display-referred.
    {
        let bins = state
            .viewer
            .as_ref()
            .and_then(|v| v.histogram.bins.as_deref());
        crate::develop::histogram_widget::show(ui, bins);
        ui.add_space(6.0);
    }
```
(`state` is borrowed immutably here; it is later borrowed mutably in the sections below — this block ends before those, so the borrows do not overlap.)

- [ ] **Step 7: Build + clippy + run the unit tests**

Run:
```bash
cargo build -p ferrolite-app
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app histogram_widget -- --nocapture
```
Expected: clean; unit tests PASS.

- [ ] **Step 8: Commit**

```bash
git add ferrolite-app/src/develop/histogram_widget.rs ferrolite-app/src/develop/mod.rs ferrolite-app/src/develop/adjustment_panel.rs
git commit -m "feat(app): read-only four-channel histogram widget in the Develop panel"
```

---

## Task 4: Pure divider math (`develop/split.rs`)

The before/after divider's position clamp, screen-x mapping, pointer-to-position mapping, and hit-test as a pure `f32` unit — no egui types — so it is trivially testable and reusable.

**Files:**
- Create: `ferrolite-app/src/develop/split.rs`
- Modify: `ferrolite-app/src/develop/mod.rs`

**Interfaces:**
- Produces: `pub const MIN_POS: f32`, `pub const MAX_POS: f32`, `pub const HANDLE_TOL: f32`; `pub fn clamp_pos(pos: f32) -> f32`; `pub fn divider_x(left: f32, width: f32, pos: f32) -> f32`; `pub fn pos_from_pointer(left: f32, width: f32, pointer_x: f32) -> f32`; `pub fn hit_divider(left: f32, width: f32, pos: f32, pointer_x: f32, tol: f32) -> bool`.

- [ ] **Step 1: Write the failing tests**

Create `ferrolite-app/src/develop/split.rs`:

```rust
//! Pure before/after split-divider math (spec §7.2): fraction-of-canvas position
//! in `[MIN_POS, MAX_POS]`, screen-x mapping, pointer→position, and hit-testing.
//! No egui types — egui only routes pointer events into these functions.

/// Keep the divider clear of the extreme edges so a sliver of each side stays
/// visible and the handle is always grabbable.
pub const MIN_POS: f32 = 0.03;
pub const MAX_POS: f32 = 0.97;
/// Pointer-to-divider distance (screen px) treated as "on the handle".
pub const HANDLE_TOL: f32 = 10.0;

/// Clamp a fractional position into the usable range.
pub fn clamp_pos(pos: f32) -> f32 {
    pos.clamp(MIN_POS, MAX_POS)
}

/// Screen x of the divider inside a canvas at `left` with `width`.
pub fn divider_x(left: f32, width: f32, pos: f32) -> f32 {
    left + clamp_pos(pos) * width
}

/// Fractional position (clamped) for a pointer at screen x `pointer_x`.
pub fn pos_from_pointer(left: f32, width: f32, pointer_x: f32) -> f32 {
    if width <= 0.0 {
        return 0.5;
    }
    clamp_pos((pointer_x - left) / width)
}

/// True when `pointer_x` is within `tol` px of the divider.
pub fn hit_divider(left: f32, width: f32, pos: f32, pointer_x: f32, tol: f32) -> bool {
    (pointer_x - divider_x(left, width, pos)).abs() <= tol
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_within_bounds() {
        assert_eq!(clamp_pos(-1.0), MIN_POS);
        assert_eq!(clamp_pos(2.0), MAX_POS);
        assert!((clamp_pos(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn divider_x_maps_fraction_to_screen() {
        // canvas [100, 300): width 200, pos 0.5 -> x 200.
        assert!((divider_x(100.0, 200.0, 0.5) - 200.0).abs() < 1e-4);
    }

    #[test]
    fn pos_from_pointer_inverts_divider_x_and_clamps() {
        let (left, width) = (100.0, 200.0);
        let x = divider_x(left, width, 0.4);
        assert!((pos_from_pointer(left, width, x) - 0.4).abs() < 1e-4);
        // Far left/right clamp to the bounds.
        assert_eq!(pos_from_pointer(left, width, 0.0), MIN_POS);
        assert_eq!(pos_from_pointer(left, width, 10_000.0), MAX_POS);
    }

    #[test]
    fn hit_test_respects_tolerance() {
        let (left, width, pos) = (0.0, 100.0, 0.5); // divider at x=50
        assert!(hit_divider(left, width, pos, 54.0, HANDLE_TOL));
        assert!(!hit_divider(left, width, pos, 70.0, HANDLE_TOL));
    }
}
```

- [ ] **Step 2: Verify failure, then declare the module**

Run: `cargo test -p ferrolite-app split -- --nocapture`
Expected: FAIL (module not declared).

In `ferrolite-app/src/develop/mod.rs`, add:
```rust
pub mod split;
```

- [ ] **Step 3: Run the tests to green**

Run: `cargo test -p ferrolite-app split:: -- --nocapture`
Expected: PASS (4 tests).

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/develop/split.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(app): pure before/after split-divider math unit"
```

---

## Task 5: Before/after split plumbing (second preview texture + callback variant)

Give the paint callback a `Before`/`After` selector, hold a second rung-1 VT (`preview_before`) wrapping the identity/`sRGB→working` "before" texture in `ViewerGpu`, add the split state to `ViewerState`, build the before-view on demand, and invalidate it on working-space change. No rendering yet.

**Files:**
- Modify: `ferrolite-app/src/viewer/callback.rs` (`PreviewWhich`, `ViewerCallback.which`, `ViewerGpu.preview_before`, prepare/paint)
- Modify: `ferrolite-app/src/viewer/mod.rs` (`split_compare`/`split_pos`/`split_full_logged`; set `which` in `paint`)
- Modify: `ferrolite-app/src/app.rs` (`ensure_before_view`; seed `preview_before: None`; invalidate in `apply_working_space`)

**Interfaces:**
- Produces: `pub enum PreviewWhich { After, Before }` (`Copy`); `ViewerCallback.which: PreviewWhich`; `ViewerGpu.preview_before: Option<VirtualTexture>`; `ViewerState.split_compare: bool`, `split_pos: f32`, `split_full_logged: bool`.
- Produces: `FerroliteApp::ensure_before_view(&mut self, frame: &eframe::Frame)`.
- Consumes: `ferrolite_pipeline::color_convert`, `VirtualTexture::single_from_texture`, `FerroliteApp::preview_to_working`.

- [ ] **Step 1: Add the callback variant + selector + before holder**

In `ferrolite-app/src/viewer/callback.rs`:

Add above `ViewerCallback`:
```rust
/// Which of the two rung-1 previews a callback draws: the edited `After`
/// (the normal preview) or the unedited `Before` (identity stack). Used by the
/// before/after split — two callbacks with the same rect but different clip rects.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PreviewWhich {
    After,
    Before,
}
```
Add to `struct ViewerGpu` (after the `pub full: Option<VirtualTexture>,` field):
```rust
    /// Rung-1 "before" (unedited, `sRGB→working`) preview for the split view.
    /// Built on demand while split-compare is active; `None` otherwise.
    pub preview_before: Option<VirtualTexture>,
```

Add to `struct ViewerCallback` (after `pub show_full: bool,`):
```rust
    pub which: PreviewWhich,
```
Re-export the new enum: extend the `pub use callback::{...}` in `ferrolite-app/src/viewer/mod.rs` (line 9) to include `PreviewWhich`:
```rust
pub use callback::{PreviewWhich, ViewerCallback, ViewerGpu, ViewerPipelines};
```

- [ ] **Step 2: Select the texture by `which` in prepare/paint**

In `ferrolite-app/src/viewer/callback.rs`, replace the body of `prepare` (the `if let Some(g) = resources.get_mut::<ViewerGpu>()` block) with:
```rust
        if let Some(g) = resources.get_mut::<ViewerGpu>() {
            if g.image_id == self.image_id {
                match self.which {
                    PreviewWhich::After => {
                        if self.show_full {
                            if let Some(full) = g.full.as_mut() {
                                full.prepare_sparse(&g.ctx, &self.view, self.viewport);
                            } else {
                                g.preview.prepare_single(&g.ctx, &self.view, self.viewport);
                            }
                        } else {
                            g.preview.prepare_single(&g.ctx, &self.view, self.viewport);
                        }
                    }
                    PreviewWhich::Before => {
                        // Split is preview-tier only: the before is always rung-1.
                        if let Some(pb) = g.preview_before.as_mut() {
                            pb.prepare_single(&g.ctx, &self.view, self.viewport);
                        }
                    }
                }
            }
        }
```
Replace the body of `paint` similarly:
```rust
        if let Some(g) = resources.get::<ViewerGpu>() {
            if g.image_id == self.image_id {
                match self.which {
                    PreviewWhich::After => {
                        if self.show_full {
                            if let Some(full) = g.full.as_ref() {
                                full.draw_sparse(pass);
                            } else {
                                g.preview.draw_single(pass);
                            }
                        } else {
                            g.preview.draw_single(pass);
                        }
                    }
                    PreviewWhich::Before => {
                        if let Some(pb) = g.preview_before.as_ref() {
                            pb.draw_single(pass);
                        }
                    }
                }
            }
        }
```

- [ ] **Step 3: Set `which: After` in `viewer::paint`**

In `ferrolite-app/src/viewer/mod.rs` `paint`, the `ViewerCallback { .. }` construction (line ~303) — add the field:
```rust
            ViewerCallback {
                image_id: state.image_id,
                view: state.view,
                viewport,
                show_full,
                which: crate::viewer::PreviewWhich::After,
            },
```

- [ ] **Step 4: Add split state to `ViewerState`**

In `ferrolite-app/src/viewer/mod.rs`, add to `struct ViewerState` (near `before_after`):
```rust
    /// When `true`, the viewer renders the before/after SPLIT (draggable divider);
    /// distinct from `before_after` (the `\` momentary full-before swap).
    pub split_compare: bool,
    /// Divider position as a fraction of the canvas width, in [MIN_POS, MAX_POS].
    pub split_pos: f32,
    /// One-shot guard so the "split suppressed at 1:1" note logs once, not per frame.
    pub split_full_logged: bool,
```
Initialize in `ViewerState::open` (near `before_after: false,`):
```rust
            split_compare: false,
            split_pos: 0.5,
            split_full_logged: false,
```

- [ ] **Step 5: Seed `preview_before: None` where `ViewerGpu` is built**

Grep `ViewerGpu {` in `ferrolite-app/src/app.rs` (there should be exactly one construction, in `apply_preview_ready`, ~line 127). Add `preview_before: None,` to that literal:
```rust
            .insert(viewer::ViewerGpu {
                ctx: gpu,
                preview: vt,
                full: None,
                preview_before: None,
                image_id,
            });
```
(If any other `ViewerGpu { .. }` construction turns up, add the field there too.)

- [ ] **Step 6: Build the before-view on demand (`ensure_before_view`)**

In `ferrolite-app/src/app.rs` `impl FerroliteApp`, add:
```rust
    /// Ensure `ViewerGpu.preview_before` holds the unedited (identity stack,
    /// `sRGB→working`) rung-1 preview while split-compare is active. Built from the
    /// retained `preview_source` via one `color_convert` pass (no upload of a new
    /// image beyond that). Rebuilt only when missing (invalidated on WS change /
    /// image open), so edits do not recompute it — the before never changes.
    fn ensure_before_view(&mut self, frame: &eframe::Frame) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let (active, image_id, src) = match self.state.viewer.as_ref() {
            Some(v) => (v.split_compare, v.image_id, v.preview_source.clone()),
            None => return,
        };
        if !active {
            return;
        }
        let Some(src) = src else {
            return;
        };
        // Already built for this image? Nothing to do.
        {
            let renderer = rs.renderer.read();
            if let Some(g) = renderer.callback_resources.get::<viewer::ViewerGpu>() {
                if g.image_id == image_id && g.preview_before.is_some() {
                    return;
                }
            }
        }
        let pw = self.preview_to_working();
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
        let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let converted = ferrolite_pipeline::color_convert(ctx_arc, &src, pw);
        let vt = {
            let renderer = rs.renderer.read();
            let Some(vp) = renderer.callback_resources.get::<viewer::ViewerPipelines>() else {
                return;
            };
            ferrolite_vt::VirtualTexture::single_from_texture(
                &gpu,
                converted.texture.clone(),
                (converted.width, converted.height),
                &vp.pipelines,
            )
        };
        let mut renderer = rs.renderer.write();
        if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
            if g.image_id == image_id {
                g.preview_before = Some(vt);
            }
        }
    }
```

- [ ] **Step 7: Invalidate `preview_before` on working-space change**

In `ferrolite-app/src/app.rs` `apply_working_space`, inside the existing `rs.renderer.write()` scope (where `full.set_opstack_version` is called on `ViewerGpu`), also clear the before-view so `ensure_before_view` rebuilds it with the new matrix:
```rust
                if g.image_id == image_id {
                    g.preview_before = None; // rebuilt by ensure_before_view with new WS
                    if let Some(full) = g.full.as_mut() {
                        full.set_opstack_version(&g.ctx, version);
                    }
                }
```
(Adapt to the exact `get_mut::<ViewerGpu>()` block already present; add only the `g.preview_before = None;` line within the `image_id` guard.)

- [ ] **Step 8: Build + clippy**

Run:
```bash
cargo build -p ferrolite-app
cargo clippy -p ferrolite-app --all-targets -- -D warnings
```
Expected: clean (no rendering path uses `Before` yet, but the plumbing compiles).

- [ ] **Step 9: Commit**

```bash
git add ferrolite-app/src/viewer/callback.rs ferrolite-app/src/viewer/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(app): before/after split plumbing — second preview texture + callback variant"
```

---

## Task 6: Split rendering, divider interaction, and the toolbar toggle

Render the before-view clipped left of a draggable divider over the after-view, draw the divider + handle, route pointer drags into the pure split math, and add the Develop-toolbar toggle button. `\` full-before stays.

**Files:**
- Modify: `ferrolite-app/src/app.rs` (`drive_viewer`: ensure before-view, gate interaction, split callback + divider draw + drag, 1:1 log)
- Modify: `ferrolite-app/src/library/develop_filter_bar.rs` (toggle button)

**Interfaces:**
- Consumes: `crate::develop::split`, `crate::viewer::{PreviewWhich, ViewerCallback}`, `crate::viewer::image_screen_rect` (not needed — the split uses the full canvas rect).

- [ ] **Step 1: Gate canvas pan/zoom off while splitting**

In `ferrolite-app/src/app.rs` `drive_viewer`, change the interactive gate (line ~591) so the divider owns input while the split is shown on the preview tier:
```rust
        let interactive = !v.crop_active && !(v.split_compare && !show_full);
```

- [ ] **Step 2: Render the split + divider (restructure the tail of `drive_viewer`)**

The split needs `&mut self` (for `ensure_before_view` and writing `split_pos`), but the current tail of `drive_viewer` holds `let Some(v) = self.state.viewer.as_mut()` across `viewer::paint` and the repaint block. Capture everything the split needs into `Copy` locals **where `v` and `show_full` are already in scope** (around the `viewer::paint` call), then do the `&mut self` split work **after** the `v` borrow ends.

The "after" view is already painted by `viewer::paint` over the full canvas rect; the split paints the "before" (identity, `sRGB→working`) clipped LEFT of the divider on top, so left = before, right = after. Same callback `rect` ⇒ identical image geometry; only the egui clip rect (scissor) differs.

Replace the tail of `drive_viewer` from the `viewer::paint` call onward with:

```rust
        let canvas_rect = ui.available_rect_before_wrap();
        let split_active = v.split_compare && !show_full;
        let (image_id, view, viewport, split_pos) =
            (v.image_id, v.view, v.viewport, v.split_pos);
        if v.split_compare && show_full && !v.split_full_logged {
            log::debug!("before/after split suppressed at 1:1 zoom; showing after-view");
            v.split_full_logged = true;
        }
        if !show_full {
            v.split_full_logged = false;
        }

        let loading_preview = viewer::paint(ui, v, show_full, interactive);
        let idle = v.idle;

        // (repaint-request block unchanged) …
        let tiles_loading = matches!(tiles_pending, Some(n) if n > 0);
        if !idle && (loading_preview || crossfading || tiles_loading) {
            ui.ctx().request_repaint();
        }

        // The `v` borrow has ended; now the split needs &mut self.
        if split_active {
            self.ensure_before_view(frame);
            let div_x = crate::develop::split::divider_x(
                canvas_rect.left(),
                canvas_rect.width(),
                split_pos,
            );
            // Paint the "before" clipped to the left of the divider (on top of
            // the already-painted after). Same rect ⇒ identical geometry.
            let left_clip = egui::Rect::from_min_max(
                canvas_rect.min,
                egui::pos2(div_x, canvas_rect.max.y),
            );
            ui.painter()
                .with_clip_rect(left_clip)
                .add(egui_wgpu::Callback::new_paint_callback(
                    canvas_rect,
                    viewer::ViewerCallback {
                        image_id,
                        view,
                        viewport,
                        show_full: false,
                        which: viewer::PreviewWhich::Before,
                    },
                ));
            // Divider line + a grab handle at mid-height.
            let painter = ui.painter();
            painter.vline(
                div_x,
                canvas_rect.y_range(),
                egui::Stroke::new(1.5, egui::Color32::WHITE),
            );
            let handle_center = egui::pos2(div_x, canvas_rect.center().y);
            painter.circle(
                handle_center,
                7.0,
                egui::Color32::from_black_alpha(120),
                egui::Stroke::new(1.5, egui::Color32::WHITE),
            );
            // Drag: a thin full-height strip around the divider owns the pointer.
            let hit = crate::develop::split::HANDLE_TOL;
            let strip = egui::Rect::from_min_max(
                egui::pos2(div_x - hit, canvas_rect.top()),
                egui::pos2(div_x + hit, canvas_rect.bottom()),
            );
            let resp = ui.interact(
                strip,
                ui.id().with(("split-divider", image_id)),
                egui::Sense::click_and_drag(),
            );
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if resp.dragged() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let new_pos = crate::develop::split::pos_from_pointer(
                        canvas_rect.left(),
                        canvas_rect.width(),
                        pos.x,
                    );
                    if let Some(v) = self.state.viewer.as_mut() {
                        v.split_pos = new_pos;
                    }
                    ui.ctx().request_repaint();
                }
            }
        }
    }
```

> Notes: (a) `viewer::paint` already added the "after" callback over `canvas_rect`; the "before" is added *after* it so it draws on top in the clipped region. (b) `egui_wgpu` sets the render viewport from the callback `rect` and the scissor from the painter's clip rect — same `rect` for both callbacks keeps the image aligned; the clip splits them by screen-x (spec §7.2). (c) With `interactive` gated off (Step 1) the canvas registers no pan/zoom while splitting, so the divider strip has no competing drag. (d) If `drive_viewer` does not already `use` `egui_wgpu`/`log`, add `egui_wgpu::Callback` fully-qualified (as above) and `log::debug!` (the `log` crate is already a dependency — grep `log::` in `app.rs`; if absent, use `eprintln!` instead).

- [ ] **Step 3: Add the Develop-toolbar toggle button**

In `ferrolite-app/src/library/develop_filter_bar.rs`, add the split toggle at the start of the `ui.horizontal_centered` closure (before the sort controls), and reset the divider to centre when turning it on:
```rust
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if let Some(v) = state.viewer.as_mut() {
            if ui
                .selectable_label(v.split_compare, "\u{21D4} Before/After")
                .on_hover_text("Split-compare the original against the current edit")
                .clicked()
            {
                v.split_compare = !v.split_compare;
                if v.split_compare {
                    v.split_pos = 0.5;
                }
            }
            ui.separator();
        }
        changed |= fw::sort_controls(ui, &mut state.filter.sort_key, &mut state.filter.sort_desc);
        // … (rest unchanged) …
```
(Toggling the flag takes effect next frame: `drive_viewer` builds `preview_before` via `ensure_before_view` and renders the split. No `state.dirty` — this is not a filter change.)

- [ ] **Step 4: Build + clippy**

Run:
```bash
cargo build -p ferrolite-app
cargo clippy -p ferrolite-app --all-targets -- -D warnings
```
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/app.rs ferrolite-app/src/library/develop_filter_bar.rs
git commit -m "feat(app): before/after split render + draggable divider + toolbar toggle"
```

---

## Finish

- [ ] **Full gate green:**
```bash
cargo fmt --all && cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: fmt clean, clippy clean, all tests green (the histogram GPU golden + split/divider/histogram-widget units run on the dev GPU / skip headless).

- [ ] **STOP and hold for Jann's hands-on visual test** (CLAUDE.md "Finishing a branch"). Verify:
  - **Histogram:** open a RAW and a JPEG; the panel histogram shows R/G/B/luma and updates (debounced) as sliders move — exposure shifts it left/right, contrast spreads it, WB moves channels; it matches the on-screen brightness (display-referred); switching working space recolours it; no UI stutter while dragging (readback is async, ~4 KB).
  - **Split:** toggle the Develop-toolbar **⇔ Before/After** button — the left of the divider shows the unedited original, the right shows the current edit, aligned (same geometry); drag the divider left/right smoothly; the `\` key still does the momentary full-before swap independently; at 1:1 zoom the split is suppressed (after-view shown), then reappears when zoomed back to fit.
  - Address any issues found before merging. Do not finish the branch until Jann approves.

---

## Self-Review notes

- **Spec §7.1 (GPU histogram):** Task 1 (compute pass, 256×{R,G,B,luma} atomics, display-referred via `working→display` + sRGB OETF, ~4 KB `map_async` readback, pipeline built once) + Task 2 (pre-warm, debounced dispatch on preview recompute, delivery over the app event channel → `request_repaint`) + Task 3 (panel widget, read-only, no per-control reset). Golden: Task 1 Step 7 (GPU vs CPU reference).
- **Spec §7.2 (before/after split):** Task 4 (pure divider math unit — clamp/hit-test/drag) + Task 5 (before texture = `OpStack::default()` ≡ `color_convert(preview_source, sRGB→working)`, second rung-1 VT, callback `Before`/`After` variant) + Task 6 (draggable vertical divider, before-left/after-right via egui clip-rect scissor, toolbar toggle; `\` full-before unchanged; preview-tier only, 1:1 logs + shows after).
- **Spec §5 contract / licensing:** the histogram lives in engine-tier `ferrolite-vt` and takes a plain `[[f32;3];3]` (reuses `pack_display_matrix`), no `ferrolite-color`/photo concept; the generic `Graph<PipelineImage>` executor is untouched; `ferrolite-color` used only in the app (matrix composition).
- **CLAUDE.md §1 (never block UI):** only the 4 KB bin buffer is read back, async (`map_async` + non-blocking `Maintain::Poll`); the per-pixel work is on the GPU; debounced so a drag is one compute, not one per frame.
- **CLAUDE.md §2 (build pipelines once):** `HistogramPipeline::new` compiles once, pre-warmed at startup alongside `DisplayPipelines`; recompute is `write_buffer` + dispatch. The split adds no new pipeline (reuses the rung-1 display pipeline for both textures).
- **Type consistency:** `HIST_LEN`/`HIST_BINS` shared between vt and app; `bin_index` identical in Rust + WGSL (unit-tested); `pack_display_matrix` reused; `PreviewWhich`/`ViewerCallback.which` threaded through `viewer::paint` (After) and the split callback (Before); `ViewerGpu.preview_before` seeded `None` at construction and invalidated on WS change.
- **Borrow discipline:** matrices/positions/ids are gathered into locals before `&mut self` calls (`maybe_update_histogram`, the split block, `ensure_before_view`), mirroring the existing `set_preview_and_full` / panel-outcome patterns.
