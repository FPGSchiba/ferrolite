//! Batch application of an `EditPatch` to N images (P7 design §5).
//!
//! **One job for the whole batch, not one-at-a-time.** Batch EXPORT processes
//! items sequentially because each is a full-res render plus a CPU-heavy encode
//! and running several saturated the machine (see `export/batch.rs`'s module
//! doc). Batch EDIT does no rendering at all — N sidecar writes is milliseconds
//! of I/O — so that constraint does not transfer and is not inherited.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ferrolite_catalog::Catalog;
use ferrolite_jobs::{JobSystem, Priority};
use ferrolite_pipeline::{EditDoc, EditPatch};

use crate::events::AppEvent;

/// Beyond this many targets no undo snapshot is retained. A serialized
/// `EditDoc` runs 0.5–2 KB, so 2,000 snapshots costs a few MB — comfortably
/// above any realistic batch, while ruling out a 50,000-image select-all
/// pinning ~100 MB for the session. The apply dialog warns BEFORE committing
/// when the target count exceeds this. Single tuning constant: change only
/// this value.
pub const BATCH_UNDO_MAX: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchTarget {
    pub image_id: i64,
    pub path: PathBuf,
}

/// Outcome counts. `skipped` (could not read the current document) and
/// `failed` (could not write) are distinct and reported separately.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatchResult {
    pub applied: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl BatchResult {
    pub fn total(&self) -> usize {
        self.applied + self.failed + self.skipped
    }
}

/// Prior documents captured for undo: `(image_id, path, serialized prior doc)`.
/// Serialized rather than held as `EditDoc` so the memory cost is the flat JSON
/// the cap is reasoned about in.
#[derive(Clone, Debug, Default)]
pub struct UndoSnapshot {
    pub entries: Vec<(i64, PathBuf, String)>,
}

/// Merge `patch` into every target and write the result.
///
/// Parameterized over `read` and `write` so the whole decision surface — counts,
/// snapshot, progress, partial failure — is testable without a filesystem.
/// `write` returns `Err(reason)` on failure; the batch continues regardless.
pub fn apply_patch_to_targets(
    patch: &EditPatch,
    targets: &[BatchTarget],
    read: impl Fn(&BatchTarget) -> Option<EditDoc>,
    write: impl Fn(&BatchTarget, &EditDoc) -> Result<(), String>,
    progress: &mut dyn FnMut(usize, usize),
) -> (BatchResult, UndoSnapshot) {
    let total = targets.len();
    let snapshot_wanted = total <= BATCH_UNDO_MAX;
    let mut result = BatchResult::default();
    let mut snapshot = UndoSnapshot::default();

    for (i, t) in targets.iter().enumerate() {
        match read(t) {
            None => result.skipped += 1,
            Some(prior) => {
                let merged = patch.apply_to(&prior);
                match write(t, &merged) {
                    Ok(()) => {
                        result.applied += 1;
                        if snapshot_wanted {
                            snapshot.entries.push((
                                t.image_id,
                                t.path.clone(),
                                ferrolite_pipeline::serialize(&prior),
                            ));
                        }
                    }
                    Err(_reason) => result.failed += 1,
                }
            }
        }
        progress(i + 1, total);
    }
    (result, snapshot)
}

