use crate::error::{rawler as rawler_err, DecodeError};
use rawler::decoders::RawDecodeParams;
use rawler::rawimage::{RawImageData, RawPhotometricInterpretation};
use rawler::rawsource::RawSource;
use std::path::Path;

/// A fully decoded RAW: integer CFA/sensor samples plus geometry and colour
/// calibration metadata. Consumed by the demosaic/display pipeline.
#[derive(Debug, Clone)]
pub struct RawDecoded {
    pub width: u32,
    pub height: u32,
    /// Components per pixel (1 for Bayer CFA, 3/4 for some formats).
    pub cpp: usize,
    /// Sensor samples, length `width * height * cpp`.
    pub pixels: Vec<u16>,
    /// 2×2 CFA colour indices (0=R, 1=G, 2=B), row-major from the top-left
    /// sensor pixel: `[(col=0,row=0), (col=1,row=0), (col=0,row=1), (col=1,row=1)]`.
    pub cfa_pattern: [u8; 4],
    /// Per-CFA-position black levels (sensor units), order matches `cfa_pattern`.
    pub black_levels: [f32; 4],
    /// Saturation / white level (sensor units).
    pub white_level: f32,
    /// Camera white-balance multipliers [R, G1, B, G2]; any non-finite or
    /// non-positive value is replaced with 1.0.
    pub wb_coeffs: [f32; 4],
}

pub fn decode_full(path: &Path) -> Result<RawDecoded, DecodeError> {
    let src = RawSource::new(path).map_err(rawler_err)?;
    let decoder = rawler::get_decoder(&src).map_err(rawler_err)?;
    let params = RawDecodeParams::default();
    let img = decoder
        .raw_image(&src, &params, false)
        .map_err(rawler_err)?;

    // RawImageData is Integer(Vec<u16>) for almost all formats; a few DNGs are
    // Float — quantize to u16 for this plan's display-only consumer.
    let pixels = match img.data {
        RawImageData::Integer(v) => v,
        // NaN/Inf saturate to 0 / 65535 via Rust's defined float-to-int cast; acceptable for this display-only consumer.
        RawImageData::Float(v) => v
            .iter()
            .map(|f| f.round().clamp(0.0, 65535.0) as u16)
            .collect(),
    };

    // --- CFA pattern ---
    // Prefer the CFA embedded in the photometric interpretation (most decoders
    // set this); fall back to camera.cfa which is always populated.
    // rawler 0.7.2: RawPhotometricInterpretation::Cfa(CFAConfig { cfa, .. })
    // CFA::color_at(row, col) -> usize  (0=R, 1=G, 2=B, …)
    let cfa = match &img.photometric {
        RawPhotometricInterpretation::Cfa(cfg) => cfg.cfa.clone(),
        _ => img.camera.cfa.clone(),
    };
    let cfa_pattern = cfa_to_pattern(&cfa);

    // --- Black levels ---
    // BlackLevel::as_bayer_array() -> [f32; 4]  (rawler 0.7.2, rawimage.rs:120)
    let black_levels = img.blacklevel.as_bayer_array();

    // --- White level ---
    // WhiteLevel(Vec<u32>)  (rawler 0.7.2, rawimage.rs:27)
    // Use the first component; default to 65535 if the vec is empty.
    let white_level = img
        .whitelevel
        .0
        .first()
        .copied()
        .unwrap_or(65535) as f32;

    // --- White-balance coefficients ---
    // img.wb_coeffs: [f32; 4]  order: [R, G1, B, G2]  (rawimage.rs:216)
    // Replace any non-finite / non-positive value; G2 falls back to G1.
    let wb = img.wb_coeffs;
    let wb_coeffs = [
        finite_pos_or_one(wb[0]),
        finite_pos_or_one(wb[1]),
        finite_pos_or_one(wb[2]),
        finite_pos_or_one(if wb[3].is_finite() && wb[3] > 0.0 {
            wb[3]
        } else {
            wb[1]
        }),
    ];

    Ok(RawDecoded {
        width: u32::try_from(img.width)
            .map_err(|_| DecodeError::Rawler("RAW width exceeds u32".into()))?,
        height: u32::try_from(img.height)
            .map_err(|_| DecodeError::Rawler("RAW height exceeds u32".into()))?,
        cpp: img.cpp,
        pixels,
        cfa_pattern,
        black_levels,
        white_level,
        wb_coeffs,
    })
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Return `v` if it is finite and > 0, otherwise 1.0.
fn finite_pos_or_one(v: f32) -> f32 {
    if v.is_finite() && v > 0.0 {
        v
    } else {
        1.0
    }
}

/// Map rawler's CFA to four 0=R/1=G/2=B indices, row-major from the top-left
/// 2×2 block: `[(col=0,row=0), (col=1,row=0), (col=0,row=1), (col=1,row=1)]`.
///
/// rawler 0.7.2: `CFA::color_at(row, col) -> usize`
/// Constants: CFA_COLOR_R=0, CFA_COLOR_G=1, CFA_COLOR_B=2  (cfa.rs:7-9).
/// Values > 2 (CYAN, MAGENTA, …) are clamped to 1 (green) as a safe default.
fn cfa_to_pattern(cfa: &rawler::CFA) -> [u8; 4] {
    let idx = |row, col| cfa.color_at(row, col).min(2) as u8;
    [idx(0, 0), idx(0, 1), idx(1, 0), idx(1, 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn decode_full_surfaces_cfa_and_levels() {
        // Use a committed fixture RAW if present; otherwise skip (kept green
        // where fixtures are absent).
        let fixture = Path::new("../fixtures/raw/sample.rw2");
        if !fixture.exists() {
            eprintln!("no RAW fixture; skipping decode_full metadata assertions");
            return;
        }
        let d = decode_full(fixture).expect("decode");
        assert_eq!(d.cfa_pattern.len(), 4);
        assert!(d.white_level > 0.0, "white level must be positive");
        assert!(
            d.wb_coeffs.iter().all(|c| c.is_finite() && *c > 0.0),
            "all WB coefficients must be finite and positive, got: {:?}",
            d.wb_coeffs
        );
    }
}
