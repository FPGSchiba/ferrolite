//! Display-linear RGBA f32 image — the CPU-side product of demosaic and the
//! input to the VT LOD pyramid. f32 on the CPU; converted to f16 at GPU upload.

use crate::ImageBufferError;

#[derive(Debug, Clone, PartialEq)]
pub struct LinearRgbaF32 {
    pub width: u32,
    pub height: u32,
    /// Interleaved RGBA, length `width*height*4`, linear `[0,1]` (not gamma-encoded).
    pub pixels: Vec<f32>,
}

impl LinearRgbaF32 {
    pub fn expected_len(width: u32, height: u32) -> usize {
        width as usize * height as usize * 4
    }

    pub fn new(width: u32, height: u32, pixels: Vec<f32>) -> Result<Self, ImageBufferError> {
        let expected = Self::expected_len(width, height);
        if pixels.len() != expected {
            return Err(ImageBufferError {
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Opaque black image (RGB 0, A 1).
    pub fn black(width: u32, height: u32) -> Self {
        let mut pixels = vec![0.0f32; Self::expected_len(width, height)];
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 1.0;
        }
        Self {
            width,
            height,
            pixels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_len_is_four_channels() {
        assert_eq!(LinearRgbaF32::expected_len(4, 2), 32);
    }

    #[test]
    fn new_accepts_correct_length() {
        let img = LinearRgbaF32::new(2, 1, vec![0.0; 8]).unwrap();
        assert_eq!(img.width, 2);
        assert_eq!(img.pixels.len(), 8);
    }

    #[test]
    fn new_rejects_wrong_length() {
        let err = LinearRgbaF32::new(2, 1, vec![0.0; 7]).unwrap_err();
        assert_eq!(err.expected, 8);
        assert_eq!(err.actual, 7);
    }

    #[test]
    fn black_is_opaque_zero_rgb() {
        let img = LinearRgbaF32::black(1, 1);
        assert_eq!(img.pixels, vec![0.0, 0.0, 0.0, 1.0]);
    }
}
