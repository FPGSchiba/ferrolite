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
        let cancelled = cancel.is_cancelled();
        let _ = tx.send(AppEvent::BatchApplyDone {
            result,
            snapshot,
            label,
            cancelled,
        });
        ctx.request_repaint();
    });
}

/// Decode a snapshot back into `(image_id, path, prior document)` triples.
/// Entries that no longer deserialize are dropped — an undo that can restore
/// most of a batch is better than one that panics.
pub fn snapshot_documents(snap: &UndoSnapshot) -> Vec<(i64, PathBuf, EditDoc)> {
    snap.entries
        .iter()
        .filter_map(|(id, path, text)| {
            ferrolite_pipeline::deserialize(text).map(|doc| (*id, path.clone(), doc))
        })
        .collect()
}

/// Restore a batch's prior documents. Writes each sidecar back and re-flags the
/// thumbnails stale (they were regenerated, or marked, against the now-undone
/// edit either way).
pub fn spawn_batch_undo(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    ctx: &egui::Context,
    snapshot: UndoSnapshot,
) {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Background, move |_cancel| {
        let docs = snapshot_documents(&snapshot);
        let mut result = BatchResult::default();
        let mut ids = Vec::with_capacity(docs.len());
        for (image_id, path, doc) in &docs {
            let xmp = ferrolite_catalog::sidecar_path(path);
            let payload = ferrolite_pipeline::serialize(doc);
            if ferrolite_catalog::write_ops(&xmp, &payload).is_err() {
                result.failed += 1;
                continue;
            }
            let db = writer.lock().expect("writer");
            let _ = db.set_has_edits(*image_id, !doc.is_identity());
            result.applied += 1;
            ids.push(*image_id);
        }
        {
            let db = writer.lock().expect("writer");
            let _ = db.set_thumbnails_stale(&ids, true);
        }
        let _ = tx.send(AppEvent::BatchApplyDone {
            result,
            snapshot: None, // undoing an undo is not offered
            label: "Undo".to_string(),
            cancelled: false, // the undo job itself is not cancellable
        });
        ctx.request_repaint();
    });
}

/// Build the batch-apply toast's level + message. Pure and unit-testable
/// without egui.
///
/// `cancelled` is the reason this is a separate function rather than inline
/// formatting at the call site: when a batch is cut short by its cancel
/// token, `apply_patch_to_targets` folds every remaining, unattempted target
/// into `result.skipped` (see that function's read closure) — the exact same
/// counter an unreadable/corrupt sidecar increments. Without `cancelled`, a
/// user who cancels a 500-image batch after 47 would see "47 applied, 453
/// skipped", which reads as 453 corrupt files rather than "you clicked
/// Cancel". `undo_hint`, when `Some`, is the live keybind text
/// (`Keymap::hint(Action::Undo)`) appended as a call to action; `None` when
/// no snapshot was retained (batch exceeded `BATCH_UNDO_MAX` or nothing was
/// applied).
pub fn batch_result_message(
    result: &BatchResult,
    label: &str,
    cancelled: bool,
    undo_hint: Option<&str>,
) -> (crate::notifications::Level, String) {
    use crate::notifications::Level;

    let (level, mut msg) = if cancelled {
        let level = if result.failed > 0 {
            Level::Warning
        } else {
            Level::Info
        };
        let mut msg = format!(
            "Cancelled \u{2014} applied to {} of {} images.",
            result.applied,
            result.total()
        );
        if result.failed > 0 {
            msg = format!("{msg} {} failed.", result.failed);
        }
        (level, msg)
    } else if result.failed == 0 && result.skipped == 0 {
        (
            Level::Info,
            format!(
                "Applied \u{201c}{label}\u{201d} to {} images.",
                result.applied
            ),
        )
    } else {
        (
            Level::Warning,
            format!(
                "Applied \u{201c}{label}\u{201d} to {} images. {} failed, {} skipped.",
                result.applied, result.failed, result.skipped
            ),
        )
    };

    if let Some(hint) = undo_hint {
        msg = format!("{msg} Press {hint} to undo.");
    }
    (level, msg)
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

    /// Undo restores each snapshot entry's prior document verbatim.
    #[test]
    fn undo_restores_the_exact_prior_documents() {
        let mut prior = EditDoc::default();
        prior.global.exposure = -1.25;
        prior.global.saturation = 0.4;
        let snap = UndoSnapshot {
            entries: vec![(
                7,
                std::path::PathBuf::from("/img/7.arw"),
                ferrolite_pipeline::serialize(&prior),
            )],
        };

        let restored = snapshot_documents(&snap);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].0, 7);
        assert_eq!(
            restored[0].2, prior,
            "prior document restored byte-for-byte"
        );
    }

    /// A snapshot entry that no longer deserializes is dropped, not panicked on.
    #[test]
    fn undo_drops_an_unparseable_snapshot_entry() {
        let snap = UndoSnapshot {
            entries: vec![(1, std::path::PathBuf::from("/a"), "garbage {{".into())],
        };
        assert!(snapshot_documents(&snap).is_empty());
    }

    /// A clean run (no failures, no skips, not cancelled) gets a plain
    /// success toast at Info, with the undo hint appended when offered.
    #[test]
    fn message_reports_full_success_with_undo_hint() {
        let result = BatchResult {
            applied: 5,
            failed: 0,
            skipped: 0,
        };
        let (level, msg) = batch_result_message(&result, "Warm portrait", false, Some("Ctrl+Z"));
        assert_eq!(level, crate::notifications::Level::Info);
        assert_eq!(
            msg,
            "Applied \u{201c}Warm portrait\u{201d} to 5 images. Press Ctrl+Z to undo."
        );
    }

    /// Partial failure/skip (NOT cancelled) reports both counts and steps up
    /// to Warning; with no snapshot retained, no undo hint is appended.
    #[test]
    fn message_reports_partial_failure_at_warning_without_undo_hint() {
        let result = BatchResult {
            applied: 3,
            failed: 1,
            skipped: 1,
        };
        let (level, msg) = batch_result_message(&result, "Warm portrait", false, None);
        assert_eq!(level, crate::notifications::Level::Warning);
        assert_eq!(
            msg,
            "Applied \u{201c}Warm portrait\u{201d} to 3 images. 1 failed, 1 skipped."
        );
    }

    /// A cancelled run must be phrased as a cancellation, not as "N skipped"
    /// (which would read as N corrupt sidecars) — the Task 4 review finding
    /// this function exists to fix.
    #[test]
    fn message_reports_cancellation_not_as_skips() {
        let result = BatchResult {
            applied: 47,
            failed: 0,
            skipped: 453,
        };
        let (level, msg) = batch_result_message(&result, "Warm portrait", true, Some("Ctrl+Z"));
        assert_eq!(level, crate::notifications::Level::Info);
        assert_eq!(
            msg,
            "Cancelled \u{2014} applied to 47 of 500 images. Press Ctrl+Z to undo."
        );
        assert!(
            !msg.contains("skipped"),
            "cancellation must never be phrased in terms of skipped count"
        );
    }

    /// A cancelled run that also had real write failures still surfaces them
    /// (distinct from the targets left unattempted by the cancel) and steps
    /// up to Warning.
    #[test]
    fn message_reports_cancellation_with_failures_at_warning() {
        let result = BatchResult {
            applied: 40,
            failed: 2,
            skipped: 458,
        };
        let (level, msg) = batch_result_message(&result, "Warm portrait", true, None);
        assert_eq!(level, crate::notifications::Level::Warning);
        assert_eq!(
            msg,
            "Cancelled \u{2014} applied to 40 of 500 images. 2 failed."
        );
    }
}
