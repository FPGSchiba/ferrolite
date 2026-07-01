//! The edit source pyramid, uploaded to the GPU once on full-decode. Each LOD is
//! an `Rgba16Float` texture (display-linear); the per-tile edit producer samples
//! the LOD matching a requested tile's level. Built once, reused for every tile
//! and every edit (CLAUDE.md GPU rule; spec §5.2).

use std::sync::Arc;

use ferrolite_gpu::GpuContext;
use ferrolite_image::{level_size, pyramid_level_count, LinearRgbaF32};
use half::f16;
use wgpu::util::DeviceExt;

use crate::image::{PipelineImage, PIPELINE_FORMAT};

pub struct GpuPyramidSource {
    levels: Vec<PipelineImage>, // index = lod
}

impl GpuPyramidSource {
    pub fn new(ctx: &GpuContext, full: &LinearRgbaF32) -> Self {
        let count = pyramid_level_count(full.width, full.height);
        // Build CPU LODs by box-downsample (same math as PyramidTileSource), then
        // upload each as a texture. (Copied across the tier boundary on purpose —
        // ferrolite-pipeline must not depend on the engine-tier ferrolite-vt.)
        let mut cpu: Vec<LinearRgbaF32> = Vec::with_capacity(count as usize);
        cpu.push(full.clone());
        for lod in 1..count {
            let (w, h) = level_size(full.width, full.height, lod);
            cpu.push(box_downsample(&cpu[(lod - 1) as usize], w, h));
        }
        let levels = cpu.iter().map(|l| upload_level(ctx, l)).collect();
        Self { levels }
    }

    pub fn level_count(&self) -> u32 {
        self.levels.len() as u32
    }

    pub fn level_size(&self, lod: u32) -> (u32, u32) {
        let l = &self.levels[lod as usize];
        (l.width, l.height)
    }

    pub fn level(&self, lod: u32) -> PipelineImage {
        self.levels[lod as usize].clone()
    }
}

fn upload_level(ctx: &GpuContext, img: &LinearRgbaF32) -> PipelineImage {
    let texels: Vec<f16> = img.pixels.iter().map(|&v| f16::from_f32(v)).collect();
    let texture = ctx.device.create_texture_with_data(
        &ctx.queue,
        &wgpu::TextureDescriptor {
            label: Some("gpu-pyramid-level"),
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

/// 2×2-average downsample to `(dst_w, dst_h)` (box filter; adequate for the edit
/// source pyramid). Mirrors `ferrolite_vt::source`'s box_downsample.
fn box_downsample(src: &LinearRgbaF32, dst_w: u32, dst_h: u32) -> LinearRgbaF32 {
    let mut px = vec![0.0f32; LinearRgbaF32::expected_len(dst_w, dst_h)];
    for dy in 0..dst_h {
        for dx in 0..dst_w {
            let sx0 = (dx * src.width / dst_w).min(src.width - 1);
            let sy0 = (dy * src.height / dst_h).min(src.height - 1);
            let sx1 = (sx0 + 1).min(src.width - 1);
            let sy1 = (sy0 + 1).min(src.height - 1);
            let mut acc = [0.0f32; 4];
            for &(x, y) in &[(sx0, sy0), (sx1, sy0), (sx0, sy1), (sx1, sy1)] {
                let i = ((y * src.width + x) * 4) as usize;
                for (c, acc_c) in acc.iter_mut().enumerate() {
                    *acc_c += src.pixels[i + c];
                }
            }
            let di = ((dy * dst_w + dx) * 4) as usize;
            for (c, acc_c) in acc.iter().enumerate() {
                px[di + c] = acc_c * 0.25;
            }
        }
    }
    LinearRgbaF32::new(dst_w, dst_h, px).expect("downsample length")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_gpu::GpuContext;
    use ferrolite_image::LinearRgbaF32;

    #[test]
    fn pyramid_levels_match_image_math() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let full = LinearRgbaF32::black(1024, 512);
        let p = GpuPyramidSource::new(&ctx, &full);
        assert_eq!(
            p.level_count(),
            ferrolite_image::pyramid_level_count(1024, 512)
        );
        assert_eq!(p.level_size(0), (1024, 512));
        assert_eq!(p.level_size(1), (512, 256));
        // Each level wraps a same-size texture.
        let l1 = p.level(1);
        assert_eq!((l1.width, l1.height), (512, 256));
    }
}
