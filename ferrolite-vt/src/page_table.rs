//! Rung 4: page-table indirection + a GPU feedback buffer. The display shader
//! resolves virtual→physical via the page table and marks needed tiles into the
//! feedback buffer; the CPU reads it back (one frame latent) to drive streaming.

use ferrolite_gpu::GpuContext;
use ferrolite_image::TileCoord;

/// Flat-indexing of all tiles across all levels (cols/rows per level + offsets).
pub struct LevelLayout {
    dims: Vec<(u32, u32)>, // (cols, rows) per lod
    offsets: Vec<u32>,
    total: u32,
}

impl LevelLayout {
    pub fn new(dims: &[(u32, u32)]) -> Self {
        let mut offsets = Vec::with_capacity(dims.len());
        let mut acc = 0u32;
        for &(c, r) in dims {
            offsets.push(acc);
            acc += c * r;
        }
        Self {
            dims: dims.to_vec(),
            offsets,
            total: acc,
        }
    }
    pub fn flat_index(&self, lod: u32, x: u32, y: u32) -> u32 {
        let (cols, _rows) = self.dims[lod as usize];
        self.offsets[lod as usize] + y * cols + x
    }
    pub fn total(&self) -> u32 {
        self.total
    }
    pub fn offsets(&self) -> &[u32] {
        &self.offsets
    }
    /// Number of LOD levels this layout covers.
    pub fn level_count(&self) -> u32 {
        self.dims.len() as u32
    }
    /// (cols, rows) of `lod`.
    pub fn dims(&self, lod: u32) -> (u32, u32) {
        self.dims[lod as usize]
    }
    pub fn from_flat(&self, flat: u32) -> TileCoord {
        // Inverse mapping for feedback read-back.
        let lod = self.offsets.iter().rposition(|&o| flat >= o).unwrap_or(0);
        let local = flat - self.offsets[lod];
        let (cols, _) = self.dims[lod];
        TileCoord {
            lod: lod as u32,
            x: local % cols,
            y: local / cols,
        }
    }
}

/// Flags stored in the `.g` channel of a page-table texel. A `0` flag means the
/// slot in `.r` is authoritative (resident if != NOT_RESIDENT).
const FLAG_RESIDENT: u32 = 1;

/// Page-table indirection texture: one `Rg32Uint` texel per virtual tile (laid
/// out by `LevelLayout`), storing `(slot, flags)`. The shader resolves a tile's
/// physical slot with `textureLoad(page_table, vec2<i32>(i32(flat), 0), 0).r`.
pub struct PageTable {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    total: u32,
}

impl PageTable {
    pub fn new(ctx: &GpuContext, total: u32) -> Self {
        let total = total.max(1);
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vt-page-table"),
            size: wgpu::Extent3d {
                width: total,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg32Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            total,
        }
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Rewrite the whole page table from the CPU slot mirror. `slots[flat]` is the
    /// physical slot for virtual tile `flat` (or `NOT_RESIDENT`). The `.g` flag is
    /// set to `FLAG_RESIDENT` for resident texels, `0` otherwise.
    pub fn update(&self, ctx: &GpuContext, slots: &[u32]) {
        debug_assert!(slots.len() as u32 <= self.total);
        // Pack (slot, flags) per texel into a contiguous Rg32Uint row.
        let mut data: Vec<u32> = Vec::with_capacity(self.total as usize * 2);
        for i in 0..self.total as usize {
            let slot = slots.get(i).copied().unwrap_or(crate::pool::NOT_RESIDENT);
            let flag = if slot == crate::pool::NOT_RESIDENT {
                0
            } else {
                FLAG_RESIDENT
            };
            data.push(slot);
            data.push(flag);
        }
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&data),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(self.total * 8), // Rg32Uint = 8 bytes/texel
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: self.total,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }
}

/// GPU feedback buffer: `total` `u32`s the display shader marks (1 = "this tile
/// was wanted this frame"). The CPU reads it back one frame latent to learn the
/// GPU-truth visible set, then clears it for the next frame.
pub struct FeedbackBuffer {
    buffer: wgpu::Buffer,
    total: u32,
}

impl FeedbackBuffer {
    pub fn new(ctx: &GpuContext, total: u32) -> Self {
        let total = total.max(1);
        let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-feedback"),
            size: (total as u64) * 4,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { buffer, total }
    }

    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// Zero the feedback buffer so the next frame starts from a clean slate.
    pub fn clear(&self, ctx: &GpuContext) {
        let zeros = vec![0u32; self.total as usize];
        ctx.queue
            .write_buffer(&self.buffer, 0, bytemuck::cast_slice(&zeros));
    }

    /// Copy the feedback buffer to the CPU and return the set of marked tiles.
    /// One frame latent: reflects what the shader wanted on the last submitted
    /// frame. Mirrors the readback mechanics of `GpuContext::read_rgba8`.
    pub fn read_back(&self, ctx: &GpuContext, layout: &LevelLayout) -> Vec<TileCoord> {
        let size = (self.total as u64) * 4;
        let staging = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-feedback-readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(&self.buffer, 0, &staging, 0, size);
        ctx.queue.submit([enc.finish()]);

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        ctx.device.poll(wgpu::Maintain::Wait);

        let data = slice.get_mapped_range();
        let marks: &[u32] = bytemuck::cast_slice(&data);
        let mut out = Vec::new();
        for (flat, &mark) in marks.iter().enumerate() {
            if mark != 0 {
                out.push(layout.from_flat(flat as u32));
            }
        }
        drop(data);
        staging.unmap();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_index_round_trips_level_tile() {
        // level_offsets: L0 at 0 (4x4=16 tiles), L1 at 16 (2x2=4), L2 at 20 (1).
        let layout = LevelLayout::new(&[(4, 4), (2, 2), (1, 1)]);
        assert_eq!(layout.flat_index(0, 0, 0), 0);
        assert_eq!(layout.flat_index(0, 3, 3), 15);
        assert_eq!(layout.flat_index(1, 0, 0), 16);
        assert_eq!(layout.flat_index(2, 0, 0), 20);
        assert_eq!(layout.total(), 21);

        // Round-trip every flat index back through from_flat.
        for (lod, &(cols, rows)) in [(4u32, 4u32), (2, 2), (1, 1)].iter().enumerate() {
            for y in 0..rows {
                for x in 0..cols {
                    let flat = layout.flat_index(lod as u32, x, y);
                    let tc = layout.from_flat(flat);
                    assert_eq!(
                        tc,
                        TileCoord {
                            lod: lod as u32,
                            x,
                            y
                        }
                    );
                }
            }
        }
    }
}
