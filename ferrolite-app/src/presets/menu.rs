//! Behaviour behind the four P7 library-context-menu items — copy settings,
//! paste settings, apply preset, save preset (design §6).
//!
//! It lives here, next to the store/modal/batch machinery, rather than inside
//! `library::image_context_menu`, because everything in this file is testable
//! WITHOUT egui: target construction, the off-thread source-document read, and
//! what happens when the group modal is confirmed. Only the actual menu
//! rendering (which egui makes untestable) stays in `image_context_menu`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use ferrolite_jobs::{JobSystem, Priority};
use ferrolite_pipeline::{EditDoc, EditPatch, GroupSet, PATCH_VERSION};

use crate::events::AppEvent;
use crate::notifications::Level;
use crate::state::AppState;

use super::apply::BatchTarget;
use super::modal::GroupModal;
use super::Preset;

/// Confirmation toast for a completed "Copy settings".
const COPIED_MESSAGE: &str = "Copied settings.";

/// Appended to a batch's result toast when the image open in Develop was left
/// out of the targets (design §5.1). Leading space: it is concatenated onto a
/// sentence produced by `apply::batch_result_message`.
pub const EXCLUDED_OPEN_IMAGE_NOTE: &str = " The image open in Develop was skipped.";

/// What an open `GroupModal` will do once the user confirms it.
///
/// The paste targets are captured when the modal OPENS, not when it is
/// confirmed: the modal is a plain window (the grid behind it stays live), so
/// resolving the selection again on confirm could act on a different set of
/// images than the "Paste settings to N images" title promised.
pub enum GroupModalPurpose {
    /// Save a preset built from this document. Boxed so the one large variant
    /// does not set the size of the whole enum (`clippy::large_enum_variant`).
    SavePreset { doc: Box<EditDoc> },
    /// Paste the session clipboard patch onto these targets.
    Paste {
        targets: Vec<BatchTarget>,
        excluded_open_image: bool,
    },
}

/// The open group modal plus what it is for. Held on `AppState` (not on
/// `FerroliteApp`) because the context menu — which opens it — only ever
/// receives `&mut AppState`.
pub struct PendingGroupModal {
    pub modal: GroupModal,
    pub purpose: GroupModalPurpose,
}

/// Why a source document is being read off the UI thread.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DocReadPurpose {
    /// Fill the copy-settings clipboard.
    Copy,
    /// Open the "Save preset" modal over the document once it arrives.
    SavePreset,
}

/// Pair `ids` with their file paths, EXCLUDING the image open in Develop so a
/// batch never races the live session's own sidecar writes (design §5.1).
///
/// Returns `(targets, excluded_open_image)`. Ids `resolve` cannot place on disk
/// are dropped silently — they are catalog rows whose folder has gone missing,
/// which a batch cannot act on either way.
///
/// Split out from `batch_targets` so the whole decision — exclusion, dropping
/// unresolvable ids, the "everything was excluded" case — is unit-testable
/// without a catalog.
pub fn build_batch_targets(
    ids: &[i64],
    open_in_develop: Option<i64>,
    mut resolve: impl FnMut(i64) -> Option<PathBuf>,
) -> (Vec<BatchTarget>, bool) {
    let mut excluded_open_image = false;
    let mut targets = Vec::with_capacity(ids.len());
    for &image_id in ids {
        if Some(image_id) == open_in_develop {
            excluded_open_image = true;
            continue;
        }
        if let Some(path) = resolve(image_id) {
            targets.push(BatchTarget { image_id, path });
        }
    }
    (targets, excluded_open_image)
}

/// Whether every id in `ids` is the image open in Develop — i.e. once §5.1's
/// exclusion is applied a paste or preset apply would have nothing left to do.
///
/// Purely in-memory (no record lookup, no catalog read), so the context menu
/// can call it on every frame it is open to decide whether to GREY those two
/// items. Greying with an honest reason is the house convention; letting the
/// user click into a guaranteed no-op is not.
pub fn all_targets_excluded(ids: &[i64], open_in_develop: Option<i64>) -> bool {
    open_in_develop.is_some()
        && !ids.is_empty()
        && ids.iter().all(|id| Some(*id) == open_in_develop)
}

