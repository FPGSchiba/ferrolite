//! `SharpenNode` — the unsharp-mask sharpen as a separable, three-pass
//! `Node<PipelineImage>`. Replaces the fused `sharpen.wgsl` `PointOpNode`
//! (O((2r+1)^2) box blur) with an O(2r+1) two-pass box blur (H then V) plus a
//! cheap apply pass, at the SAME graph position/inputs in both `EditPipeline`
//! and `TileEditPipeline` (`sharpen.wgsl` stays in-tree as reference math —
//! see that file's doc).
//!
//! **Mathematical identity this depends on:** a clamped-edge box mean is
//! separable — computing the horizontal mean (radius `r`, x clamped to
//! `[0, w-1]`) and then the vertical mean of THAT (radius `r`, y clamped to
//! `[0, h-1]`), each normalized by `2r+1`, equals the fused 2D box mean
//! (normalized by `(2r+1)^2`) to float-order precision. This holds because
//! `clamp` DUPLICATES the border texel rather than skipping it — every
//! output pixel's fused-2D window always has exactly `(2r+1)^2` taps (some
//! repeated at the border), and expanding the two 1D sums shows the H-then-V
//! composition visits the same multiset of source texels with the same
//! weights, just summed in a different (but float-associative-safe within
//! 1e-6 for well-conditioned inputs) order. `separable_box_equals_2d_box`
//! (below) proves this on a CPU reference before the GPU passes ever ran.
//!
//! **Pass structure:**
//!   1. `sharpen_box_h.wgsl` — src (rgba16float) -> `h_blur` (rgba16float),
//!      horizontal box mean, radius `r`.
//!   2. `sharpen_box_v.wgsl` — `h_blur` -> `blur` (rgba16float), vertical box
//!      mean of the H pass's output, same radius.
//!   3. `sharpen_apply.wgsl` — `out = src + amount*(src - blur)`, clamped
//!      non-negative, alpha passed through — reads both `src` and `blur`.
//!
//! **Identity passthrough:** when `amount == 0 || radius <= 0`, `evaluate`
//! returns `src.clone()` (a cheap `Arc` clone of `PipelineImage`, mirroring
//! `DehazeTransmissionNode`'s early-return pattern) WITHOUT dispatching any of
//! the three passes — byte-identical to the old shader's in-shader identity
//! branch, and cheaper (zero GPU work, not even a copy dispatch).
//!
//! **Dims-keyed intermediates:** `h_blur`/`blur` (mirrors
//! `dehaze_node.rs::Intermediates`'s `ensure_*` pattern) are reallocated only
//! when `(w, h)` changes, never per-evaluate — a slider drag at fixed
//! resolution reuses both textures.
//!
//! **Reusable blur step (forward-looking, not yet exercised):** the H+V pair
//! is factored into `encode_blur` so a later per-mask/per-radius task (Phase 4
//! Task 4) can call it more than once per evaluate without duplicating the
//! dispatch plumbing. No multi-radius cache or per-layer machinery is built
//! here — YAGNI until that task actually needs it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{GpuContext, Node};

use crate::image::{PipelineImage, PIPELINE_FORMAT};
use crate::uniforms::SharpenUniform;

/// Intermediate plane format for the H/V blur passes: the same
/// `PIPELINE_FORMAT` (`rgba16float`) every other node's textures use, NOT
/// full-precision `Rgba32Float`. This was tried (see the task report) and
/// reverted: full-precision intermediates roughly DOUBLE this node's memory
/// traffic (two full-res `Rgba32Float` planes vs `Rgba16Float`), which measured
/// as a same-session A/B regression from ~56ms to ~92-104ms on case (a) of
/// `engine_bench` — the exact opposite of this task's purpose (cutting sharpen
/// time via O(r) taps). `rgba16float` intermediates DO measurably widen the
/// `full_global` parity golden's drift (~7.9e-3, vs ~4.0e-3 at full precision)
/// — an accepted, documented precision cost of the perf win (see the task
/// report's "Parity" section for the root-cause diagnosis: the drift scales
/// with local pixel variance, proven via synthetic GPU-vs-GPU comparisons, not
/// an edge-handling bug — `separable_box_equals_2d_box` below proves the
/// underlying math is exact to 1e-6 in full CPU f32 precision).
const BLUR_FORMAT: wgpu::TextureFormat = PIPELINE_FORMAT;

