//! Metadata edit commands: optimistic in-memory apply + an off-thread persist job
//! (SQLite for rating/flag/tags, plus the xmp:Rating sidecar for rating).

use crate::events::AppEvent;
use ferrolite_catalog::{Catalog, ImageRecord};
use ferrolite_image::{Flag, Rating, TagId};
use ferrolite_jobs::{JobSystem, Priority};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaEdit {
    SetRating(Rating),
    SetFlag(Flag),
    ToggleTag(TagId),
}

/// Apply an edit to the in-memory grid row + its cached tag list (optimistic UI).
pub fn apply_edit_in_memory(rec: &mut ImageRecord, visible_tags: &mut Vec<TagId>, edit: MetaEdit) {
    match edit {
        MetaEdit::SetRating(r) => rec.rating = r,
        MetaEdit::SetFlag(f) => rec.flag = f,
        MetaEdit::ToggleTag(t) => {
            if let Some(pos) = visible_tags.iter().position(|x| *x == t) {
                visible_tags.remove(pos);
            } else {
                visible_tags.push(t);
            }
        }
    }
}

/// Persist an edit to all `image_paths` off the UI thread. Writes SQLite for
/// every axis; writes the xmp:Rating sidecar for `SetRating`. Emits a
/// `MetadataResult` (a sidecar failure is a warning, not a revert).
pub fn spawn_metadata_write(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    edit: MetaEdit,
    image_paths: Vec<(i64, PathBuf)>,
) {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Visible, move |_cancel| {
        let mut warning: Option<String> = None;
        let mut ok = true;
        for (image_id, path) in &image_paths {
            let db = writer.lock().expect("writer");
            let db_res = match edit {
                MetaEdit::SetRating(r) => db.set_rating(*image_id, r),
                MetaEdit::SetFlag(f) => db.set_flag(*image_id, f),
                MetaEdit::ToggleTag(t) => db.toggle_tag(*image_id, t),
            };
            if let Err(e) = db_res {
                ok = false;
                warning = Some(format!("catalog write failed: {e}"));
                continue;
            }
            drop(db);
            if let MetaEdit::SetRating(r) = edit {
                let xmp = ferrolite_catalog::sidecar_path(path);
                if let Err(e) = ferrolite_catalog::write_rating(&xmp, r) {
                    warning = Some(format!("sidecar write failed: {e}"));
                }
            }
        }
        let _ = tx.send(AppEvent::MetadataResult { ok, warning });
        ctx.request_repaint();
    });
}

/// Returns `0` if `current == pressed` (toggle off), otherwise `pressed`.
pub fn toggle_rating(current: u8, pressed: u8) -> u8 {
    if current == pressed {
        0
    } else {
        pressed
    }
}

/// Returns `Flag::None` if `current == pressed` (toggle off), otherwise `pressed`.
pub fn toggle_flag(current: Flag, pressed: Flag) -> Flag {
    if current == pressed {
        Flag::None
    } else {
        pressed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_catalog::FileKind;
    use ferrolite_image::Orientation;

    fn rec() -> ImageRecord {
        ImageRecord {
            id: 1,
            folder_id: 1,
            filename: "x.nef".into(),
            width: None,
            height: None,
            orientation: Orientation::Normal,
            capture_time: None,
            iso: None,
            decode_status: ferrolite_catalog::DecodeStatus::Done,
            kind: FileKind::Raw,
            rating: Rating::default(),
            flag: Flag::None,
        }
    }

    #[test]
    fn set_rating_and_flag_update_record() {
        let mut r = rec();
        let mut tags = vec![];
        apply_edit_in_memory(&mut r, &mut tags, MetaEdit::SetRating(Rating::new(4)));
        assert_eq!(r.rating, Rating::new(4));
        apply_edit_in_memory(&mut r, &mut tags, MetaEdit::SetFlag(Flag::Pick));
        assert_eq!(r.flag, Flag::Pick);
    }

    #[test]
    fn toggle_tag_adds_then_removes() {
        let mut r = rec();
        let mut tags = vec![];
        apply_edit_in_memory(&mut r, &mut tags, MetaEdit::ToggleTag(TagId(5)));
        assert_eq!(tags, vec![TagId(5)]);
        apply_edit_in_memory(&mut r, &mut tags, MetaEdit::ToggleTag(TagId(5)));
        assert!(tags.is_empty());
    }

    // --- toggle_rating ---

    #[test]
    fn toggle_rating_sets_when_different() {
        assert_eq!(toggle_rating(0, 3), 3);
    }

    #[test]
    fn toggle_rating_clears_when_same() {
        assert_eq!(toggle_rating(3, 3), 0);
    }

    #[test]
    fn toggle_rating_changes_value() {
        assert_eq!(toggle_rating(2, 5), 5);
    }

    // --- toggle_flag ---

    #[test]
    fn toggle_flag_sets_pick_from_none() {
        assert_eq!(toggle_flag(Flag::None, Flag::Pick), Flag::Pick);
    }

    #[test]
    fn toggle_flag_clears_pick_when_already_pick() {
        assert_eq!(toggle_flag(Flag::Pick, Flag::Pick), Flag::None);
    }

    #[test]
    fn toggle_flag_changes_reject_to_pick() {
        assert_eq!(toggle_flag(Flag::Reject, Flag::Pick), Flag::Pick);
    }

    #[test]
    fn toggle_flag_clears_reject_when_already_reject() {
        assert_eq!(toggle_flag(Flag::Reject, Flag::Reject), Flag::None);
    }
}
