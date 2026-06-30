#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("decode error: {0}")]
    Decode(#[from] ferrolite_decode::DecodeError),
    #[error("thumbnail encode error: {0}")]
    Encode(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("conflict: {0}")]
    Conflict(String),
}
