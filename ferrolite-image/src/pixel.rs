//! Pixel format + a validated interleaved-8-bit image buffer. Zero deps so this
//! vocabulary stays liftable into the engine-transferable tier.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb8,
    Rgba8,
}

impl PixelFormat {
    pub fn channels(self) -> usize {
        match self {
            PixelFormat::Rgb8 => 3,
            PixelFormat::Rgba8 => 4,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ImageBufferError {
    pub expected: usize,
    pub actual: usize,
}

impl fmt::Display for ImageBufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pixel buffer length {} does not match expected {}",
            self.actual, self.expected
        )
    }
}

impl std::error::Error for ImageBufferError {}

/// Interleaved 8-bit-per-channel image. `pixels.len() == width*height*channels`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBuffer {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub pixels: Vec<u8>,
}

impl ImageBuffer {
    pub fn expected_len(width: u32, height: u32, format: PixelFormat) -> usize {
        width as usize * height as usize * format.channels()
    }

    pub fn new(
        width: u32,
        height: u32,
        format: PixelFormat,
        pixels: Vec<u8>,
    ) -> Result<Self, ImageBufferError> {
        let expected = Self::expected_len(width, height, format);
        if pixels.len() != expected {
            return Err(ImageBufferError {
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            format,
            pixels,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_match_format() {
        assert_eq!(PixelFormat::Rgb8.channels(), 3);
        assert_eq!(PixelFormat::Rgba8.channels(), 4);
    }

    #[test]
    fn expected_len_multiplies_dimensions_and_channels() {
        assert_eq!(ImageBuffer::expected_len(4, 2, PixelFormat::Rgb8), 24);
        assert_eq!(ImageBuffer::expected_len(4, 2, PixelFormat::Rgba8), 32);
    }

    #[test]
    fn new_accepts_correct_length() {
        let buf = ImageBuffer::new(2, 1, PixelFormat::Rgb8, vec![0; 6]).unwrap();
        assert_eq!(buf.width, 2);
        assert_eq!(buf.pixels.len(), 6);
    }

    #[test]
    fn new_rejects_wrong_length() {
        let err = ImageBuffer::new(2, 1, PixelFormat::Rgb8, vec![0; 5]).unwrap_err();
        assert_eq!(err.expected, 6);
        assert_eq!(err.actual, 5);
    }
}