/// Intermediate planes (see `BLUR_FORMAT`), keyed on `(w, h)` and reallocated
/// together when the input dims change (mirrors
/// `dehaze_node.rs::Intermediates`).
struct Intermediates {
    dims: (u32, u32),
    /// `sharpen_box_h.wgsl`'s output: horizontal box mean of `src`.
    h_blur: wgpu::Texture,
    /// `sharpen_box_v.wgsl`'s output: vertical box mean of `h_blur` — the
    /// final separable box blur `sharpen_apply.wgsl` reads.
    blur: wgpu::Texture,
}

fn alloc_plane(ctx: &GpuContext, w: u32, h: u32, label: &str) -> wgpu::Texture {
    ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: BLUR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    })
}

impl Intermediates {
    fn new(ctx: &GpuContext, w: u32, h: u32) -> Self {
        Self {
            dims: (w, h),
            h_blur: alloc_plane(ctx, w, h, "sharpen-h-blur"),
            blur: alloc_plane(ctx, w, h, "sharpen-blur"),
        }
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn storage_out_entry(binding: u32, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
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

/// Shared by both blur passes: `0 = src texture, 1 = storage-write dst, 2 =
/// uniform` — the same shape `sharpen_box_h.wgsl`/`sharpen_box_v.wgsl` bind,
/// so one bind-group layout and one pair of pipelines cover both dispatches
/// (mirrors `dehaze_node.rs::plane_bgl` being reused for min/box passes).
fn blur_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sharpen-blur-bgl"),
        entries: &[
            texture_entry(0),
            storage_out_entry(1, BLUR_FORMAT),
            uniform_entry(2),
        ],
    })
}

/// `0 = src, 1 = blur, 2 = storage-write dst, 3 = uniform` — the apply pass's
/// bind shape.
fn apply_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sharpen-apply-bgl"),
        entries: &[
            texture_entry(0),
            texture_entry(1),
            storage_out_entry(2, PIPELINE_FORMAT),
            uniform_entry(3),
        ],
    })
}

fn compute_pipeline(
    ctx: &GpuContext,
    bgl: &wgpu::BindGroupLayout,
    label: &str,
    wgsl: &'static str,
) -> wgpu::ComputePipeline {
    let module = ctx.shader_module(label, wgsl);
    let layout = ctx
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[bgl],
            push_constant_ranges: &[],
        });
    ctx.device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            module: &module,
            entry_point: "main",
            compilation_options: Default::default(),
            cache: None,
        })
}

fn view(tex: &wgpu::Texture) -> wgpu::TextureView {
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

fn dispatch(
    enc: &mut wgpu::CommandEncoder,
    label: &str,
    pipeline: &wgpu::ComputePipeline,
    bind: &wgpu::BindGroup,
    w: u32,
    h: u32,
) {
    let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some(label),
        timestamp_writes: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind, &[]);
    pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
}

/// The separable sharpen: three compute passes (H blur, V blur, apply), built
/// once; intermediates + output reallocated only on a `(w, h)` change.
pub(crate) struct SharpenNode {
    ctx: Arc<GpuContext>,
    params: Rc<Cell<SharpenUniform>>,
    // One uniform buffer feeds all three passes: the H/V passes only read
    // `radius` (their `amount` field is dead but keeps the WGSL `struct P`
    // layout byte-identical to the apply pass's, so a single buffer/write
    // covers all three binds). Written once per evaluate, before any pass in
    // this evaluate's single command buffer is encoded, so every dispatch
    // sees the same value — no cross-pass clobber risk (unlike
    // `DehazeTransmissionNode`'s two-buffer split, which exists because THAT
    // node uploads two DIFFERENT radii in one evaluate; this node uploads one
    // value used unchanged by all three passes).
    uniform_buf: wgpu::Buffer,

    blur_bgl: wgpu::BindGroupLayout,
    h_pipeline: wgpu::ComputePipeline,
    v_pipeline: wgpu::ComputePipeline,

    apply_bgl: wgpu::BindGroupLayout,
    apply_pipeline: wgpu::ComputePipeline,

    intermediates: RefCell<Option<Intermediates>>,
    out: RefCell<Option<PipelineImage>>,
}

