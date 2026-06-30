//! Shared golden-test helpers (mirrors ferrolite-vt/tests/common). Golden PNGs
//! are authored on the dev GPU (set UPDATE_GOLDEN=1 or delete the fixture) and
//! committed; in headless CI the GPU tests skip before reaching these.

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;

/// A deterministic RGB gradient used as the edit source.
pub fn gradient(w: u32, h: u32) -> LinearRgbaF32 {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[x as f32 / w as f32, y as f32 / h as f32, 0.25, 1.0]);
        }
    }
    LinearRgbaF32::new(w, h, px).expect("gradient length")
}

pub fn max_abs_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

/// Read a `TILE_SIZE`² `Rgba16Float` GPU texture back to display-linear f32 RGBA
/// on the CPU (test-only; the production produce path never reads back).
pub fn read_tile_linear(ctx: &GpuContext, tex: &wgpu::Texture) -> Vec<f32> {
    use ferrolite_image::TILE_SIZE;
    let bpp = 8u32; // RGBA16F
    let bpr_unpadded = TILE_SIZE * bpp;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bpr_padded = bpr_unpadded.div_ceil(align) * align;
    let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tile-readback"),
        size: (bpr_padded * TILE_SIZE) as u64,
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
                rows_per_image: Some(TILE_SIZE),
            },
        },
        wgpu::Extent3d {
            width: TILE_SIZE,
            height: TILE_SIZE,
            depth_or_array_layers: 1,
        },
    );
    ctx.queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    ctx.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let mut out = vec![0.0f32; (TILE_SIZE * TILE_SIZE * 4) as usize];
    for row in 0..TILE_SIZE {
        let start = (row * bpr_padded) as usize;
        for px in 0..(TILE_SIZE * 4) {
            let o = start + px as usize * 2;
            let h = half::f16::from_le_bytes([data[o], data[o + 1]]);
            out[(row * TILE_SIZE * 4 + px) as usize] = h.to_f32();
        }
    }
    drop(data);
    buf.unmap();
    out
}

/// Read an arbitrary-size `Rgba16Float` GPU texture back to display-linear f32
/// RGBA (test-only).
pub fn read_image_linear(ctx: &GpuContext, img: &ferrolite_pipeline::PipelineImage) -> Vec<f32> {
    let (w, h) = (img.width, img.height);
    let bpp = 8u32;
    let bpr_unpadded = w * bpp;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bpr_padded = bpr_unpadded.div_ceil(align) * align;
    let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("img-readback"),
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
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    ctx.queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    ctx.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let mut out = vec![0.0f32; (w * h * 4) as usize];
    for row in 0..h {
        let start = (row * bpr_padded) as usize;
        for px in 0..(w * 4) {
            let o = start + px as usize * 2;
            let hf = half::f16::from_le_bytes([data[o], data[o + 1]]);
            out[(row * w * 4 + px) as usize] = hf.to_f32();
        }
    }
    drop(data);
    buf.unmap();
    out
}

const TOL: u8 = 4; // absorbs driver float differences

/// Compare `pixels` against `tests/fixtures/<name>`. Authors the golden if the
/// file is absent or UPDATE_GOLDEN is set (then returns, passing).
pub fn assert_golden(pixels: &[u8], w: u32, h: u32, name: &str) {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    if std::env::var("UPDATE_GOLDEN").is_ok() || !std::path::Path::new(&path).exists() {
        std::fs::create_dir_all(format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"))).unwrap();
        image::save_buffer(&path, pixels, w, h, image::ColorType::Rgba8).unwrap();
        eprintln!("wrote golden {path}");
        return;
    }
    let golden = image::open(&path).unwrap().to_rgba8();
    assert_eq!(golden.dimensions(), (w, h), "golden dims mismatch: {name}");
    assert!(
        max_abs_diff(pixels, golden.as_raw()) <= TOL,
        "{name}: rendered output drifted from golden beyond tolerance"
    );
}
