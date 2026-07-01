//! Map a catalog row + texture availability to a render state for its grid cell.

use ferrolite_catalog::{DecodeStatus, ImageRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    Placeholder,
    Ready,
    Failed,
}

pub fn cell_state(rec: &ImageRecord, has_texture: bool) -> CellState {
    match rec.decode_status {
        DecodeStatus::Failed => CellState::Failed,
        _ if has_texture => CellState::Ready,
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
        }
    }

    #[test]
    fn failed_row_is_failed_even_without_texture() {
        assert_eq!(
            cell_state(&rec(DecodeStatus::Failed), false),
            CellState::Failed
        );
    }

    #[test]
    fn done_with_texture_is_ready() {
        assert_eq!(cell_state(&rec(DecodeStatus::Done), true), CellState::Ready);
    }

    #[test]
    fn done_without_texture_is_placeholder() {
        assert_eq!(
            cell_state(&rec(DecodeStatus::Done), false),
            CellState::Placeholder
        );
    }
}