impl SharpenNode {
    pub(crate) fn new(ctx: Arc<GpuContext>, params: Rc<Cell<SharpenUniform>>) -> Self {
        let device = &ctx.device;
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sharpen-uniform"),
            size: std::mem::size_of::<SharpenUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let blur_bgl_layout = blur_bgl(device);
        let h_pipeline = compute_pipeline(
            &ctx,
            &blur_bgl_layout,
            "sharpen-box-h",
            include_str!("shaders/sharpen_box_h.wgsl"),
        );
        let v_pipeline = compute_pipeline(
            &ctx,
            &blur_bgl_layout,
            "sharpen-box-v",
            include_str!("shaders/sharpen_box_v.wgsl"),
        );

        let apply_bgl_layout = apply_bgl(device);
        let apply_pipeline = compute_pipeline(
            &ctx,
            &apply_bgl_layout,
            "sharpen-apply",
            include_str!("shaders/sharpen_apply.wgsl"),
        );

        Self {
            ctx,
            params,
            uniform_buf,
            blur_bgl: blur_bgl_layout,
            h_pipeline,
            v_pipeline,
            apply_bgl: apply_bgl_layout,
            apply_pipeline,
            intermediates: RefCell::new(None),
            out: RefCell::new(None),
        }
    }

    fn ensure_intermediates(&self, w: u32, h: u32) {
        let mut cur = self.intermediates.borrow_mut();
        let needs_alloc = match cur.as_ref() {
            Some(im) => im.dims != (w, h),
            None => true,
        };
        if needs_alloc {
            *cur = Some(Intermediates::new(&self.ctx, w, h));
        }
    }

    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((w, h)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("sharpen-out"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PIPELINE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            *out = Some(PipelineImage {
                texture: Arc::new(tex),
                width: w,
                height: h,
            });
        }
        out.as_ref().unwrap().clone()
    }

    /// Encode the separable box blur (H then V): `src_view` -> `h_view`
    /// (horizontal pass) -> `blur_view` (vertical pass, the final blur).
    /// Factored out as its own step (not inlined into `evaluate`) so a future
    /// per-mask/per-radius caller can invoke it more than once per evaluate
    /// without duplicating the bind-group/dispatch plumbing (see the module
    /// doc's "Reusable blur step" note) — no such caller exists yet.
    #[allow(clippy::too_many_arguments)]
    fn encode_blur(
        &self,
        enc: &mut wgpu::CommandEncoder,
        src_view: &wgpu::TextureView,
        h_view: &wgpu::TextureView,
        blur_view: &wgpu::TextureView,
        w: u32,
        h: u32,
    ) {
        let h_bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sharpen-box-h-bind"),
                layout: &self.blur_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(h_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.uniform_buf.as_entire_binding(),
                    },
                ],
            });
        let v_bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sharpen-box-v-bind"),
                layout: &self.blur_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(h_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(blur_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.uniform_buf.as_entire_binding(),
                    },
                ],
            });

        dispatch(enc, "sharpen-box-h", &self.h_pipeline, &h_bind, w, h);
        dispatch(enc, "sharpen-box-v", &self.v_pipeline, &v_bind, w, h);
    }
}

