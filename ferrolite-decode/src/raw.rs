use crate::error::{rawler as rawler_err, DecodeError};
use rawler::decoders::RawDecodeParams;
use rawler::rawimage::RawImageData;
use rawler::rawsource::RawSource;
use std::path::Path;

/// A fully decoded RAW: integer CFA/sensor samples plus geometry. Consumed by
/// the VT/viewer in a later plan; here it only proves rawler decodes the file.
#[derive(Debug, Clone)]
pub struct RawDecoded {
    pub width: u32,
    pub height: u32,
    /// Components per pixel (1 for Bayer CFA, 3/4 for some formats).
    pub cpp: usize,
    /// Sensor samples, length `width * height * cpp`.
    pub pixels: Vec<u16>,
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
        RawImageData::Float(v) => v
            .iter()
            .map(|f| f.round().clamp(0.0, 65535.0) as u16)
            .collect(),
    };

    Ok(RawDecoded {
        width: u32::try_from(img.width)
            .map_err(|_| DecodeError::Rawler("RAW width exceeds u32".into()))?,
        height: u32::try_from(img.height)
            .map_err(|_| DecodeError::Rawler("RAW height exceeds u32".into()))?,
        cpp: img.cpp,
        pixels,
    })
}
