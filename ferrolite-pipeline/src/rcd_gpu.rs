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
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
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
    let green_pipe = compute_pipeline(
        ctx,
        &green_bgl,
        "rcd-green",
        include_str!("shaders/rcd_green.wgsl"),
    );
    let green_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rcd-green-bind"),
        layout: &green_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: cfa_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: green_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buf.as_entire_binding(),
            },
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
    let chroma_pipe = compute_pipeline(
        ctx,
        &chroma_bgl,
        "rcd-chroma",
        include_str!("shaders/rcd_chroma.wgsl"),
    );
    let chroma_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rcd-chroma-bind"),
        layout: &chroma_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: cfa_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: green_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&out_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: params_buf.as_entire_binding(),
            },
        ],
    });

    let mut enc =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rcd") });
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rcd-green"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&green_pipe);
        pass.set_bind_group(0, &green_bind, &[]);
        pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
    }
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rcd-chroma"),
            timestamp_writes: None,
        });
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
        assert!(
            max_d < 2e-3,
            "GPU RCD drifted from CPU reference: max abs diff {max_d}"
        );
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
        assert!(
            (gpu.pixels[0] - 2.0).abs() < 2e-3,
            "R must carry >1 (got {})",
            gpu.pixels[0]
        );
    }
}