impl Node<PipelineImage> for SharpenNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let src = inputs[0];
        let p = self.params.get();

        // Identity passthrough (amount 0 or radius <= 0): return the input
        // unchanged, no GPU work at all — mirrors `DehazeTransmissionNode`'s
        // early-return (`src.clone()` is a cheap `Arc` clone of
        // `PipelineImage`), and is byte-identical to the old fused shader's
        // in-shader identity branch (same texture, not merely a copy of it).
        if p.amount == 0.0 || p.radius <= 0 {
            return src.clone();
        }

        let (w, h) = (src.width, src.height);
        self.ensure_intermediates(w, h);
        let out = self.ensure_out(w, h);

        self.ctx
            .queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&p));

        let intermediates = self.intermediates.borrow();
        let im = intermediates.as_ref().expect("allocated above");

        let src_view = view(&src.texture);
        let h_view = view(&im.h_blur);
        let blur_view = view(&im.blur);
        let out_view = view(&out.texture);

        let apply_bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sharpen-apply-bind"),
                layout: &self.apply_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&blur_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&out_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.uniform_buf.as_entire_binding(),
                    },
                ],
            });

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sharpen-separable"),
            });

        self.encode_blur(&mut enc, &src_view, &h_view, &blur_view, w, h);
        dispatch(
            &mut enc,
            "sharpen-apply",
            &self.apply_pipeline,
            &apply_bind,
            w,
            h,
        );

        self.ctx.queue.submit([enc.finish()]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::upload_source;
    use ferrolite_image::LinearRgbaF32;

    /// CPU reference: the OLD fused 2D box mean — mirrors `sharpen.wgsl`'s
    /// nested loop and clamp exactly (clamp the combined `(x+dx, y+dy)` to
    /// `[0, dims-1]` on BOTH axes together, normalize by the actual tap count
    /// `(2r+1)^2`).
    fn box_mean_2d(px: &[[f32; 3]], w: usize, h: usize, r: i32) -> Vec<[f32; 3]> {
        let mut out = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0f32; 3];
                let mut n = 0.0f32;
                for dy in -r..=r {
                    for dx in -r..=r {
                        let qx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                        let qy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                        let p = px[qy * w + qx];
                        sum[0] += p[0];
                        sum[1] += p[1];
                        sum[2] += p[2];
                        n += 1.0;
                    }
                }
                out[y * w + x] = [sum[0] / n, sum[1] / n, sum[2] / n];
            }
        }
        out
    }

    /// CPU reference: the separable H-then-V box mean — mirrors
    /// `sharpen_box_h.wgsl`/`sharpen_box_v.wgsl` exactly (clamp only the
    /// pass's own axis, normalize by `2r+1` each pass).
    fn box_mean_separable(px: &[[f32; 3]], w: usize, h: usize, r: i32) -> Vec<[f32; 3]> {
        let n = (2 * r + 1) as f32;
        let mut h_out = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0f32; 3];
                for dx in -r..=r {
                    let qx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                    let p = px[y * w + qx];
                    sum[0] += p[0];
                    sum[1] += p[1];
                    sum[2] += p[2];
                }
                h_out[y * w + x] = [sum[0] / n, sum[1] / n, sum[2] / n];
            }
        }
        let mut v_out = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0f32; 3];
                for dy in -r..=r {
                    let qy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                    let p = h_out[qy * w + x];
                    sum[0] += p[0];
                    sum[1] += p[1];
                    sum[2] += p[2];
                }
                v_out[y * w + x] = [sum[0] / n, sum[1] / n, sum[2] / n];
            }
        }
        v_out
    }

    /// Deterministic gradient + cheap pseudo-noise fixture (no RNG dependency,
    /// mirrors this crate's other test fixtures — e.g. `engine_bench.rs`'s
    /// `bench_source`).
    fn gradient_noise_fixture(w: usize, h: usize) -> Vec<[f32; 3]> {
        let mut px = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let n = ((x * 7 + y * 13) % 11) as f32 / 10.0;
                px[y * w + x] = [
                    x as f32 / w as f32 + n * 0.05,
                    y as f32 / h as f32 + n * 0.03,
                    0.25 + n * 0.02,
                ];
            }
        }
        px
    }

    /// Step 1 (TDD): proves the separable H-then-V box mean equals the fused
    /// 2D box mean, per-pixel, within float noise — BEFORE any GPU shader
    /// exists. This is the mathematical identity `SharpenNode`'s three-pass
    /// split depends on (see the module doc).
    #[test]
    fn separable_box_equals_2d_box() {
        let (w, h, r) = (16usize, 16usize, 3i32);
        let px = gradient_noise_fixture(w, h);
        let two_d = box_mean_2d(&px, w, h, r);
        let sep = box_mean_separable(&px, w, h, r);
        assert_eq!(two_d.len(), sep.len());
        for (i, (a, b)) in two_d.iter().zip(sep.iter()).enumerate() {
            for c in 0..3 {
                let d = (a[c] - b[c]).abs();
                assert!(
                    d < 1e-6,
                    "pixel {i} channel {c}: 2d={} separable={} diff={d}",
                    a[c],
                    b[c]
                );
            }
        }
    }

    /// Read all four RGBA channels of an `Rgba16Float` `PipelineImage` back to
    /// f32 (test-only; mirrors `dehaze_node.rs::read_rgba_channels`).
    fn read_rgba_channels(ctx: &GpuContext, img: &PipelineImage) -> Vec<[f32; 4]> {
        let (w, h) = (img.width, img.height);
        let bpp = 8u32; // RGBA16F
        let bpr_unpadded = w * bpp;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let bpr_padded = bpr_unpadded.div_ceil(align) * align;
        let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sharpen-node-test-readback"),
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
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        ctx.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        ctx.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let mut out = vec![[0.0f32; 4]; (w * h) as usize];
        for row in 0..h {
            let start = (row * bpr_padded) as usize;
            for x in 0..w {
                let o = start + (x * 4) as usize * 2;
                let r = half::f16::from_le_bytes([data[o], data[o + 1]]).to_f32();
                let g = half::f16::from_le_bytes([data[o + 2], data[o + 3]]).to_f32();
                let b = half::f16::from_le_bytes([data[o + 4], data[o + 5]]).to_f32();
                let a = half::f16::from_le_bytes([data[o + 6], data[o + 7]]).to_f32();
                out[(row * w + x) as usize] = [r, g, b, a];
            }
        }
        drop(data);
        buf.unmap();
        out
    }

    /// Step 1 (TDD) continued: `SharpenNode`'s GPU output must match the OLD
    /// fused 2D formula computed CPU-side, within `2e-3` (absorbs the
    /// rgba16float storage round-trip through two intermediate planes, same
    /// order as `dehaze_node.rs`'s GPU-vs-CPU tolerances).
    #[test]
    fn sharpen_node_matches_old_2d_formula() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let (w, h) = (32u32, 24u32);
        let (r, amount) = (3i32, 0.8f32);

        let planar = gradient_noise_fixture(w as usize, h as usize);
        let mut interleaved = Vec::with_capacity((w * h * 4) as usize);
        for p in &planar {
            interleaved.extend_from_slice(&[p[0], p[1], p[2], 1.0]);
        }
        let img = LinearRgbaF32::new(w, h, interleaved).expect("fixture image");
        let src = upload_source(&ctx, &img);

        let params = Rc::new(Cell::new(SharpenUniform {
            amount,
            radius: r,
            pad: [0.0; 2],
        }));
        let node = SharpenNode::new(ctx.clone(), params);
        let out = node.evaluate(&[&src]);
        assert_eq!((out.width, out.height), (w, h));

        let gpu = read_rgba_channels(&ctx, &out);
        let cpu_blur = box_mean_2d(&planar, w as usize, h as usize, r);

        let mut max_d = 0.0f32;
        for (i, (g, blur)) in gpu.iter().zip(cpu_blur.iter()).enumerate() {
            let c = planar[i];
            for ch in 0..3 {
                let expected = (c[ch] + amount * (c[ch] - blur[ch])).max(0.0);
                let d = (g[ch] - expected).abs();
                max_d = max_d.max(d);
                assert!(
                    d < 2e-3,
                    "pixel {i} channel {ch}: gpu={} expected={} diff={d}",
                    g[ch],
                    expected
                );
            }
            assert!((g[3] - 1.0).abs() < 1e-6, "alpha mismatch at pixel {i}");
        }
        eprintln!("sharpen_node_matches_old_2d_formula: max abs diff = {max_d}");
    }

    /// Identity passthrough: `amount == 0` must return the SAME texture (an
    /// `Arc` clone, not a copy) — no GPU work, byte-identical to the input.
    #[test]
    fn sharpen_node_identity_passthrough_same_texture() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let (w, h) = (8u32, 8u32);
        let px = vec![0.3f32; (w * h * 4) as usize];
        let img = LinearRgbaF32::new(w, h, px).expect("flat fixture");
        let src = upload_source(&ctx, &img);

        let params = Rc::new(Cell::new(SharpenUniform {
            amount: 0.0,
            radius: 5,
            pad: [0.0; 2],
        }));
        let node = SharpenNode::new(ctx.clone(), params);
        let out = node.evaluate(&[&src]);
        assert!(
            Arc::ptr_eq(&out.texture, &src.texture),
            "amount == 0 must return the input texture unchanged (no dispatch, no copy)"
        );

        // radius <= 0 must also passthrough, independent of amount.
        let params2 = Rc::new(Cell::new(SharpenUniform {
            amount: 0.8,
            radius: 0,
            pad: [0.0; 2],
        }));
        let node2 = SharpenNode::new(ctx, params2);
        let out2 = node2.evaluate(&[&src]);
        assert!(
            Arc::ptr_eq(&out2.texture, &src.texture),
            "radius <= 0 must return the input texture unchanged"
        );
    }
}
