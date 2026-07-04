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
}