/// Submit the batch as ONE Background job (contract 1: priority, cancellation,
/// progress). Reads and writes each target's sidecar, flags the affected
/// thumbnails stale, and reports through `AppEvent::BatchApplyDone`.
pub fn spawn_batch_apply(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    ctx: &egui::Context,
    patch: EditPatch,
    targets: Vec<BatchTarget>,
    label: String,
) {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Background, move |cancel| {
        let mut last_repaint = 0usize;
        let tx_progress = tx.clone();
        let ctx_progress = ctx.clone();
        let mut progress = |done: usize, total: usize| {
            let _ = tx_progress.send(AppEvent::BatchApplyProgress { done, total });
            // Throttle repaints like the export path: every 16 items and on
            // completion, so progress advances without flooding the UI thread.
            if done == total || done.saturating_sub(last_repaint) >= 16 {
                last_repaint = done;
                ctx_progress.request_repaint();
            }
        };

        let (result, snapshot) = apply_patch_to_targets(
            &patch,
            &targets,
            |t| {
                if cancel.is_cancelled() {
                    return None;
                }
                let xmp = ferrolite_catalog::sidecar_path(&t.path);
                match ferrolite_catalog::read_ops(&xmp) {
                    Some(text) => ferrolite_pipeline::deserialize(&text),
                    // No sidecar yet == an unedited image, which is a perfectly
                    // valid target: start from the default document.
                    None if !xmp.exists() => Some(EditDoc::default()),
                    None => None, // present but malformed → skip, do not clobber
                }
            },
            |t, doc| {
                let xmp = ferrolite_catalog::sidecar_path(&t.path);
                let payload = ferrolite_pipeline::serialize(doc);
                ferrolite_catalog::write_ops(&xmp, &payload).map_err(|e| e.to_string())?;
                let db = writer.lock().expect("writer");
                db.set_has_edits(t.image_id, !doc.is_identity())
                    .map_err(|e| e.to_string())?;
                Ok(())
            },
            &mut progress,
        );

        // Flag every touched thumbnail stale in one statement (design §5.2).
        let ids: Vec<i64> = targets.iter().map(|t| t.image_id).collect();
        {
            let db = writer.lock().expect("writer");
            let _ = db.set_thumbnails_stale(&ids, true);
        }

        let snapshot = (!snapshot.entries.is_empty()).then_some(snapshot);
        let _ = tx.send(AppEvent::BatchApplyDone {
            result,
            snapshot,
            label,
        });
        ctx.request_repaint();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{EditDoc, EditPatch, GroupSet};
    use std::collections::HashMap;

    fn target(id: i64) -> BatchTarget {
        BatchTarget {
            image_id: id,
            path: std::path::PathBuf::from(format!("/img/{id}.arw")),
        }
    }

    /// Applies the patch to every target, returns accurate counts, and captures
    /// each target's PRIOR document for undo.
    #[test]
    fn applies_to_all_targets_and_snapshots_prior_docs() {
        let mut store: HashMap<i64, EditDoc> = HashMap::new();
        for id in 1..=3 {
            let mut d = EditDoc::default();
            d.global.exposure = id as f32;
            store.insert(id, d);
        }
        let written: std::cell::RefCell<HashMap<i64, EditDoc>> =
            std::cell::RefCell::new(HashMap::new());

        let mut source = EditDoc::default();
        source.global.exposure = 9.0;
        let patch = EditPatch::from_doc(&source, GroupSet::LIGHT);

        let (result, snap) = apply_patch_to_targets(
            &patch,
            &[target(1), target(2), target(3)],
            |t| store.get(&t.image_id).cloned(),
            |t, doc| {
                written.borrow_mut().insert(t.image_id, doc.clone());
                Ok(())
            },
            &mut |_done, _total| {},
        );

        assert_eq!(result.applied, 3);
        assert_eq!(result.failed, 0);
        assert_eq!(result.skipped, 0);
        for id in 1..=3 {
            assert_eq!(written.borrow()[&id].global.exposure, 9.0);
        }
        assert_eq!(
            snap.entries.len(),
            3,
            "one snapshot entry per applied target"
        );
    }

    /// A write failure is counted, does NOT abort the batch, and the failed
    /// target contributes no undo entry (there is nothing to roll back).
    #[test]
    fn a_failed_write_is_counted_and_does_not_abort_the_batch() {
        let mut store: HashMap<i64, EditDoc> = HashMap::new();
        for id in 1..=3 {
            store.insert(id, EditDoc::default());
        }
        let patch = EditPatch::from_doc(&EditDoc::default(), GroupSet::LIGHT);

        let (result, snap) = apply_patch_to_targets(
            &patch,
            &[target(1), target(2), target(3)],
            |t| store.get(&t.image_id).cloned(),
            |t, _doc| {
                if t.image_id == 2 {
                    Err("read-only".to_string())
                } else {
                    Ok(())
                }
            },
            &mut |_d, _t| {},
        );

        assert_eq!(result.applied, 2);
        assert_eq!(result.failed, 1);
        assert_eq!(snap.entries.len(), 2, "no undo entry for the failed write");
    }

    /// A target whose current document cannot be read is SKIPPED, not failed —
    /// they are different outcomes and the toast reports them separately.
    #[test]
    fn an_unreadable_target_is_skipped_not_failed() {
        let patch = EditPatch::from_doc(&EditDoc::default(), GroupSet::LIGHT);
        let (result, snap) = apply_patch_to_targets(
            &patch,
            &[target(1)],
            |_t| None,
            |_t, _doc| Ok(()),
            &mut |_d, _t| {},
        );
        assert_eq!(result.skipped, 1);
        assert_eq!(result.applied, 0);
        assert_eq!(result.failed, 0);
        assert!(snap.entries.is_empty());
    }

    /// Past BATCH_UNDO_MAX no snapshot is taken — the dialog warns up front.
    #[test]
    fn no_snapshot_is_taken_beyond_the_undo_cap() {
        let targets: Vec<BatchTarget> = (1..=(BATCH_UNDO_MAX as i64 + 1)).map(target).collect();
        let patch = EditPatch::from_doc(&EditDoc::default(), GroupSet::LIGHT);
        let (result, snap) = apply_patch_to_targets(
            &patch,
            &targets,
            |_t| Some(EditDoc::default()),
            |_t, _doc| Ok(()),
            &mut |_d, _t| {},
        );
        assert_eq!(result.applied, targets.len());
        assert!(
            snap.entries.is_empty(),
            "over the cap, no snapshot is retained"
        );
    }

    #[test]
    fn progress_is_reported_for_every_target() {
        let patch = EditPatch::from_doc(&EditDoc::default(), GroupSet::LIGHT);
        let seen = std::cell::RefCell::new(Vec::new());
        let _ = apply_patch_to_targets(
            &patch,
            &[target(1), target(2)],
            |_t| Some(EditDoc::default()),
            |_t, _doc| Ok(()),
            &mut |done, total| seen.borrow_mut().push((done, total)),
        );
        assert_eq!(*seen.borrow(), vec![(1, 2), (2, 2)]);
    }
}
