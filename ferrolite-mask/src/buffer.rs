//! `MaskBuffer` — a single-channel `R32Float` GPU texture, the mask vocabulary
//! for the whole masking stage. Cheap to clone (Arc handle), mirroring
//! `ferrolite_pipeline::PipelineImage`. Shape passes write it via a write-only
//! storage binding; compositing reads it via `textureLoad` (non-filterable).

use std::sync::Arc;

use ferrolite_gpu::GpuContext;

/// The single-channel mask texture format (R = coverage in [0,1]).
pub const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;

#[derive(Clone)]
pub struct MaskBuffer {
    pub texture: Arc<wgpu::Texture>,
    pub width: u32,
    pub height: u32,
}

impl MaskBuffer {
    /// Allocate an uninitialised `R32Float` mask texture of `width × height`.
    pub fn alloc(ctx: &GpuContext, width: u32, height: u32) -> Self {
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mask-buffer"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: MASK_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        Self {
            texture: Arc::new(texture),
            width: width.max(1),
            height: height.max(1),
        }
    }
}
