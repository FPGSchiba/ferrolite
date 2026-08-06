//! Behaviour behind the four P7 library-context-menu items — copy settings,
//! paste settings, apply preset, save preset (design §6).
//!
//! It lives here, next to the store/modal/batch machinery, rather than inside
//! `library::image_context_menu`, because everything in this file is testable
//! WITHOUT egui: target construction, the off-thread source-document read, and
//! what happens when the group modal is confirmed. Only the actual menu
//! rendering (which egui makes untestable) stays in `image_context_menu`.

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
    resolve: impl Fn(i64) -> Option<PathBuf>,
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

/// `build_batch_targets` against the live catalog: ids are looked up in the
/// browsed record list and turned into paths by `AppState::image_path`.
pub fn batch_targets(state: &AppState, ids: &[i64]) -> (Vec<BatchTarget>, bool) {
    let open = state.viewer.as_ref().map(|v| v.image_id);
    build_batch_targets(ids, open, |id| {
        state
            .images
            .iter()
            .find(|r| r.id == id)
            .and_then(|rec| state.image_path(rec))
    })
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
