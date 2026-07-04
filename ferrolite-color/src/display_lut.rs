//! Monitor-profile 3D-LUT bake: `working→monitor` baked from a parsed ICC via
//! moxcms, indexed through a gamma shaper. GPU-agnostic (data only).

use crate::error::ColorError;

/// Cube edge length of the display LUT (nodes per axis). Mirrored by
/// `ferrolite-vt`'s LUT texture allocation — the two MUST match.
pub const DISPLAY_LUT_SIZE: u32 = 33;

/// Gamma the LUT index grid is encoded with, concentrating nodes in the
/// shadows. Mirrored by `display.wgsl`'s `shaper_encode` — the two MUST match.
pub const DISPLAY_LUT_SHAPER_GAMMA: f32 = 2.2;

/// LUT index (`[0,1]`) → working-linear input fed to the transform.
pub fn shaper_decode(x: f32) -> f32 {
    x.clamp(0.0, 1.0).powf(DISPLAY_LUT_SHAPER_GAMMA)
}

/// Working-linear value → LUT sample coordinate (`[0,1]`). Inverse of `shaper_decode`.
pub fn shaper_encode(x: f32) -> f32 {
    x.clamp(0.0, 1.0).powf(1.0 / DISPLAY_LUT_SHAPER_GAMMA)
}

/// A parsed monitor ICC profile (matrix/TRC or cLUT/A2B, uniformly) + a
/// human-readable name for the UI.
pub struct DisplayProfile {
    pub(crate) profile: moxcms::ColorProfile,
    pub name: String,
}

impl DisplayProfile {
    /// Parse monitor ICC bytes. `Err` on malformed input (caller falls back to sRGB).
    pub fn parse(bytes: &[u8]) -> Result<DisplayProfile, ColorError> {
        let profile = moxcms::ColorProfile::new_from_slice(bytes)
            .map_err(|e| ColorError::Icc(e.to_string()))?;
        let name = profile_name(&profile).unwrap_or_else(|| "Monitor profile".to_string());
        Ok(DisplayProfile { profile, name })
    }
}

