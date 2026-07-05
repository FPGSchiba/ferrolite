//! Dab-stamping brush rasterizer. A build-once compute pass that stamps a batch
//! of `Dab`s onto an existing `MaskBuffer` (incremental, ping-pong — no
//! read-modify-write on one texture). Coverage is analytic in normalized source
//! coords (like the shape evaluators), so `rasterize_tile` with `halo = max dab
//! radius` is bit-consistent with `rasterize_full` at tile borders.

use std::sync::Arc;

use bytemuck::Zeroable;
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
    pub fn rasterize_full(&self, dabs: &[Dab], erase: bool, width: u32, height: u32) -> MaskBuffer {
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