/// A path resolver over `records` that consults `folder_path` AT MOST ONCE per
/// distinct `folder_id`.
///
/// Both memoizations matter on a click handler that may see thousands of ids:
/// `folder_path` is a read-pool SQLite query (contract 1 — a per-target
/// round-trip inside a click handler is exactly the UI-thread stall the rule
/// exists to prevent), and the id→record lookup would otherwise be
/// O(targets × images). A selection almost always shares one folder, so the
/// query count collapses to 1.
///
/// The path build (folder path + filename) mirrors `AppState::image_path`; it
/// is repeated here rather than called because `image_path` resolves the folder
/// itself and so cannot be memoized from outside.
pub fn memoized_path_resolver<'a>(
    records: &'a [ferrolite_catalog::ImageRecord],
    mut folder_path: impl FnMut(i64) -> Option<PathBuf> + 'a,
) -> impl FnMut(i64) -> Option<PathBuf> + 'a {
    let by_id: HashMap<i64, &ferrolite_catalog::ImageRecord> =
        records.iter().map(|r| (r.id, r)).collect();
    let mut folders: HashMap<i64, Option<PathBuf>> = HashMap::new();
    move |id| {
        let rec = by_id.get(&id)?;
        let base = folders
            .entry(rec.folder_id)
            .or_insert_with(|| folder_path(rec.folder_id))
            .clone()?;
        Some(base.join(&rec.filename))
    }
}

/// `build_batch_targets` against the live catalog, resolving each distinct
/// folder exactly once (see `memoized_path_resolver`).
pub fn batch_targets(state: &AppState, ids: &[i64]) -> (Vec<BatchTarget>, bool) {
    let open = state.viewer.as_ref().map(|v| v.image_id);
    let resolve = memoized_path_resolver(&state.images, |folder_id| {
        state
            .reads
            .folder_path(folder_id)
            .ok()
            .flatten()
            .map(PathBuf::from)
    });
    build_batch_targets(ids, open, resolve)
}

/// Message for a copy/paste/preset action that ended up with nothing to do.
fn empty_target_message(excluded_open_image: bool) -> &'static str {
    if excluded_open_image {
        "Nothing to apply — the image open in Develop was skipped."
    } else {
        "Nothing to apply — those images could not be located."
    }
}

/// The source document for `image_id` without touching the disk, but ONLY when
/// that image is the one open in Develop. Its in-memory stack runs ahead of the
/// sidecar (ops persistence is asynchronous), so reading the file there would
/// capture a stale document.
fn live_doc(state: &AppState, image_id: i64) -> Option<EditDoc> {
    state
        .viewer
        .as_ref()
        .filter(|v| v.image_id == image_id)
        .map(|v| v.op_stack.clone())
}

/// Absolute path of `image_id` from the browsed record list.
fn source_path(state: &AppState, image_id: i64) -> Option<PathBuf> {
    state
        .images
        .iter()
        .find(|r| r.id == image_id)
        .and_then(|rec| state.image_path(rec))
}

/// Read one image's `frl:ops` document off the UI thread (contract 1 — this is
/// file I/O) and deliver it as `AppEvent::MenuDocRead`. Mirrors
/// `develop::ops_persist::spawn_ops_read`; a missing or malformed sidecar
/// yields the default (unedited) document rather than failing.
fn spawn_doc_read(
    jobs: &Arc<JobSystem>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    path: PathBuf,
    purpose: DocReadPurpose,
) {
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Interactive, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        let xmp = ferrolite_catalog::sidecar_path(&path);
        let doc = ferrolite_catalog::read_ops(&xmp)
            .and_then(|p| ferrolite_pipeline::deserialize(&p))
            .unwrap_or_default();
        let _ = tx.send(AppEvent::MenuDocRead { doc, purpose });
        ctx.request_repaint();
    });
}