fn profile_name(p: &moxcms::ColorProfile) -> Option<String> {
    use moxcms::ProfileText;
    let s = match p.description.as_ref()? {
        ProfileText::PlainString(s) => s.clone(),
        ProfileText::Localizable(v) => v.first().map(|l| l.value.clone())?,
        ProfileText::Description(d) => {
            if !d.unicode_string.is_empty() {
                d.unicode_string.clone()
            } else if !d.ascii_string.is_empty() {
                d.ascii_string.clone()
            } else {
                d.mac_string.clone()
            }
        }
    };
    let s = s.trim().trim_end_matches('\0').trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// A baked `working→monitor` 3D LUT. `rgba16f` is `size³` RGBA half-float
/// texels, R fastest then G then B (matches wgpu `write_texture` row/layer order).
#[derive(Debug, Clone)]
pub struct DisplayLut {
    pub size: u32,
    pub rgba16f: Vec<u16>,
}

/// Build a moxcms source profile representing `working` with a LINEAR TRC, so
/// working-linear RGB can be fed straight through a profile→profile transform.
fn linear_working_profile(working: crate::WorkingSpace) -> moxcms::ColorProfile {
    use crate::WorkingSpace;
    let mut p = match working {
        WorkingSpace::Srgb => moxcms::ColorProfile::new_srgb(),
        WorkingSpace::AdobeRgb => moxcms::ColorProfile::new_adobe_rgb(),
        WorkingSpace::DisplayP3 => moxcms::ColorProfile::new_display_p3(),
        WorkingSpace::Rec2020 => moxcms::ColorProfile::new_bt2020(),
        WorkingSpace::ProPhoto => moxcms::ColorProfile::new_pro_photo_rgb(),
    };
    let lin = moxcms::curve_from_gamma(1.0);
    p.red_trc = Some(lin.clone());
    p.green_trc = Some(lin.clone());
    p.blue_trc = Some(lin);
    p.cicp = None; // don't let CICP transfer override the linear TRC
    p
}

/// Bake the `working→monitor` transform into a `size³` RGBA16F 3D LUT, indexed
/// through the gamma shaper (`shaper_decode`).
pub fn bake_display_lut(
    working: crate::WorkingSpace,
    monitor: &DisplayProfile,
    size: u32,
) -> Result<DisplayLut, ColorError> {
    use moxcms::{Layout, TransformOptions};
    let src = linear_working_profile(working);
    let opts = TransformOptions {
        allow_use_cicp_transfer: false,
        prefer_fixed_point: false,
        ..TransformOptions::default()
    };
    let xf = src
        .create_transform_f32(Layout::Rgb, &monitor.profile, Layout::Rgb, opts)
        .map_err(|e| ColorError::Icc(e.to_string()))?;

    let n = size as usize;
    let denom = (n - 1) as f32;
    // Build the input grid: working-linear values from shaper-decoded indices.
    let mut input = Vec::with_capacity(n * n * n * 3);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                input.push(shaper_decode(r as f32 / denom));
                input.push(shaper_decode(g as f32 / denom));
                input.push(shaper_decode(b as f32 / denom));
            }
        }
    }
    let mut out = vec![0.0f32; input.len()];
    xf.transform(&input, &mut out)
        .map_err(|e| ColorError::Icc(e.to_string()))?;

    // Pack to RGBA16F, clamped to [0,1], alpha = 1.
    let mut rgba16f = Vec::with_capacity(n * n * n * 4);
    for px in out.chunks_exact(3) {
        rgba16f.push(half::f16::from_f32(px[0].clamp(0.0, 1.0)).to_bits());
        rgba16f.push(half::f16::from_f32(px[1].clamp(0.0, 1.0)).to_bits());
        rgba16f.push(half::f16::from_f32(px[2].clamp(0.0, 1.0)).to_bits());
        rgba16f.push(half::f16::from_f32(1.0).to_bits());
    }
    Ok(DisplayLut { size, rgba16f })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shaper_round_trips() {
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            assert!((shaper_encode(shaper_decode(x)) - x).abs() < 1e-5, "x={x}");
        }
    }

    #[test]
    fn shaper_endpoints_are_fixed() {
        assert!((shaper_decode(0.0)).abs() < 1e-6);
        assert!((shaper_decode(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn parse_accepts_emitted_srgb_profile() {
        // Reuse the crate's own ICC emitter as a known-valid profile.
        let bytes = crate::emit_icc(crate::WorkingSpace::Srgb).expect("emit");
        let dp = DisplayProfile::parse(&bytes).expect("parse");
        assert!(!dp.name.is_empty());
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(DisplayProfile::parse(&[0u8; 8]).is_err());
    }

    #[test]
    fn bakes_lut_of_expected_shape() {
        let mon =
            DisplayProfile::parse(&crate::emit_icc(crate::WorkingSpace::Srgb).unwrap()).unwrap();
        let lut = bake_display_lut(crate::WorkingSpace::Srgb, &mon, DISPLAY_LUT_SIZE).unwrap();
        assert_eq!(lut.size, DISPLAY_LUT_SIZE);
        let n = DISPLAY_LUT_SIZE as usize;
        assert_eq!(lut.rgba16f.len(), n * n * n * 4);
        assert!(lut
            .rgba16f
            .iter()
            .all(|&h| half::f16::from_bits(h).is_finite()));
    }

    #[test]
    fn srgb_working_to_srgb_monitor_reproduces_srgb_oetf() {
        // sRGB working through an sRGB monitor profile ≈ the sRGB OETF within
        // trilinear tolerance: the LUT-encoded corners bracket a known value.
        let mon =
            DisplayProfile::parse(&crate::emit_icc(crate::WorkingSpace::Srgb).unwrap()).unwrap();
        let lut = bake_display_lut(crate::WorkingSpace::Srgb, &mon, DISPLAY_LUT_SIZE).unwrap();
        let n = DISPLAY_LUT_SIZE as usize;
        // Node at index (n-1,n-1,n-1) is working-linear (1,1,1) → sRGB ~1.0.
        let last = (n * n * n - 1) * 4;
        let white = half::f16::from_bits(lut.rgba16f[last]).to_f32();
        assert!((white - 1.0).abs() < 0.02, "white corner {white}");
        // Node at index (0,0,0) is (0,0,0) → 0.
        let black = half::f16::from_bits(lut.rgba16f[0]).to_f32();
        assert!(black.abs() < 0.02, "black corner {black}");
    }

    #[test]
    fn lut_channels_are_monotonic_along_red_axis() {
        let mon =
            DisplayProfile::parse(&crate::emit_icc(crate::WorkingSpace::Srgb).unwrap()).unwrap();
        let lut = bake_display_lut(crate::WorkingSpace::Rec2020, &mon, DISPLAY_LUT_SIZE).unwrap();
        let n = DISPLAY_LUT_SIZE as usize;
        // Walk r at g=b=0; the R output must be non-decreasing.
        let mut prev = -1.0f32;
        for r in 0..n {
            let idx = r * 4; // g=b=0 → linear index = r
            let v = half::f16::from_bits(lut.rgba16f[idx]).to_f32();
            assert!(v + 1e-3 >= prev, "non-monotonic at r={r}: {v} < {prev}");
            prev = v;
        }
    }
}
