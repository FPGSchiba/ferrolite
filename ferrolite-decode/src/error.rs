use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("rawler error: {0}")]
    Rawler(String),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("no embedded preview, full image, or thumbnail in {0}")]
    NoPreview(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("exif error: {0}")]
    Exif(String),
}

/// rawler's error type implements `Display`; we flatten it to a string so this
/// crate does not re-export rawler's error in its public API.
pub(crate) fn rawler<E: std::fmt::Display>(e: E) -> DecodeError {
    DecodeError::Rawler(e.to_string())
}
