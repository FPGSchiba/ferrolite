//! Filled in Task 6.

#[derive(Debug, Clone)]
pub enum PixelData {
    Eight(Vec<u8>),
    Sixteen(Vec<u16>),
}

#[derive(Debug, Clone)]
pub struct RenderedImage {
    pub width: u32,
    pub height: u32,
    pub data: PixelData,
}
