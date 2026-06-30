//! Pure zoom/pan/fit + LOD selection math. No egui, no GPU — unit-testable.

/// View transform: `zoom` (screen px per image px) and `pan` (image-space px
/// offset of the viewport center).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewTransform {
    pub zoom: f32,
    pub pan: (f32, f32),
}

impl ViewTransform {
    /// Scale so the whole image fits the viewport; centered (pan 0,0).
    pub fn fit(image: (u32, u32), viewport: (f32, f32)) -> Self {
        let zx = viewport.0 / image.0 as f32;
        let zy = viewport.1 / image.1 as f32;
        Self {
            zoom: zx.min(zy).max(f32::MIN_POSITIVE),
            pan: (0.0, 0.0),
        }
    }

    /// LOD whose texels are ~1 screen pixel: `lod = floor(log2(1/zoom))`, clamped.
    pub fn lod_for(&self, _image: (u32, u32), max_lod: u32) -> u32 {
        if self.zoom >= 1.0 {
            return 0;
        }
        let l = (1.0 / self.zoom).log2().floor();
        (l.max(0.0) as u32).min(max_lod.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_centers_and_scales_to_viewport() {
        // 2000x1000 image into 1000x1000 viewport -> zoom 0.5 (width-bound).
        let t = ViewTransform::fit((2000, 1000), (1000.0, 1000.0));
        assert!((t.zoom - 0.5).abs() < 1e-6);
    }

    #[test]
    fn lod_increases_as_zoom_decreases() {
        // Zoomed way out -> coarse LOD; at 1:1 -> LOD 0.
        let mut t = ViewTransform {
            zoom: 1.0,
            pan: (0.0, 0.0),
        };
        assert_eq!(t.lod_for((4096, 4096), 6), 0);
        t.zoom = 0.25; // 1 screen px = 4 image px -> LOD ~2
        assert_eq!(t.lod_for((4096, 4096), 6), 2);
    }
}
