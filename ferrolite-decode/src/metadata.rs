use ferrolite_image::Orientation;

/// Camera/exposure metadata read cheaply from a RAW (no full pixel decode).
#[derive(Debug, Clone, PartialEq)]
pub struct Metadata {
    pub make: String,
    pub model: String,
    pub width: u32,
    pub height: u32,
    pub orientation: Orientation,
    pub iso: Option<u32>,
    pub aperture: Option<f32>,
    pub shutter: Option<f32>,
    pub focal_length: Option<f32>,
    pub capture_time: Option<String>,
    pub lens: Option<String>,
}
