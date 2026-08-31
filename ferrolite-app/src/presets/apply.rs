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

/// Outcome counts. `skipped` (could not read the current document), `failed`
/// (could not write), and `unchanged` (read fine, but the patch merged onto
/// the prior document byte-for-byte identically, so nothing was written) are
/// three distinct outcomes and reported separately. `unchanged` is
/// deliberately its own counter rather than folded into `skipped` — the two
/// mean different things ("could not read" vs. "read fine, no-op") and an
/// earlier review on this branch already flagged conflating them as a
/// mistake.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatchResult {
    pub applied: usize,
    pub failed: usize,
    pub skipped: usize,
    pub unchanged: usize,
}

impl BatchResult {
    pub fn total(&self) -> usize {
        self.applied + self.failed + self.skipped + self.unchanged
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
///
/// Returns the ids that were actually WRITTEN (i.e. `result.applied`'s
/// members, in order) alongside the result/snapshot — F5 (whole-branch
/// review): callers must flag only these thumbnails stale, not every target,
/// or a cancelled/partial batch queues skipped/failed/never-attempted images
/// for a pointless full decode + GPU render + encode on next browse.
pub fn apply_patch_to_targets(
    patch: &EditPatch,
    targets: &[BatchTarget],
    read: impl Fn(&BatchTarget) -> Option<EditDoc>,
    write: impl Fn(&BatchTarget, &EditDoc) -> Result<(), String>,
    progress: &mut dyn FnMut(usize, usize),
) -> (BatchResult, UndoSnapshot, Vec<i64>) {
    let total = targets.len();
    let snapshot_wanted = total <= BATCH_UNDO_MAX;
    let mut result = BatchResult::default();
    let mut snapshot = UndoSnapshot::default();
    let mut applied_ids = Vec::new();

    for (i, t) in targets.iter().enumerate() {
        match read(t) {
            None => result.skipped += 1,
            Some(prior) => {
                let merged = patch.apply_to(&prior);
                if merged == prior {
                    // No-op: the patch's groups already matched what this
                    // target had (e.g. "paste only Tone curve" onto a target
                    // that, like the source, has no curve). Writing an
                    // identical document would still flag the thumbnail
                    // stale and burn a full decode + GPU render + encode to
                    // produce a byte-identical image — this is the bug
                    // report's "pasting appears to do nothing": there WAS no
                    // change, so nothing should be written, applied, or
                    // snapshotted for undo.
                    result.unchanged += 1;
                } else {
                    match write(t, &merged) {
                        Ok(()) => {
                            result.applied += 1;
                            applied_ids.push(t.image_id);
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
        }
        progress(i + 1, total);
    }
    (result, snapshot, applied_ids)
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

        let (result, snapshot, applied_ids) = apply_patch_to_targets(
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

        // Flag only the images that were ACTUALLY WRITTEN stale (design §5.2)
        // — F5 (whole-branch review): flagging every target, including
        // skipped/failed/never-attempted-after-cancel ones, queues a
        // pointless full decode + GPU render + encode for images whose
        // thumbnail never changed.
        {
            let db = writer.lock().expect("writer");
            let _ = db.set_thumbnails_stale(&applied_ids, true);
        }

        let snapshot = (!snapshot.entries.is_empty()).then_some(snapshot);
        let cancelled =
            batch_was_genuinely_cancelled(cancel.is_cancelled(), &result, targets.len());
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

/// How many of `snap`'s entries `snapshot_documents` will drop (no longer
/// deserializable). Extracted as its own pure function so `spawn_batch_undo`
/// folding this count into `BatchResult.failed` — an image
/// `snapshot_documents` could not restore is a failure to revert it, not a
/// silent no-op — has a unit test independent of the job system.
fn undo_snapshot_decode_failures(snap: &UndoSnapshot) -> usize {
    snap.entries.len() - snapshot_documents(snap).len()
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
        let decode_failures = undo_snapshot_decode_failures(&snapshot);
        let docs = snapshot_documents(&snapshot);
        // Entries `snapshot_documents` silently dropped (no longer
        // deserializable) are images this undo could NOT restore — a
        // failure to revert, not a no-op — so fold them into `failed` up
        // front. Without this, the toast would report success ("Reverted…
        // on 5 images") while quietly leaving some images on their
        // post-batch edit, contradicting "restore most of a batch" with a
        // claim of restoring all of it.
        let mut result = BatchResult {
            failed: decode_failures,
            ..BatchResult::default()
        };
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
        // A DISTINCT event from `BatchApplyDone` (see that variant's doc
        // comment): undo never carries a snapshot of its own (undoing an
        // undo is not offered) and must never risk clobbering
        // `AppState.batch_undo` if a newer, unrelated batch apply's
        // `BatchApplyDone` raced this completion.
        let _ = tx.send(AppEvent::BatchUndoDone { result });
        ctx.request_repaint();
    });
}

/// Whether a batch run should be reported as CANCELLED, as opposed to an
/// ordinary completed run whose cancel token merely happened to be signalled
/// (e.g. a cancel button click racing the very last item's completion).
///
/// `apply_patch_to_targets` always visits every target regardless of
/// cancellation (a cancelled read simply returns `None` from the read
/// closure, counted as `skipped` — see `spawn_batch_apply`'s read closure),
/// so `cancel.is_cancelled()` alone can be `true` even when every target was
/// actually attempted. Reporting THAT as "Cancelled — applied to 500 of 500
/// images." would be exactly the kind of misleading phrasing `cancelled`
/// exists to prevent in the first place. Only a run that left at least one
/// target genuinely unattempted (`applied + failed < target_count`, i.e.
/// some remainder landed in `skipped` for lack of being attempted) reads as
/// cancelled. Pure so this arithmetic — easy to get subtly wrong — has a
/// unit test independent of the job system and its `CancelToken`.
fn batch_was_genuinely_cancelled(
    cancel_flag: bool,
    result: &BatchResult,
    target_count: usize,
) -> bool {
    cancel_flag && result.applied + result.failed < target_count
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

    // An all-unchanged batch: every target was read fine but the patch
    // merged onto it identically (`apply_patch_to_targets`'s `merged ==
    // prior` short-circuit), so nothing was written and there is nothing to
    // undo. Reported as its own case, ahead of the ordinary success branch
    // below, which would otherwise read "Applied \u{201c}X\u{201d} to 0
    // images." — technically accurate but indistinguishable from a silent
    // failure. This is the exact report that motivated this function: the
    // author saw no visible thumbnail change and filed it as a bug, when in
    // fact the paste was a correct no-op. A genuinely cancelled run can never
    // land here (`batch_was_genuinely_cancelled` requires at least one
    // target left unattempted, which is counted as `skipped`, not
    // `unchanged`), but `!cancelled` is kept explicit rather than relied on.
    if !cancelled
        && result.applied == 0
        && result.failed == 0
        && result.skipped == 0
        && result.unchanged > 0
    {
        return (
            Level::Info,
            format!(
                "Applied \u{201c}{label}\u{201d} \u{2014} no changes ({} images already matched).",
                result.unchanged
            ),
        );
    }

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
    } else if result.total() > BATCH_UNDO_MAX {
        // F4 (whole-branch review): the paste modal warns BEFORE the user
        // commits a batch over `BATCH_UNDO_MAX` (design §5.4), but preset-
        // apply has no such dialog (P7-D3) — this toast is the only place
        // the user is ever told undo is unavailable, so it must say so
        // explicitly rather than just quietly omitting the undo hint.
        msg = format!("{msg} Undo is unavailable for batches over {BATCH_UNDO_MAX} images.");
    }
    (level, msg)
}

/// Build the batch-UNDO toast's level + message. Deliberately a separate
/// function from `batch_result_message`, not a call to it with a
/// `label: "Undo"`: the `label` slot elsewhere always names a PRESET
/// (`Applied "Warm portrait" to 5 images.`), so reusing it for the revert
/// would render as `Applied "Undo" to 5 images.` — Undo is not a preset
/// name. `result.failed` here already includes both write failures AND any
/// snapshot entries `snapshot_documents` could not deserialize (folded in by
/// `spawn_batch_undo` before this is called), so a non-zero `failed` alone
/// is enough to step up to `Warning`.
pub fn batch_undo_message(result: &BatchResult) -> (crate::notifications::Level, String) {
    use crate::notifications::Level;

    if result.failed == 0 {
        (
            Level::Info,
            format!(
                "Reverted the last batch apply on {} images.",
                result.applied
            ),
        )
    } else {
        (
            Level::Warning,
            format!(
                "Reverted the last batch apply on {} images. {} failed.",
                result.applied, result.failed
            ),
        )
    }
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

        let (result, snap, applied_ids) = apply_patch_to_targets(
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
        assert_eq!(
            applied_ids,
            vec![1, 2, 3],
            "F5: applied_ids must list exactly the ids that were written"
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
        // Must actually CHANGE the target (not an identity patch onto a
        // default document) — otherwise the `merged == prior` no-op guard
        // would short-circuit every target as `unchanged` before `write` is
        // ever called, and this test would no longer exercise a write
        // failure at all.
        let mut source = EditDoc::default();
        source.global.exposure = 9.0;
        let patch = EditPatch::from_doc(&source, GroupSet::LIGHT);

        let (result, snap, applied_ids) = apply_patch_to_targets(
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
        assert_eq!(
            applied_ids,
            vec![1, 3],
            "F5: the failed target (2) must not appear in applied_ids"
        );
    }

    /// A target whose current document cannot be read is SKIPPED, not failed —
    /// they are different outcomes and the toast reports them separately.
    #[test]
    fn an_unreadable_target_is_skipped_not_failed() {
        let patch = EditPatch::from_doc(&EditDoc::default(), GroupSet::LIGHT);
        let (result, snap, applied_ids) = apply_patch_to_targets(
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
        assert!(
            applied_ids.is_empty(),
            "F5: a skipped (unreadable) target must not appear in applied_ids"
        );
    }

    /// An all-unchanged batch (every target's document already equals what
    /// the patch would merge in) must write NOTHING, apply nothing, and
    /// snapshot nothing. This is the actual bug: before the `merged == prior`
    /// guard, a byte-identical write still flagged the thumbnail stale and
    /// burned a full decode + GPU render + encode per image to produce a
    /// pixel-identical thumbnail, which the author correctly could not see
    /// change and filed as "pasting doesn't refresh thumbnails".
    #[test]
    fn an_all_unchanged_batch_writes_nothing_and_is_counted_as_unchanged() {
        let patch = EditPatch::from_doc(&EditDoc::default(), GroupSet::LIGHT);
        let write_calls = std::cell::RefCell::new(0usize);

        let (result, snap, applied_ids) = apply_patch_to_targets(
            &patch,
            &[target(1), target(2), target(3)],
            |_t| Some(EditDoc::default()), // already matches the identity patch
            |_t, _doc| {
                *write_calls.borrow_mut() += 1;
                Ok(())
            },
            &mut |_d, _t| {},
        );

        assert_eq!(result.unchanged, 3);
        assert_eq!(result.applied, 0);
        assert_eq!(result.failed, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(
            *write_calls.borrow(),
            0,
            "an unchanged target must never be written"
        );
        assert!(
            applied_ids.is_empty(),
            "an unchanged target must not appear in applied_ids"
        );
        assert!(
            snap.entries.is_empty(),
            "an unchanged target gets no undo entry — there is nothing to revert"
        );
    }

    /// A single batch that hits every outcome bucket — skipped, failed,
    /// unchanged, applied — must count each one correctly and keep them
    /// distinct. In particular `unchanged` (read fine, no-op) must never be
    /// folded into `skipped` (could not read) — an earlier review on this
    /// branch already flagged conflating the two as a mistake.
    #[test]
    fn a_mixed_batch_counts_every_bucket_correctly() {
        let mut source = EditDoc::default();
        source.global.exposure = 9.0;
        let patch = EditPatch::from_doc(&source, GroupSet::LIGHT);

        let mut already_matching = EditDoc::default();
        already_matching.global.exposure = 9.0;

        let (result, snap, applied_ids) = apply_patch_to_targets(
            &patch,
            &[target(1), target(2), target(3), target(4)],
            move |t| match t.image_id {
                1 => None,                           // unreadable -> skipped
                2 => Some(EditDoc::default()),       // read fine, write fails -> failed
                3 => Some(already_matching.clone()), // already matches -> unchanged
                4 => Some(EditDoc::default()),       // differs -> applied
                _ => unreachable!(),
            },
            |t, _doc| {
                if t.image_id == 2 {
                    Err("read-only".to_string())
                } else {
                    Ok(())
                }
            },
            &mut |_d, _t| {},
        );

        assert_eq!(result.skipped, 1, "target 1 was unreadable");
        assert_eq!(result.failed, 1, "target 2's write failed");
        assert_eq!(result.unchanged, 1, "target 3 already matched");
        assert_eq!(result.applied, 1, "target 4 differed and was written");
        assert_eq!(
            applied_ids,
            vec![4],
            "only the genuinely-applied target appears in applied_ids"
        );
        assert_eq!(
            snap.entries.len(),
            1,
            "only the applied target gets an undo entry"
        );
    }

    /// Past BATCH_UNDO_MAX no snapshot is taken — the dialog warns up front.
    #[test]
    fn no_snapshot_is_taken_beyond_the_undo_cap() {
        let targets: Vec<BatchTarget> = (1..=(BATCH_UNDO_MAX as i64 + 1)).map(target).collect();
        // Non-identity patch (see the comment in
        // `a_failed_write_is_counted_and_does_not_abort_the_batch`) — this
        // test's `result.applied == targets.len()` assertion requires every
        // target to actually be WRITTEN, which the `merged == prior` no-op
        // guard would otherwise short-circuit for a default-onto-default
        // identity patch.
        let mut source = EditDoc::default();
        source.global.exposure = 9.0;
        let patch = EditPatch::from_doc(&source, GroupSet::LIGHT);
        let (result, snap, applied_ids) = apply_patch_to_targets(
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
        assert_eq!(
            applied_ids.len(),
            targets.len(),
            "F5: applied_ids is populated regardless of the undo-snapshot cap"
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
            unchanged: 0,
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
            unchanged: 0,
        };
        let (level, msg) = batch_result_message(&result, "Warm portrait", false, None);
        assert_eq!(level, crate::notifications::Level::Warning);
        assert_eq!(
            msg,
            "Applied \u{201c}Warm portrait\u{201d} to 3 images. 1 failed, 1 skipped."
        );
    }

    /// An all-unchanged batch (nothing applied, failed, or skipped — every
    /// target simply already matched) must say so plainly rather than
    /// falling into the ordinary success phrasing, which would otherwise
    /// read "Applied ... to 0 images." — indistinguishable from a silent
    /// failure. This is the wording that would have told the author
    /// immediately what happened instead of it being filed as a bug.
    #[test]
    fn message_reports_all_unchanged_plainly() {
        let result = BatchResult {
            applied: 0,
            failed: 0,
            skipped: 0,
            unchanged: 3,
        };
        let (level, msg) = batch_result_message(&result, "Pasted settings", false, None);
        assert_eq!(level, crate::notifications::Level::Info);
        assert_eq!(
            msg,
            "Applied \u{201c}Pasted settings\u{201d} \u{2014} no changes (3 images already matched)."
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
            unchanged: 0,
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
            unchanged: 0,
        };
        let (level, msg) = batch_result_message(&result, "Warm portrait", true, None);
        assert_eq!(level, crate::notifications::Level::Warning);
        assert_eq!(
            msg,
            "Cancelled \u{2014} applied to 40 of 500 images. 2 failed."
        );
    }

    /// F4 (whole-branch review): applying a preset to a batch over
    /// `BATCH_UNDO_MAX` never gets a snapshot, so `undo_hint` is `None` — but
    /// unlike an ordinary no-snapshot case, the user must be told explicitly
    /// that undo is unavailable, not just left to notice its absence.
    #[test]
    fn message_over_undo_cap_warns_undo_is_unavailable() {
        let result = BatchResult {
            applied: BATCH_UNDO_MAX + 1,
            failed: 0,
            skipped: 0,
            unchanged: 0,
        };
        let (level, msg) = batch_result_message(&result, "Warm portrait", false, None);
        assert_eq!(level, crate::notifications::Level::Info);
        assert!(
            msg.contains(&format!(
                "Undo is unavailable for batches over {BATCH_UNDO_MAX} images."
            )),
            "must explicitly warn that undo is unavailable over the cap: {msg}"
        );
    }

    /// A batch AT or under the cap that simply had nothing retained (e.g.
    /// nothing applied) must NOT get the over-cap warning — it only fires
    /// when `total() > BATCH_UNDO_MAX`.
    #[test]
    fn message_under_undo_cap_without_hint_has_no_unavailable_warning() {
        let result = BatchResult {
            applied: 0,
            failed: 0,
            skipped: 3,
            unchanged: 0,
        };
        let (_level, msg) = batch_result_message(&result, "Warm portrait", false, None);
        assert!(
            !msg.contains("unavailable"),
            "a small batch must not claim undo is unavailable: {msg}"
        );
    }

    /// The undo path gets its own phrasing, never "Undo" plugged into the
    /// preset-name slot (`Applied "Undo" to N images.` would read as if
    /// "Undo" were a preset).
    #[test]
    fn undo_message_reports_full_success() {
        let result = BatchResult {
            applied: 5,
            failed: 0,
            skipped: 0,
            unchanged: 0,
        };
        let (level, msg) = batch_undo_message(&result);
        assert_eq!(level, crate::notifications::Level::Info);
        assert_eq!(msg, "Reverted the last batch apply on 5 images.");
        assert!(!msg.contains("Undo"), "must not name Undo as if a preset");
    }

    /// Any failure (write failure or an undeserializable snapshot entry,
    /// already folded into `failed` by `spawn_batch_undo`) steps the undo
    /// toast up to Warning and names the count.
    #[test]
    fn undo_message_reports_failures_at_warning() {
        let result = BatchResult {
            applied: 3,
            failed: 2,
            skipped: 0,
            unchanged: 0,
        };
        let (level, msg) = batch_undo_message(&result);
        assert_eq!(level, crate::notifications::Level::Warning);
        assert_eq!(msg, "Reverted the last batch apply on 3 images. 2 failed.");
    }

    /// `snapshot_documents` dropping an unparseable entry must be reflected
    /// as a decode failure so `spawn_batch_undo` can fold it into
    /// `BatchResult.failed` — an image left un-reverted is a failure to
    /// restore it, not a silent no-op (see that function's doc comment).
    #[test]
    fn undo_snapshot_decode_failures_counts_dropped_entries() {
        let mut prior = EditDoc::default();
        prior.global.exposure = -1.0;
        let snap = UndoSnapshot {
            entries: vec![
                (1, std::path::PathBuf::from("/a"), "garbage {{".into()),
                (
                    2,
                    std::path::PathBuf::from("/b"),
                    ferrolite_pipeline::serialize(&prior),
                ),
                (3, std::path::PathBuf::from("/c"), "also garbage".into()),
            ],
        };
        assert_eq!(undo_snapshot_decode_failures(&snap), 2);
    }

    /// A snapshot with nothing undecodable must report zero decode failures
    /// — this fix must not manufacture spurious failures on the happy path.
    #[test]
    fn undo_snapshot_decode_failures_is_zero_when_all_entries_parse() {
        let mut prior = EditDoc::default();
        prior.global.exposure = 0.5;
        let snap = UndoSnapshot {
            entries: vec![(
                1,
                std::path::PathBuf::from("/a"),
                ferrolite_pipeline::serialize(&prior),
            )],
        };
        assert_eq!(undo_snapshot_decode_failures(&snap), 0);
    }

    /// A cancel landing strictly BEFORE the run completed every target
    /// (some remainder never got attempted) must read as genuinely
    /// cancelled.
    #[test]
    fn genuinely_cancelled_when_targets_remain_unattempted() {
        let result = BatchResult {
            applied: 47,
            failed: 0,
            skipped: 453,
            unchanged: 0,
        };
        assert!(batch_was_genuinely_cancelled(true, &result, 500));
    }

    /// A cancel token that fired but landed AFTER the run had already
    /// attempted every target (applied + failed == target_count) must NOT
    /// read as cancelled — this is the exact bug reported: "Cancelled —
    /// applied to 500 of 500 images."
    #[test]
    fn not_cancelled_when_the_cancel_lands_after_the_last_item() {
        let result = BatchResult {
            applied: 500,
            failed: 0,
            skipped: 0,
            unchanged: 0,
        };
        assert!(!batch_was_genuinely_cancelled(true, &result, 500));
    }

    /// A cancel flag that never fired must never read as cancelled,
    /// regardless of how many targets were attempted.
    #[test]
    fn not_cancelled_when_the_cancel_flag_never_fired() {
        let result = BatchResult {
            applied: 47,
            failed: 0,
            skipped: 453,
            unchanged: 0,
        };
        assert!(!batch_was_genuinely_cancelled(false, &result, 500));
    }
}
