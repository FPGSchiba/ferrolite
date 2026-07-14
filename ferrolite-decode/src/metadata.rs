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
    /// Focal length reported by the camera in 35 mm-equivalent terms (EXIF
    /// `FocalLengthIn35mmFilm`, 0xA405). An integer SHORT tag, `None` when
    /// absent. Only the standard/EXIF route populates this; the RAW route
    /// leaves it `None` (rawler does not expose the tag).
    pub focal_length_35mm: Option<u32>,
    pub capture_time: Option<String>,
    pub lens: Option<String>,
}
