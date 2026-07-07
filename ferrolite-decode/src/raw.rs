use crate::color::ColorProfile;
use crate::error::{rawler as rawler_err, DecodeError};
use ferrolite_image::Orientation;
use rawler::decoders::RawDecodeParams;
use rawler::rawimage::{RawImageData, RawPhotometricInterpretation};
use rawler::rawsource::RawSource;
use std::path::Path;

/// A fully decoded RAW cropped to the camera's recommended image rectangle
/// (`crop_area`, else `active_area`, else the full sensor): integer CFA/sensor
/// samples plus geometry and colour calibration metadata. `width`/`height` are
/// the CROPPED dimensions; `cfa_pattern` and `black_levels` are phase-aligned to
/// the crop origin. Consumed by the demosaic/display pipeline.
#[derive(Debug, Clone)]
pub struct RawDecoded {
    /// Cropped width (see struct docs) — NOT the full sensor width.
    pub width: u32,
    /// Cropped height (see struct docs) — NOT the full sensor height.
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
    /// Camera color calibration (XYZ→camera matrix + reference white). Additive
    /// decode product; consumed by `ferrolite-pipeline` via `ferrolite-color`.
    pub color_profile: ColorProfile,
    /// EXIF orientation of the sensor frame. The demosaic output is sensor-
    /// native; the consumer applies this to upright the full-res tier so it
    /// matches the already-uprighted embedded preview.
    pub orientation: Orientation,
}

/// Decode a RAW file, cropping the sensor buffer to the camera's recommended
/// image rectangle (`img.crop_area`, else `img.active_area`, else no crop) so
/// the masked/optically-black sensor border is excluded. The returned
/// `width`/`height` are the CROPPED dimensions, and `cfa_pattern`/`black_levels`
/// are phase-shifted to the crop origin (a no-op at an even/even origin).
pub fn decode_full(path: &Path) -> Result<RawDecoded, DecodeError> {
    let src = RawSource::new(path).map_err(rawler_err)?;
    let decoder = rawler::get_decoder(&src).map_err(rawler_err)?;
    let params = RawDecodeParams::default();
    let img = decoder
        .raw_image(&src, &params, false)
        .map_err(rawler_err)?;

    // EXIF orientation of the sensor frame (RAW pixels are stored sensor-native).
    // Read cheaply from metadata; default to Normal when absent/unreadable.
    let orientation = decoder
        .raw_metadata(&src, &params)
        .ok()
        .and_then(|m| m.exif.orientation)
        .map(Orientation::from_exif)
        .unwrap_or(Orientation::Normal);

    // Crop to the camera's recommended image rectangle so the sensor's masked /
    // optically-black border is excluded (otherwise the tiled renderer edge-
    // replicates it into a stretched seam). Prefer `crop_area` (the intended
    // final image, matching the embedded preview); fall back to `active_area`
    // (optically-black-excluded); else no crop. `Rect { p: Point{x,y}, d: Dim2{w,h} }`
    // is in sensor-buffer pixel coords (pre-orientation).
    let full_w = img.width;
    let full_h = img.height;
    let crop = img
        .crop_area
        .or(img.active_area)
        .filter(|r| r.p.x + r.d.w <= full_w && r.p.y + r.d.h <= full_h)
        .filter(|r| !(r.p.x == 0 && r.p.y == 0 && r.d.w == full_w && r.d.h == full_h));

    // RawImageData is Integer(Vec<u16>) for almost all formats; a few DNGs are
    // Float — quantize to u16 for this plan's display-only consumer.
    let full_pixels = match img.data {
        RawImageData::Integer(v) => v,
        // NaN/Inf saturate to 0 / 65535 via Rust's defined float-to-int cast; acceptable for this display-only consumer.
        RawImageData::Float(v) => v
            .iter()
            .map(|f| f.round().clamp(0.0, 65535.0) as u16)
            .collect(),
    };
    let (pixels, width, height, crop_origin) = match crop {
        Some(r) => (
            crop_sensor_buffer(
                &full_pixels,
                full_w,
                full_h,
                img.cpp,
                r.p.x,
                r.p.y,
                r.d.w,
                r.d.h,
            ),
            r.d.w,
            r.d.h,
            (r.p.x, r.p.y),
        ),
        None => (full_pixels, full_w, full_h, (0, 0)),
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
    // Cropping can move the top-left into a different Bayer phase; shift the
    // pattern to the crop origin so it describes the cropped buffer's (0,0).
    let cfa = cfa.shift(crop_origin.0, crop_origin.1);
    let cfa_pattern = cfa_to_pattern(&cfa);

    // --- Black levels ---
    // BlackLevel::as_bayer_array() -> [f32; 4]  (rawler 0.7.2, rawimage.rs:120)
    let black_levels = permute_black_levels_by_origin(
        img.blacklevel.as_bayer_array(),
        crop_origin.0,
        crop_origin.1,
    );

    // --- White level ---
    // WhiteLevel(Vec<u32>)  (rawler 0.7.2, rawimage.rs:27)
    // Use the first component; default to 65535 if the vec is empty.
    let white_level = img.whitelevel.0.first().copied().unwrap_or(65535) as f32;

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
        width: u32::try_from(width)
            .map_err(|_| DecodeError::Rawler("RAW width exceeds u32".into()))?,
        height: u32::try_from(height)
            .map_err(|_| DecodeError::Rawler("RAW height exceeds u32".into()))?,
        cpp: img.cpp,
        pixels,
        cfa_pattern,
        black_levels,
        white_level,
        wb_coeffs,
        color_profile: ColorProfile::from_color_matrix(&img.color_matrix),
        orientation,
    })
}

