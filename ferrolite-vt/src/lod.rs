//! Pure trilinear-LOD level/fraction math. Mirrors the `display.wgsl` blend so the
//! level selection is unit-testable without a GPU. No photo concepts, no wgpu.

/// `(lo, hi, frac)` for a texel density `d` (image px per screen px). `lo` is the
/// sharper level, `hi` the next-coarser, `frac` the blend weight toward `hi`.
pub fn lod_levels(texel_density: f32, level_count: u32) -> (u32, u32, f32) {
    let d = texel_density.max(1.0);
    let l = d.log2().max(0.0);
    let max = level_count.saturating_sub(1);
    let lo = (l.floor() as u32).min(max);
    let hi = (lo + 1).min(max);
    let frac = if lo == hi { 0.0 } else { l.fract() };
    (lo, hi, frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_power_of_two_has_zero_fraction() {
        // d = 4 -> log2 = 2.0 -> lo=2, hi=3, frac=0.
        let (lo, hi, frac) = lod_levels(4.0, 6);
        assert_eq!((lo, hi), (2, 3));
        assert!(frac.abs() < 1e-6);
    }

    #[test]
    fn midpoint_blends_half() {
        // d = 2^2.5 ~ 5.657 -> lo=2, hi=3, frac~0.5.
        let (lo, hi, frac) = lod_levels(2f32.powf(2.5), 6);
        assert_eq!((lo, hi), (2, 3));
        assert!((frac - 0.5).abs() < 1e-3);
    }

    #[test]
    fn clamps_at_coarsest_level_with_no_blend() {
        // Very coarse density clamps lo=hi=max, frac=0 (nothing coarser to blend).
        let (lo, hi, frac) = lod_levels(1024.0, 4);
        assert_eq!((lo, hi), (3, 3));
        assert!(frac.abs() < 1e-6);
    }

    #[test]
    fn density_below_one_is_lod_zero() {
        let (lo, hi, frac) = lod_levels(0.5, 6);
        assert_eq!(lo, 0);
        assert_eq!(hi, 1);
        assert!(frac.abs() < 1e-6);
    }
}
