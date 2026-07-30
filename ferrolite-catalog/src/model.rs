use ferrolite_image::{Color, FileKind, Flag, Orientation, Rating, TagId};
use std::path::PathBuf;

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
    pub lens: Option<String>,
    pub aperture: Option<f32>,
    pub focal_length: Option<f32>,
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
    /// Cache of "has a non-identity frl:ops stack" (rebuildable from the sidecar).
    pub has_edits: bool,
    /// The persisted thumbnail's own pixel dimensions (`thumbnails.w`/`h`),
    /// joined in from the `thumbnails` table. Already display-upright at BOTH
    /// ingest time and after an edited-thumbnail regen (`thumb_regen.rs`
    /// renders through the full `EditPipeline`, including crop/geometry, then
    /// `generate_thumbnail` resizes that upright output preserving its
    /// aspect) — unlike `width`/`height` above, which are the ingest-time
    /// SENSOR-space dims (pre-orientation-swap) and never change after a
    /// crop. This is the source of truth for the grid/filmstrip cell aspect
    /// ratio (`library::grid::cell_aspect`). `None` when no thumbnail row
    /// exists yet (e.g. a `Pending` row not yet reached by ingest).
    pub thumb_w: Option<u32>,
    pub thumb_h: Option<u32>,
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
    pub parent_id: Option<i64>,
}

/// A row of the persisted export queue (spec §8.4). Ordered by `position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportQueueEntry {
    pub image_id: i64,
    pub position: i64,
    pub added_at: i64,
}

/// Result of an ingest pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IngestSummary {
    pub scanned: usize,
    pub added: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// One row awaiting the Task-14 background EXIF metadata backfill: an image
/// whose `lens`/`aperture`/`focal_length` are all still NULL (either
/// ingested before the v7 migration added those columns, or not yet reached
/// by a backfill pass). `path` is the already-joined folder-path + filename
/// (see `images_needing_metadata_backfill`'s doc comment), so the backfill
/// job never needs a separate `folder_path` round-trip per row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillCandidate {
    pub id: i64,
    pub path: PathBuf,
    pub kind: FileKind,
}

/// One image's resolved Task-14 backfill write, ready for
/// `Catalog::apply_metadata_backfill_batch`. `lens = Some(String::new())` is
/// the "attempted, found nothing" sentinel (see that method's doc comment) —
/// it is written back literally (never coerced to `None`), which is what
/// permanently excludes the row from `images_needing_metadata_backfill`
/// instead of retrying it on every launch.
#[derive(Debug, Clone, PartialEq)]
pub struct BackfillResult {
    pub id: i64,
    pub lens: Option<String>,
    pub aperture: Option<f32>,
    pub focal_length: Option<f32>,
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
            lens: meta.lens.clone(),
            aperture: meta.aperture,
            focal_length: meta.focal_length,
            decode_status: DecodeStatus::Done,
            kind,
            rating,
            added_at,
        }
    }

    /// Build a stat-only `Pending` row (no file read yet). Used by the instant
    /// index pass so every filename appears in the grid immediately; a later
    /// metadata pass upgrades it to `Done` (or `Failed`).
    pub fn pending(
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
            lens: None,
            aperture: None,
            focal_length: None,
            decode_status: DecodeStatus::Pending,
            kind,
            rating: Rating::default(),
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
            lens: None,
            aperture: None,
            focal_length: None,
            decode_status: DecodeStatus::Failed,
            kind,
            rating: Rating::default(),
            added_at,
        }
    }
}
