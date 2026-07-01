//! Export error type. Never panics — every failure is a variant surfaced to the
//! UI as a status-bar warning (spec §10).

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("no GPU adapter available for export")]
    NoGpu,
    #[error("export cancelled")]
    Cancelled,
    #[error("render failed: {0}")]
    Render(String),
    #[error("encode failed: {0}")]
    Encode(String),
    #[error("write failed: {0}")]
    Io(String),
}
