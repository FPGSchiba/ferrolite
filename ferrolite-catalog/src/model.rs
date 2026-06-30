use ferrolite_image::{Color, FileKind, Flag, Orientation, Rating, TagId};

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
    pub kind: FileKind,
    pub rating: Rating,
    pub added_at: i64,
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
    pub kind: FileKind,
    pub rating: Rating,
    pub flag: Flag,
}

/// A tag row read back from the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRecord {
    pub id: TagId,
    pub name: String,
    pub color: Color,
}

/// A collection row read back from the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionRecord {
    pub id: i64,
    pub name: String,
    pub color: Color,
    pub sort_order: i64,
}

/// Result of an ingest pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IngestSummary {
    pub scanned: usize,
    pub added: usize,
    pub skipped: usize,
    pub failed: usize,
}

impl NewImage {
    /// Build a `Done` row from decoded metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn from_metadata(
        folder_id: i64,
        filename: String,
        mtime: i64,
        size: i64,
        meta: &ferrolite_decode::Metadata,
        kind: FileKind,
        rating: Rating,
        added_at: i64,
    ) -> Self {
        Self {
            folder_id,
            filename,
            mtime,
            size,
            make: Some(meta.make.clone()),
            model: Some(meta.model.clone()),
            width: Some(meta.width),
            height: Some(meta.height),
            orientation: meta.orientation,
            capture_time: meta.capture_time.clone(),
            iso: meta.iso,
            decode_status: DecodeStatus::Done,
            kind,
            rating,
            added_at,
        }
    }

    /// Build a `Failed` placeholder row (decode failed; grid shows a broken cell).
    pub fn failed(
        folder_id: i64,
        filename: String,
        mtime: i64,
        size: i64,
        kind: FileKind,
        added_at: i64,
    ) -> Self {
        Self {
            folder_id,
            filename,
            mtime,
            size,
            make: None,
            model: None,
            width: None,
            height: None,
            orientation: Orientation::Normal,
            capture_time: None,
            iso: None,
            decode_status: DecodeStatus::Failed,
            kind,
            rating: Rating::default(),
            added_at,
        }
    }
}
