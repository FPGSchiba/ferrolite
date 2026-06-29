use ferrolite_image::Orientation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeStatus {
    Pending,
    Done,
    Failed,
}

impl DecodeStatus {
    pub fn as_i64(self) -> i64 {
        match self {
            DecodeStatus::Pending => 0,
            DecodeStatus::Done => 1,
            DecodeStatus::Failed => 2,
        }
    }

    pub fn from_i64(v: i64) -> DecodeStatus {
        match v {
            1 => DecodeStatus::Done,
            2 => DecodeStatus::Failed,
            _ => DecodeStatus::Pending,
        }
    }
}

/// Values written when ingesting one image.
#[derive(Debug, Clone)]
pub struct NewImage {
    pub folder_id: i64,
    pub filename: String,
    pub mtime: i64,
    pub size: i64,
    pub make: Option<String>,
    pub model: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Orientation,
    pub capture_time: Option<String>,
    pub iso: Option<u32>,
    pub decode_status: DecodeStatus,
}

/// Row read back from the catalog for the grid/status bar.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRecord {
    pub id: i64,
    pub folder_id: i64,
    pub filename: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Orientation,
    pub capture_time: Option<String>,
    pub iso: Option<u32>,
    pub decode_status: DecodeStatus,
}

/// Result of an ingest pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IngestSummary {
    pub scanned: usize,
    pub added: usize,
    pub skipped: usize,
    pub failed: usize,
}
