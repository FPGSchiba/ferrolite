//! Source-agnostic tile supply (cross-cutting contract §5). The VT consumes a
//! `TileSource`; it never knows what produced the pixels. `PyramidTileSource`
//! builds an in-memory LOD pyramid (box-downsample) from one full image.

use ferrolite_image::{
    level_size as img_level_size, pyramid_level_count, tile_pixel_origin, LinearRgbaF32, TileCoord,
    TILE_SIZE,
};

pub trait TileSource {
    fn level_count(&self) -> u32;
    fn level_size(&self, lod: u32) -> (u32, u32);
    /// A `TILE_SIZE`² tile, edge-clamped where the tile overhangs the level.
    fn tile(&self, coord: TileCoord) -> LinearRgbaF32;
}

pub struct PyramidTileSource {
    levels: Vec<LinearRgbaF32>, // index = lod
}

impl PyramidTileSource {
    pub fn new(full: LinearRgbaF32) -> Self {
        let count = pyramid_level_count(full.width, full.height);
        let mut levels = Vec::with_capacity(count as usize);
        levels.push(full);
        for lod in 1..count {
            let (w, h) = img_level_size(levels[0].width, levels[0].height, lod);
            levels.push(box_downsample(&levels[(lod - 1) as usize], w, h));
        }
        Self { levels }
    }
}

impl TileSource for PyramidTileSource {
    fn level_count(&self) -> u32 {
        self.levels.len() as u32
    }
    fn level_size(&self, lod: u32) -> (u32, u32) {
        let l = &self.levels[lod as usize];
        (l.width, l.height)
    }
    fn tile(&self, coord: TileCoord) -> LinearRgbaF32 {
        let level = &self.levels[coord.lod as usize];
        let (ox, oy) = tile_pixel_origin(coord);
        let mut px = Vec::with_capacity((TILE_SIZE * TILE_SIZE * 4) as usize);
        for ty in 0..TILE_SIZE {
            for tx in 0..TILE_SIZE {
                let sx = (ox + tx).min(level.width - 1);
                let sy = (oy + ty).min(level.height - 1);
                let i = ((sy * level.width + sx) * 4) as usize;
                px.extend_from_slice(&level.pixels[i..i + 4]);
            }
        }
        LinearRgbaF32::new(TILE_SIZE, TILE_SIZE, px).expect("tile length")
    }
}

/// Simple 2×2-average downsample to `(dst_w, dst_h)`. (Box filter is adequate for
/// the display pyramid; `fast_image_resize` can replace this for quality later.)
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
    use ferrolite_image::{LinearRgbaF32, TileCoord, TILE_SIZE};

    fn solid(w: u32, h: u32, rgb: [f32; 3]) -> LinearRgbaF32 {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
        }
        LinearRgbaF32::new(w, h, px).unwrap()
    }

    #[test]
    fn level_count_matches_pyramid_math() {
        let src = PyramidTileSource::new(solid(1024, 512, [0.5, 0.5, 0.5]));
        assert_eq!(
            src.level_count(),
            ferrolite_image::pyramid_level_count(1024, 512)
        );
        assert_eq!(src.level_size(1), (512, 256));
    }

    #[test]
    fn tile_is_tile_sized_and_edge_clamped() {
        let src = PyramidTileSource::new(solid(300, 300, [1.0, 0.0, 0.0]));
        let t = src.tile(TileCoord { lod: 0, x: 0, y: 0 });
        assert_eq!((t.width, t.height), (TILE_SIZE, TILE_SIZE));
        // Interior pixel is red; out-of-image area is edge-clamped (also red here).
        assert_eq!(&t.pixels[0..4], &[1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn tile_interior_pixel_value() {
        let src = PyramidTileSource::new(solid(512, 512, [0.2, 0.4, 0.6]));
        let t = src.tile(TileCoord { lod: 0, x: 1, y: 1 });
        // Tile (1,1) starts at pixel (256, 256)
        assert_eq!((t.width, t.height), (TILE_SIZE, TILE_SIZE));
        // First pixel should be the color from (256, 256) in the level
        assert_eq!(&t.pixels[0..4], &[0.2, 0.4, 0.6, 1.0]);
    }
}
