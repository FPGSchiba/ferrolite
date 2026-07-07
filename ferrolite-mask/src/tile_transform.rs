//! Tile-space placement for compositing a mask into a sub-region of the full
//! image. Spatial shape passes are pure functions of a full-image-normalized
//! uv; when a mask is composited at a tile's own (haloed) resolution, this maps
//! each composite-buffer pixel back to the full-image uv the shape expects, and
//! supplies the brush rasterizer's tile origin + level dims. `whole_image` is
//! the identity used by the preview / UI-overlay paths (composite spans the
//! whole level 1:1), which reduces every consumer to its pre-tiling behavior.

/// Placement of a composite buffer within its full image level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileTransform {
    /// Haloed tile origin in the tile's LOD level pixel space (may be negative
    /// at the top/left edges).
    pub origin: [i32; 2],
    /// Full dimensions of the tile's LOD level (pixels).
    pub level_dims: [u32; 2],
}

impl TileTransform {
    /// Identity: the composite buffer IS the whole level, 1:1.
    pub fn whole_image(w: u32, h: u32) -> Self {
        Self {
            origin: [0, 0],
            level_dims: [w, h],
        }
    }

    /// uv scale + offset mapping a composite-local uv in `[0,1]^2` (over the
    /// `w`x`h` composite buffer) to full-image-normalized uv:
    /// `uv_full = uv_local * scale + offset`. For `whole_image(w,h)` this is
    /// `scale = [1,1]`, `offset = [0,0]`.
    pub fn uv_scale_offset(&self, w: u32, h: u32) -> ([f32; 2], [f32; 2]) {
        let lw = self.level_dims[0].max(1) as f32;
        let lh = self.level_dims[1].max(1) as f32;
        (
            [w as f32 / lw, h as f32 / lh],
            [self.origin[0] as f32 / lw, self.origin[1] as f32 / lh],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_image_is_identity_uv() {
        let t = TileTransform::whole_image(100, 60);
        let (scale, offset) = t.uv_scale_offset(100, 60);
        assert_eq!(scale, [1.0, 1.0]);
        assert_eq!(offset, [0.0, 0.0]);
    }

    #[test]
    fn tile_maps_composite_uv_to_full_image_uv() {
        // A 40x40 composite buffer placed at level-pixel origin (100, 20) inside
        // a 400x400 level. uv_full = (origin + composite_px) / level_dims.
        let t = TileTransform {
            origin: [100, 20],
            level_dims: [400, 400],
        };
        let (scale, offset) = t.uv_scale_offset(40, 40);
        // scale = extent/level = 40/400 = 0.1 ; offset = origin/level = 0.25, 0.05
        assert!((scale[0] - 0.1).abs() < 1e-6);
        assert!((scale[1] - 0.1).abs() < 1e-6);
        assert!((offset[0] - 0.25).abs() < 1e-6);
        assert!((offset[1] - 0.05).abs() < 1e-6);
        // A composite-local uv of 0.5 (pixel center of the 40px buffer) maps to
        // full uv = 0.5*0.1 + 0.25 = 0.30 == (100 + 20)/400.
        let uv_full_x = 0.5 * scale[0] + offset[0];
        assert!((uv_full_x - 0.30).abs() < 1e-6);
    }
}
