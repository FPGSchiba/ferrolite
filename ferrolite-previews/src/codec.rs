//! 8-bit sRGB JPEG codec for cached preview renders.
//!
//! `encode_srgb_jpeg` takes a WORKING-space linear render, maps it to display
//! (caller-supplied 3×3 matrix) and encodes (sRGB OETF) to 8-bit, downscales
//! (never upscales) to `long_edge`, and JPEG-encodes it. `decode_srgb_jpeg`
//! reverses the JPEG step only — the result is still 8-bit sRGB, ready for the
//! app to `color_convert` into whatever it needs to display.

use ferrolite_color::{mul_vec3, srgb_oetf, Mat3};
use ferrolite_image::{ImageBuffer, LinearRgbaF32, PixelFormat};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ExtendedColorType, RgbImage};

/// Errors from encoding a render to JPEG or decoding JPEG bytes back.
#[derive(Debug, thiserror::Error)]
pub enum PreviewCodecError {
    /// The source pixel buffer's length didn't match its stated dimensions.
    #[error("invalid image buffer: {0}")]
    InvalidBuffer(String),
    /// JPEG encode/decode failed (corrupt bytes, unsupported format, I/O).
    #[error("JPEG codec error: {0}")]
    Image(#[from] image::ImageError),
    /// The decoded pixel buffer didn't match the dimensions reported by the
    /// JPEG decoder (should not happen in practice; guards against a
    /// mismatched `ImageBuffer::new` construction).
    #[error("decoded buffer shape mismatch: {0}")]
    Shape(#[from] ferrolite_image::ImageBufferError),
}

/// Encode a WORKING-space linear render to an 8-bit sRGB JPEG (given
/// `quality`), downscaled so its long edge == `long_edge` (never upscaled;
/// aspect ratio preserved). `display_matrix` is `working_to_display(working_space)`;
/// the sRGB OETF is applied after it.
pub fn encode_srgb_jpeg(
    render: &LinearRgbaF32,
    display_matrix: Mat3,
    long_edge: u32,
    quality: u8,
) -> Result<Vec<u8>, PreviewCodecError> {
    let rgb8 = render_to_srgb8(render, display_matrix);
    let src_img = RgbImage::from_raw(render.width, render.height, rgb8).ok_or_else(|| {
        PreviewCodecError::InvalidBuffer(format!(
            "render pixel buffer does not match {}x{}",
            render.width, render.height
        ))
    })?;

    let (target_w, target_h) = target_dims(render.width, render.height, long_edge);
    let resized = if (target_w, target_h) == (render.width, render.height) {
        src_img
    } else {
        image::imageops::resize(&src_img, target_w, target_h, FilterType::Triangle)
    };

    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, quality).encode(
        resized.as_raw(),
        target_w,
        target_h,
        ExtendedColorType::Rgb8,
    )?;
    Ok(bytes)
}

/// Decode JPEG bytes → 8-bit sRGB [`ImageBuffer`] (`Rgb8`) for the app to
/// `color_convert`.
pub fn decode_srgb_jpeg(bytes: &[u8]) -> Result<ImageBuffer, PreviewCodecError> {
    let decoded = image::load_from_memory(bytes)?.to_rgb8();
    let (width, height) = decoded.dimensions();
    let buf = ImageBuffer::new(width, height, PixelFormat::Rgb8, decoded.into_raw())?;
    Ok(buf)
}

/// Apply `display_matrix` to each working-linear RGB triple, clamp to
/// `[0, 1]`, apply the sRGB OETF, and quantize to 8-bit. Alpha is dropped —
/// cached previews are opaque.
fn render_to_srgb8(render: &LinearRgbaF32, display_matrix: Mat3) -> Vec<u8> {
    let mut out = Vec::with_capacity(render.width as usize * render.height as usize * 3);
    for px in render.pixels.chunks_exact(4) {
        let mapped = mul_vec3(&display_matrix, &[px[0], px[1], px[2]]);
        for channel in mapped {
            let clamped = channel.clamp(0.0, 1.0);
            let encoded = srgb_oetf(clamped);
            out.push((encoded * 255.0).round().clamp(0.0, 255.0) as u8);
        }
    }
    out
}

/// Compute output dimensions for `long_edge` scaling: the longer of
/// `(width, height)` becomes `long_edge`, aspect preserved, but never
/// upscaled (dimensions are left unchanged if already `<= long_edge`).
fn target_dims(width: u32, height: u32, long_edge: u32) -> (u32, u32) {
    let src_long = width.max(height);
    if src_long == 0 || src_long <= long_edge {
        return (width, height);
    }
    let scale = long_edge as f32 / src_long as f32;
    if width >= height {
        (long_edge, ((height as f32 * scale).round() as u32).max(1))
    } else {
        (((width as f32 * scale).round() as u32).max(1), long_edge)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_color::identity;

    /// A solid-color linear render of `width`×`height`, RGB = `linear`, opaque.
    fn solid_render(width: u32, height: u32, linear: f32) -> LinearRgbaF32 {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..(width as usize * height as usize) {
            pixels.extend_from_slice(&[linear, linear, linear, 1.0]);
        }
        LinearRgbaF32::new(width, height, pixels).expect("valid buffer")
    }

    #[test]
    fn encode_downscales_long_edge() {
        let render = solid_render(4096, 2048, 0.0);
        let bytes = encode_srgb_jpeg(&render, identity(), 2048, 90).expect("encode succeeds");
        let decoded = decode_srgb_jpeg(&bytes).expect("decode succeeds");
        assert_eq!(decoded.width, 2048);
        assert_eq!(decoded.height, 1024);
    }

    #[test]
    fn encode_never_upscales() {
        let render = solid_render(512, 256, 0.0);
        let bytes = encode_srgb_jpeg(&render, identity(), 2048, 90).expect("encode succeeds");
        let decoded = decode_srgb_jpeg(&bytes).expect("decode succeeds");
        assert_eq!(decoded.width, 512);
        assert_eq!(decoded.height, 256);
    }

    #[test]
    fn roundtrip_is_color_close() {
        let linear = 0.18_f32; // 18% mid-gray, working-linear.
        let render = solid_render(16, 16, linear);
        let bytes = encode_srgb_jpeg(&render, identity(), 2048, 90).expect("encode succeeds");
        let decoded = decode_srgb_jpeg(&bytes).expect("decode succeeds");

        let expected = (srgb_oetf(linear) * 255.0).round() as i32;
        for channel in decoded.pixels.iter() {
            let actual = i32::from(*channel);
            assert!(
                (actual - expected).abs() <= 2,
                "expected {expected} +/- 2, got {actual}"
            );
        }
    }

    #[test]
    fn decode_rejects_garbage() {
        let result = decode_srgb_jpeg(&[0, 1, 2]);
        assert!(result.is_err());
    }
}