/// The camera [`ColorProfile`] from rawler metadata WITHOUT a pixel decode
/// (dummy `raw_image`), so a preview-cache key can be built at open time
/// without paying for demosaic. Must equal `decode_full(path).color_profile`
/// — the same `img.color_matrix` field feeds both (verified by the
/// `decode_color_profile_matches_full` test).
pub fn decode_color_profile(path: &Path) -> Result<ColorProfile, DecodeError> {
    let src = RawSource::new(path).map_err(rawler_err)?;
    let decoder = rawler::get_decoder(&src).map_err(rawler_err)?;
    let params = RawDecodeParams::default();
    // `dummy = true`: geometry/metadata only, NO pixel decode (no demosaic).
    let img = decoder.raw_image(&src, &params, true).map_err(rawler_err)?;
    Ok(ColorProfile::from_color_matrix(&img.color_matrix))
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

/// Copy the `cw`×`ch` sub-rectangle whose top-left is sensor pixel `(cx, cy)`
/// out of a row-major `full_w`×`full_h` buffer with `cpp` components per pixel.
/// The caller guarantees `cx + cw <= full_w` and `cy + ch <= full_h`.
#[allow(clippy::too_many_arguments)]
fn crop_sensor_buffer(
    pixels: &[u16],
    full_w: usize,
    full_h: usize,
    cpp: usize,
    cx: usize,
    cy: usize,
    cw: usize,
    ch: usize,
) -> Vec<u16> {
    debug_assert!(
        cx + cw <= full_w && cy + ch <= full_h,
        "crop rect out of bounds"
    );
    debug_assert_eq!(
        pixels.len(),
        full_w * full_h * cpp,
        "pixel buffer size mismatch"
    );
    let mut out = Vec::with_capacity(cw * ch * cpp);
    for row in 0..ch {
        let src_y = cy + row;
        let row_start = (src_y * full_w + cx) * cpp;
        out.extend_from_slice(&pixels[row_start..row_start + cw * cpp]);
    }
    out
}

/// Reorder a 2×2 per-position black-level array (indexed `row*2 + col`) so it
/// matches the CFA phase after cropping at sensor origin `(cx, cy)`. Cropped
/// position `(r, c)` reads sensor position `((cy + r) % 2, (cx + c) % 2)`.
/// Identity when `cx` and `cy` are both even.
fn permute_black_levels_by_origin(bl: [f32; 4], cx: usize, cy: usize) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for r in 0..2 {
        for c in 0..2 {
            let src = (((cy + r) % 2) * 2) + ((cx + c) % 2);
            out[r * 2 + c] = bl[src];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn crop_sensor_buffer_extracts_subrect_cpp1() {
        // 4x3 single-component buffer, values = y*10 + x.
        let full_w = 4usize;
        let full_h = 3usize;
        let px: Vec<u16> = (0..full_h)
            .flat_map(|y| (0..full_w).map(move |x| (y * 10 + x) as u16))
            .collect();
        // Crop the 2x2 starting at (1,1): expect [11,12, 21,22].
        let out = crop_sensor_buffer(&px, full_w, full_h, 1, 1, 1, 2, 2);
        assert_eq!(out, vec![11, 12, 21, 22]);
    }

    #[test]
    fn crop_sensor_buffer_respects_cpp() {
        // 2x2, cpp=2: pixel (x,y) -> [y*10+x, 100+y*10+x].
        let (full_w, full_h, cpp) = (2usize, 2usize, 2usize);
        let mut px = Vec::new();
        for y in 0..full_h {
            for x in 0..full_w {
                px.push((y * 10 + x) as u16);
                px.push((100 + y * 10 + x) as u16);
            }
        }
        // Crop the 1x2 column starting at (1,0): pixels (1,0) and (1,1).
        let out = crop_sensor_buffer(&px, full_w, full_h, cpp, 1, 0, 1, 2);
        assert_eq!(out, vec![1, 101, 11, 111]);
    }

    #[test]
    fn black_levels_permute_is_identity_for_even_origin() {
        let bl = [1.0, 2.0, 3.0, 4.0]; // [(0,0),(0,1),(1,0),(1,1)]
        assert_eq!(permute_black_levels_by_origin(bl, 8, 6), bl);
        assert_eq!(permute_black_levels_by_origin(bl, 0, 0), bl);
    }

    #[test]
    fn black_levels_permute_shifts_phase_for_odd_origin() {
        // bl indexed r*2+c: (0,0)=1 (0,1)=2 (1,0)=3 (1,1)=4.
        // Odd x (cx=1), even y (cy=0): cropped (r,c) -> sensor ((0+r)%2,(1+c)%2).
        //   (0,0)->(0,1)=2  (0,1)->(0,0)=1  (1,0)->(1,1)=4  (1,1)->(1,0)=3
        assert_eq!(
            permute_black_levels_by_origin([1.0, 2.0, 3.0, 4.0], 1, 0),
            [2.0, 1.0, 4.0, 3.0]
        );
        // Odd x and odd y (cx=1,cy=1): cropped (r,c) -> sensor ((1+r)%2,(1+c)%2).
        //   (0,0)->(1,1)=4  (0,1)->(1,0)=3  (1,0)->(0,1)=2  (1,1)->(0,0)=1
        assert_eq!(
            permute_black_levels_by_origin([1.0, 2.0, 3.0, 4.0], 1, 1),
            [4.0, 3.0, 2.0, 1.0]
        );
    }

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
        // Color profile is always present (real matrix or sRGB fallback), finite.
        assert!(
            d.color_profile
                .xyz_to_cam
                .iter()
                .flatten()
                .all(|v| v.is_finite()),
            "color profile matrix must be finite"
        );
        assert!(d.color_profile.white_xy.iter().all(|v| *v > 0.0));
    }

    /// LINCHPIN: the cheap dummy-decode profile must equal the full-decode
    /// profile. If rawler ever returned different color matrices for a dummy
    /// vs a real `raw_image`, every preview-cache read would build a key with a
    /// different `color_profile_hash` than the write path used, so every read
    /// would miss every write. This test is the guard for that invariant.
    #[test]
    fn decode_color_profile_matches_full() {
        let fixture = Path::new("../fixtures/raw/sample.rw2");
        if !fixture.exists() {
            eprintln!("no RAW fixture; skipping decode_color_profile equivalence");
            return;
        }
        let cheap = decode_color_profile(fixture).expect("cheap profile decode");
        let full = decode_full(fixture).expect("full decode").color_profile;
        assert_eq!(
            cheap, full,
            "dummy-decode color profile must equal the full-decode color profile"
        );
    }

    /// The active-area crop must shrink the decoded frame to the camera's
    /// recommended rectangle (removing the masked/optically-black border that
    /// otherwise seams at the right/bottom edge in the tiled renderer). For the
    /// bundled RW2 (`crop_area` origin (8,6), even/even) the crop preserves the
    /// Bayer phase, so `cfa_pattern` and `black_levels` are unchanged.
    #[test]
    fn decode_full_crops_to_active_area() {
        let fixture = Path::new("../fixtures/raw/sample.rw2");
        if !fixture.exists() {
            eprintln!("no RAW fixture; skipping active-area crop assertion");
            return;
        }
        let d = decode_full(fixture).expect("decode");
        // Cropped dims (NOT the full 4060x2250 sensor).
        assert_eq!(
            (d.width, d.height),
            (3968, 2232),
            "decoded to crop_area dims"
        );
        // Pixel buffer length matches cropped dims * cpp.
        assert_eq!(
            d.pixels.len(),
            (d.width as usize) * (d.height as usize) * d.cpp
        );
        // Even/even origin -> phase preserved; every black level finite.
        assert!(d.black_levels.iter().all(|b| b.is_finite()));
        assert!(d.white_level > 0.0);
    }
}
