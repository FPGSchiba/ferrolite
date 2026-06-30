//! Tile coordinate vocabulary and LOD-pyramid math. Pure, GPU-free, photo-free
//! so it stays in the engine-transferable tier and is testable without a device.

/// Edge length of a square virtual tile, in pixels.
pub const TILE_SIZE: u32 = 256;

/// Address of one virtual tile: mip level + tile column/row within that level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub lod: u32,
    pub x: u32,
    pub y: u32,
}

/// Pixel size of `lod`, halving each level (floor), clamped to a 1px minimum.
pub fn level_size(width: u32, height: u32, lod: u32) -> (u32, u32) {
    let w = (width >> lod).max(1);
    let h = (height >> lod).max(1);
    (w, h)
}

/// Number of LOD levels: keep halving until both dims fit within one tile.
pub fn pyramid_level_count(width: u32, height: u32) -> u32 {
    let mut lod = 0u32;
    loop {
        let (w, h) = level_size(width, height, lod);
        if w <= TILE_SIZE && h <= TILE_SIZE {
            return lod + 1;
        }
        lod += 1;
    }
}

/// Tile grid dimensions of `lod` (ceil-division by `TILE_SIZE`).
pub fn tiles_per_level(width: u32, height: u32, lod: u32) -> (u32, u32) {
    let (w, h) = level_size(width, height, lod);
    (w.div_ceil(TILE_SIZE), h.div_ceil(TILE_SIZE))
}

/// Top-left pixel of `coord` within its own LOD level.
pub fn tile_pixel_origin(coord: TileCoord) -> (u32, u32) {
    (coord.x * TILE_SIZE, coord.y * TILE_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_count_reaches_single_tile_top() {
        // 1024x512 -> L0 1024x512, L1 512x256, L2 256x128 (fits one tile both dims) => 3 levels
        assert_eq!(pyramid_level_count(1024, 512), 3);
        // exactly one tile already => 1 level
        assert_eq!(pyramid_level_count(256, 256), 1);
        // smaller than a tile => still 1 level
        assert_eq!(pyramid_level_count(100, 10), 1);
    }

    #[test]
    fn level_size_halves_each_lod_min_one() {
        assert_eq!(level_size(1024, 512, 0), (1024, 512));
        assert_eq!(level_size(1024, 512, 1), (512, 256));
        assert_eq!(level_size(1024, 512, 2), (256, 128));
        // never collapses below 1
        assert_eq!(level_size(1, 1, 5), (1, 1));
    }

    #[test]
    fn tiles_per_level_is_ceil_div_tile_size() {
        assert_eq!(tiles_per_level(512, 256, 0), (2, 1));
        assert_eq!(tiles_per_level(513, 256, 0), (3, 1)); // ceil
        assert_eq!(tiles_per_level(1024, 512, 1), (2, 1)); // 512x256 at L1
    }

    #[test]
    fn tile_origin_multiplies_by_tile_size() {
        assert_eq!(tile_pixel_origin(TileCoord { lod: 0, x: 0, y: 0 }), (0, 0));
        assert_eq!(
            tile_pixel_origin(TileCoord { lod: 3, x: 2, y: 1 }),
            (512, 256)
        );
    }
}
