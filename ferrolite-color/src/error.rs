//! Error type for the color crate.

/// Errors from ICC emit/parse. Pure color math (matrices, adaptation) is
/// infallible and returns values directly.
#[derive(Debug, thiserror::Error)]
pub enum ColorError {
    #[error("ICC profile error: {0}")]
    Icc(String),
}