/// Install `doc` as the session copy-settings clipboard.
///
/// Captures the FULL document (`default_owns()`); the paste dialog narrows it
/// later, so an ad-hoc copy never has to guess intent up front (design §6.2).
pub fn set_clipboard(state: &mut AppState, doc: &EditDoc) {
    state.clipboard_patch = Some(EditPatch::from_doc(doc, super::modal::default_owns()));
    state.notify(Level::Info, COPIED_MESSAGE);
}

/// Open the "Save preset" modal over an already-resolved source document.
pub fn open_save_modal(state: &mut AppState, doc: EditDoc) {
    state.open_group_modal = Some(PendingGroupModal {
        modal: GroupModal::new_save(),
        purpose: GroupModalPurpose::SavePreset { doc: Box::new(doc) },
    });
}

/// "Copy settings" — capture `image_id`'s document into the session clipboard.
pub fn start_copy(state: &mut AppState, ctx: &egui::Context, image_id: i64) {
    if let Some(doc) = live_doc(state, image_id) {
        set_clipboard(state, &doc);
        return;
    }
    match source_path(state, image_id) {
        Some(path) => spawn_doc_read(&state.jobs, &state.tx, ctx, path, DocReadPurpose::Copy),
        None => state.notify(Level::Error, "Could not locate that image's file."),
    }
}

/// "Save preset from this image…" — resolve the source document, then open the
/// group modal in Save mode (immediately when the image is open in Develop,
/// otherwise once the off-thread sidecar read lands).
pub fn start_save_preset(state: &mut AppState, ctx: &egui::Context, image_id: i64) {
    if let Some(doc) = live_doc(state, image_id) {
        open_save_modal(state, doc);
        return;
    }
    match source_path(state, image_id) {
        Some(path) => spawn_doc_read(
            &state.jobs,
            &state.tx,
            ctx,
            path,
            DocReadPurpose::SavePreset,
        ),
        None => state.notify(Level::Error, "Could not locate that image's file."),
    }
}

/// "Paste settings…" — open the group modal so the user can narrow which groups
/// the ad-hoc copy writes. No dialog means no way to express that intent, which
/// is why pasting has one and applying a preset does not.
pub fn start_paste(state: &mut AppState, ids: &[i64]) {
    if state.clipboard_patch.is_none() {
        return; // the menu item is disabled in this case; belt and braces
    }
    let (targets, excluded_open_image) = batch_targets(state, ids);
    if targets.is_empty() {
        state.notify(Level::Info, empty_target_message(excluded_open_image));
        return;
    }
    state.open_group_modal = Some(PendingGroupModal {
        modal: GroupModal::new_paste(targets.len()),
        purpose: GroupModalPurpose::Paste {
            targets,
            excluded_open_image,
        },
    });
}

/// "Apply preset" — a preset already declares its own groups, so this takes no
/// dialog: build the targets and run the batch straight away (design §6.3).
pub fn apply_preset(state: &mut AppState, ctx: &egui::Context, index: usize, ids: &[i64]) {
    let Some(preset) = state.presets.get(index) else {
        return; // list changed under a stale menu index
    };
    let patch = preset.to_patch();
    let label = preset.name.clone();

    let (targets, excluded_open_image) = batch_targets(state, ids);
    if targets.is_empty() {
        state.notify(Level::Info, empty_target_message(excluded_open_image));
        return;
    }
    state.batch_excluded_open_image = excluded_open_image;
    super::apply::spawn_batch_apply(
        &state.jobs,
        &state.writer,
        &state.tx,
        ctx,
        patch,
        targets,
        label,
    );
}

