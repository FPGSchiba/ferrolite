//! Map a catalog row + texture availability to a render state for its grid cell.

use ferrolite_catalog::{DecodeStatus, ImageRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    Placeholder,
    /// No texture yet, but an ingest is currently active — the thumbnail is
    /// (most likely) still being generated, so the cell should show a distinct
    /// "working on it" affordance rather than the flat idle placeholder.
    Generating,
    Ready,
    Failed,
}

/// `is_ingesting` should be `state.active_ingests > 0` — the grid has no other
/// reliable per-image signal that a thumbnail is actively being produced
/// (ingest thumbnails are generated inline in the ingest job, not tracked in
/// `thumb_pending`).
pub fn cell_state(rec: &ImageRecord, has_texture: bool, is_ingesting: bool) -> CellState {
    match rec.decode_status {
        DecodeStatus::Failed => CellState::Failed,
        _ if has_texture => CellState::Ready,
        _ if is_ingesting => CellState::Generating,
        _ => CellState::Placeholder,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_catalog::{FileKind, Flag, Rating};
    use ferrolite_image::Orientation;

    fn rec(status: DecodeStatus) -> ImageRecord {
        ImageRecord {
            id: 1,
            folder_id: 1,
            filename: "x.nef".into(),
            width: Some(100),
            height: Some(100),
            orientation: Orientation::Normal,
            capture_time: None,
            iso: None,
            decode_status: status,
            kind: FileKind::Raw,
            rating: Rating::default(),
            flag: Flag::None,
            has_edits: false,
            thumb_w: None,
            thumb_h: None,
        }
    }

    #[test]
    fn failed_row_is_failed_even_without_texture() {
        assert_eq!(
            cell_state(&rec(DecodeStatus::Failed), false, false),
            CellState::Failed
        );
    }

    #[test]
    fn failed_row_is_failed_even_while_ingesting() {
        assert_eq!(
            cell_state(&rec(DecodeStatus::Failed), false, true),
            CellState::Failed
        );
    }

    #[test]
    fn textured_row_is_ready_regardless_of_ingest_state() {
        assert_eq!(
            cell_state(&rec(DecodeStatus::Done), true, false),
            CellState::Ready
        );
        assert_eq!(
            cell_state(&rec(DecodeStatus::Done), true, true),
            CellState::Ready
        );
    }

    #[test]
    fn untextured_row_is_generating_while_ingesting() {
        assert_eq!(
            cell_state(&rec(DecodeStatus::Done), false, true),
            CellState::Generating
        );
    }

    #[test]
    fn untextured_row_is_placeholder_when_idle() {
        assert_eq!(
            cell_state(&rec(DecodeStatus::Done), false, false),
            CellState::Placeholder
        );
    }
}
