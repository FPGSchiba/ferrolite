//! Full-res tiled export render. Reuses the Spec 2 GPU tile producer
//! (`TileEditPipeline::produce_tile`) to render the edited image one
//! `TILE_SIZE²` tile at a time, reads each tile back to the CPU, converts
//! working→output + OETF, and quantizes into the final RGB buffer. No
//! whole-image RGBA16F/f32 CPU buffer is ever allocated (CLAUDE.md §1/§2;
//! spec §8.1).

use std::sync::Arc;

use ferrolite_color::{working_to_output, WorkingSpace};
use ferrolite_gpu::GpuContext;
use ferrolite_image::{tile_pixel_origin, TileCoord, TILE_SIZE};
use ferrolite_jobs::CancelToken;
use ferrolite_pipeline::{edited_output_dims, GpuPyramidSource, OpStack, TileEditPipeline};
use half::f16;

use crate::convert::{convert_pixel, to_u16, to_u8};
use crate::error::ExportError;
use crate::options::BitDepth;

#[derive(Debug, Clone)]
pub enum PixelData {
    Eight(Vec<u8>),
    Sixteen(Vec<u16>),
}

#[derive(Debug, Clone)]
pub struct RenderedImage {
    pub width: u32,
    pub height: u32,
    pub data: PixelData,
}

/// Read a `TILE_SIZE²` `Rgba16Float` tile texture (COPY_SRC) back to the CPU as
/// f32 RGBA (row-unpadded, len = TILE_SIZE*TILE_SIZE*4). Blocks on the device.
fn read_tile_rgba16f(ctx: &GpuContext, tex: &wgpu::Texture) -> Vec<f32> {
    let dim = TILE_SIZE;
    let channels = 4u32;
    let bpp = 2u32; // f16
    let bpr_unpadded = dim * channels * bpp;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bpr_padded = bpr_unpadded.div_ceil(align) * align;

    let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("export-tile-readback"),
        size: (bpr_padded * dim) as u64,
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
                rows_per_image: Some(dim),
            },
        },
        wgpu::Extent3d {
            width: dim,
            height: dim,
            depth_or_array_layers: 1,
        },
    );
    ctx.queue.submit([enc.finish()]);

    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    ctx.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();

    let mut out = Vec::with_capacity((dim * dim * channels) as usize);
    for row in 0..dim {
        let start = (row * bpr_padded) as usize;
        let end = start + (bpr_unpadded) as usize;
        let row_u16: &[u16] = bytemuck::cast_slice(&data[start..end]);
        for &h in row_u16 {
            out.push(f16::from_bits(h).to_f32());
        }
    }
    drop(data);
    buf.unmap();
    out
}

/// Render the full-res edited image to a quantized RGB buffer, tile by tile.
/// `camera_to_working` is the row-major 3×3 for the open image + working space
/// (from the app's `camera_to_working()`); `working_space`→`output_space` drives
/// the output conversion. Checks `cancel` once per tile and reports `(done,total)`.
#[allow(clippy::too_many_arguments)] // spec §8.1 public interface (task brief); each param is
                                     // an independent required input, not a natural group.
pub fn render_tiled(
    ctx: &Arc<GpuContext>,
    pyramid: &Arc<GpuPyramidSource>,
    stack: &OpStack,
    camera_to_working: [[f32; 3]; 3],
    working_space: WorkingSpace,
    output_space: WorkingSpace,
    depth: BitDepth,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(u32, u32),
) -> Result<RenderedImage, ExportError> {
    let (src_w, src_h) = pyramid.level_size(0);
    let (out_w, out_h) = edited_output_dims(stack, src_w, src_h);
    if out_w == 0 || out_h == 0 {
        return Err(ExportError::Render("zero output dimensions".into()));
    }

    // Build the per-tile edit pipeline ONCE for this export (CLAUDE.md GPU rule).
    let mut pipeline = TileEditPipeline::new(
        ctx.clone(),
        pyramid.clone(),
        stack.clone(),
        camera_to_working,
    );

    let m = working_to_output(working_space, output_space); // ferrolite_color::Mat3

    let tiles_x = out_w.div_ceil(TILE_SIZE);
    let tiles_y = out_h.div_ceil(TILE_SIZE);
    let total = tiles_x * tiles_y;

    // Final quantized RGB buffer (3 or 6 bytes/px). This is the only full-image
    // CPU allocation — no whole-image f32/RGBA16F.
    let px_count = (out_w * out_h) as usize;
    let mut buf8: Vec<u8> = Vec::new();
    let mut buf16: Vec<u16> = Vec::new();
    match depth {
        BitDepth::Eight => buf8 = vec![0u8; px_count * 3],
        BitDepth::Sixteen => buf16 = vec![0u16; px_count * 3],
    }

    let mut done = 0u32;
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            if cancel.is_cancelled() {
                return Err(ExportError::Cancelled);
            }
            let coord = TileCoord {
                lod: 0,
                x: tx,
                y: ty,
            };
            let tile_tex = pipeline.produce_tile(coord);
            let rgba = read_tile_rgba16f(ctx, &tile_tex); // len TILE_SIZE²*4 f32

            let (ox, oy) = tile_pixel_origin(coord); // interior top-left in output
            for row in 0..TILE_SIZE {
                let py = oy + row;
                if py >= out_h {
                    break;
                }
                for col in 0..TILE_SIZE {
                    let px = ox + col;
                    if px >= out_w {
                        break;
                    }
                    let ti = ((row * TILE_SIZE + col) * 4) as usize;
                    let rgb_lin = [rgba[ti], rgba[ti + 1], rgba[ti + 2]];
                    let enc = convert_pixel(rgb_lin, &m, output_space);
                    let di = ((py * out_w + px) * 3) as usize;
                    match depth {
                        BitDepth::Eight => {
                            let q = to_u8(enc);
                            buf8[di] = q[0];
                            buf8[di + 1] = q[1];
                            buf8[di + 2] = q[2];
                        }
                        BitDepth::Sixteen => {
                            let q = to_u16(enc);
                            buf16[di] = q[0];
                            buf16[di + 1] = q[1];
                            buf16[di + 2] = q[2];
                        }
                    }
                }
            }
            done += 1;
            progress(done, total);
        }
    }

    let data = match depth {
        BitDepth::Eight => PixelData::Eight(buf8),
        BitDepth::Sixteen => PixelData::Sixteen(buf16),
    };
    Ok(RenderedImage {
        width: out_w,
        height: out_h,
        data,
    })
}
