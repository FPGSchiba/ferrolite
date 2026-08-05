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
    #[error("jpeg error: {0}")]
    Jpeg(String),
    /// The underlying decoder **panicked** and the unwind was caught.
    ///
    /// `rawler` asserts internally on some containers it cannot really handle
    /// (confirmed: Canon sRAW1/mRAW trips `assertion failed: self.initialized`
    /// in `pixarray.rs`). Because every decode runs inside a `ferrolite-jobs`
    /// worker, letting that unwind escape would kill the worker mid-ingest, so
    /// the RAW entry points catch it and report this instead. A file yielding
    /// this error is undecodable *today*, not necessarily malformed — see
    /// `fixtures/raw-broken/README.md`.
    #[error("decoder panicked while reading {0}")]
    DecoderPanicked(PathBuf),
}

/// rawler's error type implements `Display`; we flatten it to a string so this
/// crate does not re-export rawler's error in its public API.
pub(crate) fn rawler<E: std::fmt::Display>(e: E) -> DecodeError {
    DecodeError::Rawler(e.to_string())
}