/// Act on a confirmed group modal. Returns `true` when the modal should CLOSE.
///
/// A rejected preset name returns `false` and leaves the reason on
/// `modal.name_error`, so the user fixes the name instead of losing everything
/// they typed.
pub fn confirm_group_modal(
    state: &mut AppState,
    ctx: &egui::Context,
    pending: &mut PendingGroupModal,
    name: Option<String>,
    owns: GroupSet,
) -> bool {
    match &pending.purpose {
        GroupModalPurpose::SavePreset { doc } => {
            let preset = Preset {
                version: PATCH_VERSION,
                name: name.unwrap_or_default(),
                owns,
                doc: (**doc).clone(),
            };
            match super::save(&super::presets_dir(), &preset) {
                Ok(_) => {
                    // The write itself is synchronous — one small JSON file,
                    // and the modal can only report a rejected name inline if
                    // it knows the outcome now. The directory RE-SCAN has no
                    // such constraint, so it goes through the off-thread
                    // `spawn_load_all` (contract 1) and lands via
                    // `AppEvent::PresetsLoaded`.
                    super::spawn_load_all(&state.jobs, &state.tx, ctx);
                    state.notify(
                        Level::Info,
                        format!("Saved preset \u{201c}{}\u{201d}.", preset.name),
                    );
                    true
                }
                Err(e) => {
                    pending.modal.name_error = Some(e.to_string());
                    false
                }
            }
        }
        GroupModalPurpose::Paste {
            targets,
            excluded_open_image,
        } => {
            let Some(clip) = state.clipboard_patch.clone() else {
                return true; // clipboard cleared while the modal was open
            };
            // `owns` comes from the dialog, narrowing the full document the
            // copy captured.
            let patch = EditPatch {
                version: clip.version,
                owns,
                doc: clip.doc,
            };
            state.batch_excluded_open_image = *excluded_open_image;
            super::apply::spawn_batch_apply(
                &state.jobs,
                &state.writer,
                &state.tx,
                ctx,
                patch,
                targets.clone(),
                "Pasted settings".to_string(),
            );
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::image_context_menu::regen_target_ids;
    use std::collections::HashSet;

    fn fake_resolve(id: i64) -> Option<PathBuf> {
        Some(PathBuf::from(format!("/photos/{id}.arw")))
    }

    fn rec(id: i64, folder_id: i64, filename: &str) -> ferrolite_catalog::ImageRecord {
        ferrolite_catalog::ImageRecord {
            id,
            folder_id,
            filename: filename.to_string(),
            width: None,
            height: None,
            orientation: ferrolite_image::Orientation::Normal,
            capture_time: None,
            iso: None,
            decode_status: ferrolite_catalog::DecodeStatus::Done,
            kind: ferrolite_image::FileKind::Raw,
            rating: ferrolite_image::Rating::new(0),
            flag: ferrolite_image::Flag::None,
            has_edits: false,
            thumb_w: None,
            thumb_h: None,
        }
    }

    #[test]
    fn every_id_becomes_a_target_when_nothing_is_open_in_develop() {
        let (targets, excluded) = build_batch_targets(&[1, 2, 3], None, fake_resolve);
        assert_eq!(targets.len(), 3);
        assert_eq!(
            targets.iter().map(|t| t.image_id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(!excluded);
    }

    #[test]
    fn the_image_open_in_develop_is_excluded_and_reported() {
        let (targets, excluded) = build_batch_targets(&[1, 2, 3], Some(2), fake_resolve);
        assert_eq!(
            targets.iter().map(|t| t.image_id).collect::<Vec<_>>(),
            vec![1, 3],
            "the open image must never be a batch target (design §5.1)"
        );
        assert!(
            excluded,
            "the exclusion must be reported so the toast says so"
        );
    }

    /// The single-image case where the ONLY target is the open Develop image:
    /// nothing is left to apply, and the caller must say why.
    #[test]
    fn all_targets_excluded_yields_no_targets_and_an_honest_reason() {
        let (targets, excluded) = build_batch_targets(&[7], Some(7), fake_resolve);
        assert!(targets.is_empty());
        assert!(excluded);
        assert_eq!(
            empty_target_message(excluded),
            "Nothing to apply — the image open in Develop was skipped."
        );
    }

    #[test]
    fn unresolvable_ids_are_dropped_without_affecting_the_exclusion_flag() {
        let (targets, excluded) = build_batch_targets(&[1, 2, 3], None, |id| {
            if id == 2 {
                None // a row whose folder has gone missing
            } else {
                fake_resolve(id)
            }
        });
        assert_eq!(
            targets.iter().map(|t| t.image_id).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert!(!excluded);
        assert_eq!(
            empty_target_message(false),
            "Nothing to apply — those images could not be located."
        );
    }

    /// Selection scoping must match every other multi-image action in the menu:
    /// right-clicking an image that is NOT in the selection acts on that image
    /// alone, never on the whole selection.
    #[test]
    fn right_clicking_outside_the_selection_targets_only_that_image() {
        let selection: HashSet<i64> = [1, 2, 3].into_iter().collect();
        let ids = regen_target_ids(false, 9, &selection);
        let (targets, _) = build_batch_targets(&ids, None, fake_resolve);
        assert_eq!(
            targets.iter().map(|t| t.image_id).collect::<Vec<_>>(),
            vec![9]
        );
    }

    #[test]
    fn right_clicking_inside_the_selection_targets_the_whole_selection() {
        let selection: HashSet<i64> = [1, 2, 3].into_iter().collect();
        let ids = regen_target_ids(false, 2, &selection);
        let (targets, _) = build_batch_targets(&ids, None, fake_resolve);
        assert_eq!(
            targets.iter().map(|t| t.image_id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    /// Loupe/filmstrip scoping (`single_image = true`) never picks up a stale
    /// grid selection, even when the right-clicked image is part of it.
    #[test]
    fn single_image_scope_targets_one_image_even_with_a_stale_selection() {
        let selection: HashSet<i64> = [1, 2, 3].into_iter().collect();
        let ids = regen_target_ids(true, 2, &selection);
        let (targets, _) = build_batch_targets(&ids, None, fake_resolve);
        assert_eq!(
            targets.iter().map(|t| t.image_id).collect::<Vec<_>>(),
            vec![2]
        );
    }

    /// A selection that shares one folder must cost ONE `folder_path` query,
    /// not one per image: this runs inside a click handler, and a read-pool
    /// round-trip per target is the UI-thread stall contract 1 forbids.
    #[test]
    fn one_shared_folder_costs_exactly_one_folder_lookup() {
        let records = vec![
            rec(1, 10, "a.arw"),
            rec(2, 10, "b.arw"),
            rec(3, 10, "c.arw"),
        ];
        let mut lookups = 0usize;
        let resolve = memoized_path_resolver(&records, |folder_id| {
            lookups += 1;
            Some(PathBuf::from(format!("/vol/{folder_id}")))
        });
        let (targets, _) = build_batch_targets(&[1, 2, 3], None, resolve);

        assert_eq!(lookups, 1, "three images in one folder = one query");
        assert_eq!(
            targets.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
            vec![
                PathBuf::from("/vol/10").join("a.arw"),
                PathBuf::from("/vol/10").join("b.arw"),
                PathBuf::from("/vol/10").join("c.arw"),
            ]
        );
    }

    /// Mixed folders: one lookup per DISTINCT folder (not per image), and every
    /// path is still joined against its own folder.
    #[test]
    fn mixed_folders_cost_one_lookup_each_and_keep_the_right_paths() {
        let records = vec![
            rec(1, 10, "a.arw"),
            rec(2, 20, "b.arw"),
            rec(3, 10, "c.arw"),
            rec(4, 20, "d.arw"),
        ];
        let mut seen: Vec<i64> = Vec::new();
        let resolve = memoized_path_resolver(&records, |folder_id| {
            seen.push(folder_id);
            Some(PathBuf::from(format!("/vol/{folder_id}")))
        });
        let (targets, _) = build_batch_targets(&[1, 2, 3, 4], None, resolve);

        assert_eq!(seen, vec![10, 20], "each distinct folder resolved once");
        assert_eq!(
            targets.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
            vec![
                PathBuf::from("/vol/10").join("a.arw"),
                PathBuf::from("/vol/20").join("b.arw"),
                PathBuf::from("/vol/10").join("c.arw"),
                PathBuf::from("/vol/20").join("d.arw"),
            ]
        );
    }

    /// A folder that cannot be resolved is remembered as a MISS — the failing
    /// query must not be retried once per image in it either.
    #[test]
    fn an_unresolvable_folder_is_queried_once_and_drops_its_images() {
        let records = vec![
            rec(1, 10, "a.arw"),
            rec(2, 99, "b.arw"),
            rec(3, 99, "c.arw"),
        ];
        let mut lookups = 0usize;
        let resolve = memoized_path_resolver(&records, |folder_id| {
            lookups += 1;
            (folder_id != 99).then(|| PathBuf::from(format!("/vol/{folder_id}")))
        });
        let (targets, _) = build_batch_targets(&[1, 2, 3], None, resolve);

        assert_eq!(lookups, 2, "the missing folder is not re-queried per image");
        assert_eq!(
            targets.iter().map(|t| t.image_id).collect::<Vec<_>>(),
            vec![1]
        );
    }

    /// An id with no record in the browsed list resolves to nothing without
    /// touching the catalog at all.
    #[test]
    fn an_unknown_id_never_reaches_the_folder_query() {
        let records = vec![rec(1, 10, "a.arw")];
        let mut lookups = 0usize;
        let resolve = memoized_path_resolver(&records, |_| {
            lookups += 1;
            Some(PathBuf::from("/vol"))
        });
        let (targets, _) = build_batch_targets(&[42], None, resolve);

        assert_eq!(lookups, 0);
        assert!(targets.is_empty());
    }

    /// The greying predicate the menu uses: true only when EVERY target is the
    /// image open in Develop, so the two multi-image items can be disabled with
    /// an honest reason instead of clicking into a guaranteed no-op.
    #[test]
    fn all_targets_excluded_is_true_only_when_every_target_is_the_open_image() {
        assert!(
            all_targets_excluded(&[7], Some(7)),
            "right-clicking the open image alone"
        );
        assert!(
            all_targets_excluded(&[7, 7], Some(7)),
            "a degenerate duplicate list is still all-excluded"
        );
        assert!(
            !all_targets_excluded(&[7, 8], Some(7)),
            "one survivor is enough to keep the action live"
        );
        assert!(
            !all_targets_excluded(&[7], Some(8)),
            "a different image is open"
        );
        assert!(
            !all_targets_excluded(&[7], None),
            "no Develop session means nothing is excluded"
        );
        assert!(
            !all_targets_excluded(&[], Some(7)),
            "an empty target list is not an exclusion — there was nothing to exclude"
        );
    }

    /// The predicate and the actual target build must agree: whenever the menu
    /// greys the item, `build_batch_targets` would indeed have produced zero
    /// targets (and vice versa for the live case).
    #[test]
    fn the_greying_predicate_agrees_with_the_target_build() {
        for (ids, open) in [
            (vec![7i64], Some(7i64)),
            (vec![7, 8], Some(7)),
            (vec![7], None),
            (vec![7, 8], None),
        ] {
            let (targets, _) = build_batch_targets(&ids, open, fake_resolve);
            assert_eq!(
                all_targets_excluded(&ids, open),
                targets.is_empty(),
                "predicate disagreed with the build for ids={ids:?} open={open:?}"
            );
        }
    }

    /// The copy clipboard captures the FULL document; narrowing is the paste
    /// dialog's job (design §6.2).
    #[test]
    fn set_clipboard_captures_the_full_document_with_the_default_group_set() {
        let mut state = AppState::for_test();
        let doc = EditDoc::default().set_op(ferrolite_pipeline::Op::Exposure(
            ferrolite_pipeline::Exposure { ev: 0.5 },
        ));
        set_clipboard(&mut state, &doc);
        let clip = state.clipboard_patch.expect("clipboard must be filled");
        assert_eq!(clip.owns, super::super::modal::default_owns());
        assert_eq!(clip.doc, doc);
        assert_eq!(clip.version, PATCH_VERSION);
    }
}
