//! GPU edit nodes. This task adds only `upload_source` (the graph root upload);
//! `SourceNode` and `PointOpNode` arrive in later tasks.

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use half::f16;
use std::sync::Arc;
use wgpu::util::DeviceExt;

use crate::image::{PipelineImage, PIPELINE_FORMAT};

/// Upload a display-linear `f32` image as an `Rgba16Float` GPU texture (the
/// pipeline source). Mirrors the VT's single-texture upload (f32 -> f16).
pub fn upload_source(ctx: &GpuContext, img: &LinearRgbaF32) -> PipelineImage {
    let texels: Vec<f16> = img.pixels.iter().map(|&v| f16::from_f32(v)).collect();
    let texture = ctx.device.create_texture_with_data(
        &ctx.queue,
        &wgpu::TextureDescriptor {
            label: Some("pipeline-source"),
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
