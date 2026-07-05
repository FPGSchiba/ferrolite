//! Shared GPU golden-test helpers (mirrors ferrolite-pipeline/tests/common).
//! Golden PNGs are authored on the dev GPU (UPDATE_GOLDEN=1 or delete the
//! fixture) and committed; headless CI skips the GPU tests before reaching here.
#![allow(dead_code)]

use ferrolite_gpu::GpuContext;
use ferrolite_mask::MaskBuffer;
use half::f16;

/// Read an `R32Float` `MaskBuffer` back to a row-unpadded `Vec<f32>`
/// (`width*height` values). Test-only; production never reads masks back.
pub fn read_r32f(ctx: &GpuContext, buf: &MaskBuffer) -> Vec<f32> {
    let (w, h) = (buf.width, buf.height);
    let bpp = 4u32; // R32Float = 4 bytes
    let bpr_unpadded = w * bpp;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bpr_padded = bpr_unpadded.div_ceil(align) * align;
    let readback = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mask-readback"),
        size: (bpr_padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &buf.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &readback,
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
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    ctx.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let mut out = vec![0.0f32; (w * h) as usize];
    for row in 0..h {
        let start = (row * bpr_padded) as usize;
        for x in 0..w {
            let o = start + x as usize * 4;
            out[(row * w + x) as usize] =
                f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
        }
    }
    drop(data);
    readback.unmap();
    out
}

/// Upload an `Rgba16Float` texture from a per-pixel closure (a generic color
/// input for range-shape tests; stands in for the photo pipeline's texture).
pub fn upload_rgba16f(
    ctx: &GpuContext,
    w: u32,
    h: u32,
    f: impl Fn(u32, u32) -> [f32; 4],
) -> wgpu::Texture {
    use wgpu::util::DeviceExt;
    let mut texels: Vec<f16> = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            for c in f(x, y) {
                texels.push(f16::from_f32(c));
            }
        }
    }
    ctx.device.create_texture_with_data(
        &ctx.queue,
        &wgpu::TextureDescriptor {
            label: Some("range-input"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(&texels),
    )
}

pub fn mask_max_abs_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

const TOL: u8 = 4; // absorbs driver float differences (matches pipeline goldens)

/// Compare mask `values` in [0,1] against `tests/fixtures/<name>` as an L8
/// grayscale PNG. Authors the golden if absent or `UPDATE_GOLDEN` is set.
pub fn assert_mask_golden(values: &[f32], w: u32, h: u32, name: &str) {
    let quantized: Vec<u8> = values
        .iter()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        .collect();
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    if std::env::var("UPDATE_GOLDEN").is_ok() || !std::path::Path::new(&path).exists() {
        std::fs::create_dir_all(format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"))).unwrap();
        image::save_buffer(&path, &quantized, w, h, image::ColorType::L8).unwrap();
        eprintln!("wrote golden {path}");
        return;
    }
    let golden = image::open(&path).unwrap().to_luma8();
    assert_eq!(golden.dimensions(), (w, h), "golden dims mismatch: {name}");
    assert!(
        mask_max_abs_diff(&quantized, golden.as_raw()) <= TOL,
        "{name}: mask drifted from golden beyond tolerance"
    );
}
