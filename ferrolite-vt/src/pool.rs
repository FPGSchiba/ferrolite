//! Physical tile pool: a budget-limited array texture of `TILE_SIZE`² slots,
//! plus a CPU allocator mapping resident tiles to slot indices.

use std::collections::HashMap;

use ferrolite_gpu::GpuContext;
use ferrolite_image::{LinearRgbaF32, TileCoord, TILE_SIZE};
use half::f16;

/// Sentinel slot index meaning "this tile is not resident in the pool". Matches
/// the WGSL `const NOT_RESIDENT: u32 = 0xFFFFFFFFu;` in `display.wgsl`.
pub const NOT_RESIDENT: u32 = 0xFFFF_FFFF;

/// Maps tiles to physical slot indices; recycles freed slots.
pub struct SlotAllocator {
    capacity: u32,
    free: Vec<u32>,
    map: HashMap<TileCoord, u32>,
}

impl SlotAllocator {
    pub fn new(capacity: u32) -> Self {
        Self {
            capacity,
            free: (0..capacity).rev().collect(),
            map: HashMap::new(),
        }
    }
    pub fn slot_of(&self, t: TileCoord) -> Option<u32> {
        self.map.get(&t).copied()
    }
    pub fn alloc(&mut self, t: TileCoord) -> Option<u32> {
        if let Some(&s) = self.map.get(&t) {
            return Some(s);
        }
        let s = self.free.pop()?;
        self.map.insert(t, s);
        Some(s)
    }
    pub fn free(&mut self, t: TileCoord) {
        if let Some(s) = self.map.remove(&t) {
            self.free.push(s);
        }
    }
    pub fn capacity(&self) -> u32 {
        self.capacity
    }
}

/// GPU side: an array texture of `capacity` `Rgba16Float` `TILE_SIZE`² layers.
pub struct TilePool {
    texture: wgpu::Texture,
    capacity: u32,
}

impl TilePool {
    pub fn new(ctx: &GpuContext, capacity: u32) -> Self {
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vt-tile-pool"),
            size: wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: capacity.max(1),
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        Self { texture, capacity }
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Upload one tile's pixels into physical `slot` (array layer).
    pub fn upload(&self, ctx: &GpuContext, slot: u32, tile: &LinearRgbaF32) {
        let texels: Vec<f16> = tile.pixels.iter().map(|&v| f16::from_f32(v)).collect();
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: slot,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&texels),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(TILE_SIZE * 4 * 2), // RGBA * f16
                rows_per_image: Some(TILE_SIZE),
            },
            wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_image::TileCoord;

    #[test]
    fn slot_allocator_reuses_evicted_slots() {
        let mut a = SlotAllocator::new(2);
        let s0 = a.alloc(TileCoord { lod: 0, x: 0, y: 0 }).unwrap();
        let s1 = a.alloc(TileCoord { lod: 0, x: 1, y: 0 }).unwrap();
        assert_ne!(s0, s1);
        assert!(
            a.alloc(TileCoord { lod: 0, x: 2, y: 0 }).is_none(),
            "pool full"
        );
        a.free(TileCoord { lod: 0, x: 0, y: 0 });
        let s2 = a.alloc(TileCoord { lod: 0, x: 2, y: 0 }).unwrap();
        assert_eq!(s2, s0, "freed slot reused");
    }
}
