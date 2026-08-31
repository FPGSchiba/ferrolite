//! The Develop-panel "Presets" menu (P7 Task 8): a compact menu button —
//! deliberately NOT a tab, so it never competes with the Light/Color/Effects
//! tab row (design intent) — that lets the user apply a saved preset to the
//! image currently open in Develop, save the current edit as a new preset,
//! or rename/delete a saved preset.
//!
//! Applying a preset to the CURRENT image goes through the exact same
//! edit-commit path a slider drag uses: this module hands back an
//! `EditOutcome`, which flows through `PanelOutcome` to
//! `AppController::apply_edit` (wired in `app.rs`), exactly like every base
//! tab's `PanelTab::show`. That is what makes it land in Develop's own undo
//! history and go through the existing (exactly-once) sidecar write — this
//! module never touches the sidecar directly.
//!
//! Save/rename/delete instead mutate `AppState` directly (open the shared
//! `GroupModal` in save mode, or stage a `PendingRenamePreset` /
//! `PendingDeletePreset` for `app.rs` to drive) and return no edit.

use crate::develop::adjustment_panel::EditOutcome;
use crate::notifications::Level;
use crate::state::{AppState, PendingDeletePreset, PendingRenamePreset};
use ferrolite_pipeline::{GroupSet, OpKind};

/// One menu click, deferred until after the menu-drawing closures return —
/// mirrors the Library context menu's "Apply preset" submenu
/// (`image_context_menu::show_edit_settings_items`), which defers its own
/// `state`-mutating call the same way to avoid borrowing `state` from inside
/// the closures egui's `menu_button` takes.
enum Chosen {
    Apply(usize),
    SaveCurrent,
    Rename(usize),
    Delete(usize),
}

/// Render the "Presets" menu button and act on whatever was clicked this
/// frame. Returns `Some(EditOutcome)` only when a preset was applied to the
/// current image; every other action (save/rename/delete) mutates
/// `AppState` directly and returns `None`.
pub(crate) fn presets_row(ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
    let has_viewer = state.viewer.is_some();
    let preset_names: Vec<String> = state.presets.iter().map(|p| p.name.clone()).collect();
    let mut chosen: Option<Chosen> = None;

    let resp = ui
        .add_enabled_ui(has_viewer, |ui| {
            ui.menu_button(format!("{} Presets", crate::icons::PRESET), |ui| {
                if preset_names.is_empty() {
                    ui.add_enabled(false, egui::Button::new("No presets saved"))
                        .on_disabled_hover_text("Save the current edit as a preset first");
                }
                for (i, name) in preset_names.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.button(name).clicked() {
                            chosen = Some(Chosen::Apply(i));
                            ui.close_menu();
                        }
                        ui.menu_button(crate::icons::EDIT, |ui| {
                            if ui.button("Rename\u{2026}").clicked() {
                                chosen = Some(Chosen::Rename(i));
                                ui.close_menu();
                            }
                            if ui.button("Delete").clicked() {
                                chosen = Some(Chosen::Delete(i));
                                ui.close_menu();
                            }
                        });
                    });
                }
                ui.separator();
                if ui.button("Save current as preset\u{2026}").clicked() {
                    chosen = Some(Chosen::SaveCurrent);
                    ui.close_menu();
                }
            });
        })
        .response;
    if !has_viewer {
        resp.on_disabled_hover_text("Open an image in Develop to apply or save a preset");
    }

    match chosen {
        Some(Chosen::Apply(i)) => return apply_preset_to_current(state, i),
        Some(Chosen::SaveCurrent) => {
            if let Some(doc) = state.viewer.as_ref().map(|v| v.op_stack.clone()) {
                crate::presets::menu::open_save_modal(state, doc);
            }
        }
        Some(Chosen::Rename(i)) => start_rename(state, i),
        Some(Chosen::Delete(i)) => {
            if let Some(preset) = state.presets.get(i).cloned() {
                state.pending_delete_preset = Some(PendingDeletePreset { preset });
            }
        }
        None => {}
    }
    None
}

/// Which `OpKind` to tag a preset's merged document with. This only matters
/// for two things — `History`'s same-kind coalescing (separately guarded by
/// the `break_coalesce` call in `apply_preset_to_current`, so it does not
/// depend on this choice being "right") and
/// `AppController::maybe_spawn_lens_bake`, which only fires on
/// `OpKind::LensCorrection` — so a preset that owns the LENS group must be
/// tagged with it, or a changed correction amount would apply without
/// re-baking the warp grid.
fn preset_apply_kind(owns: GroupSet) -> OpKind {
    if owns.contains(GroupSet::LENS) {
        OpKind::LensCorrection
    } else {
        OpKind::LocalAdjustments
    }
}

/// Merge `state.presets[index]` into the CURRENT image's document and return
/// the resulting `EditOutcome`, or `None` when there is nothing to apply: no
/// viewer, a stale index (the list changed under an open menu), or the
/// preset is already a no-op against the current edit.
fn apply_preset_to_current(state: &mut AppState, index: usize) -> Option<EditOutcome> {
    let preset = state.presets.get(index)?.clone();
    let current = state.viewer.as_ref()?.op_stack.clone();
    let patch = preset.to_patch();
    let merged = patch.apply_to(&current);
    if merged == current {
        state.notify(
            Level::Info,
            format!(
                "\u{201c}{}\u{201d} matches the current edit already.",
                preset.name
            ),
        );
        return None;
    }
    // A preset apply is a discrete, deliberate action — never a continuation
    // of whatever the user was dragging a moment ago. Force a fresh undo
    // step regardless of which `OpKind` this apply ends up tagged with,
    // rather than relying on it happening to differ from the previous
    // push's kind (see `History::push`'s same-kind coalescing rule).
    if let Some(v) = state.viewer.as_mut() {
        v.history.break_coalesce();
    }
    Some(EditOutcome {
        stack: merged,
        kind: preset_apply_kind(patch.owns),
        commit: true,
    })
}

/// Stage the "Rename preset" dialog (driven by `FerroliteApp::drive_rename_preset`).
fn start_rename(state: &mut AppState, index: usize) {
    let Some(preset) = state.presets.get(index) else {
        return; // stale index — the list changed under an open menu
    };
    state.pending_rename_preset = Some(PendingRenamePreset {
        new_name: preset.name.clone(),
        original: preset.clone(),
        error: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{EditPatch, Op, PATCH_VERSION};

    // default-then-assign mirrors `presets::store`'s own `sample()` helper;
    // clearer than struct-update syntax for a single nested field.
    #[allow(clippy::field_reassign_with_default)]
    fn preset_with_exposure(name: &str, owns: GroupSet, ev: f32) -> crate::presets::Preset {
        let mut doc = ferrolite_pipeline::EditDoc::default();
        doc.global.exposure = ev;
        crate::presets::Preset {
            version: PATCH_VERSION,
            name: name.to_string(),
            owns,
            doc,
        }
    }

    fn viewer_with_stack(stack: ferrolite_pipeline::OpStack) -> crate::viewer::ViewerState {
        let mut v = crate::viewer::ViewerState::open(
            1,
            std::path::PathBuf::from("x"),
            ferrolite_image::FileKind::Raw,
        );
        v.op_stack = stack;
        v
    }

    #[test]
    fn preset_apply_kind_is_lens_correction_only_when_owns_includes_lens() {
        assert_eq!(
            preset_apply_kind(GroupSet::LENS),
            OpKind::LensCorrection,
            "a preset touching LENS must trigger the bake-check kind"
        );
        assert_eq!(
            preset_apply_kind(GroupSet::LIGHT.union(GroupSet::COLOR)),
            OpKind::LocalAdjustments,
            "no LENS group: falls back to the discrete-commit kind"
        );
        assert_eq!(preset_apply_kind(GroupSet::EMPTY), OpKind::LocalAdjustments);
    }

    #[test]
    fn apply_preset_to_current_merges_the_patch_into_the_live_doc() {
        let mut state = AppState::for_test();
        state.viewer = Some(viewer_with_stack(ferrolite_pipeline::OpStack::default()));
        state.presets = vec![preset_with_exposure("Warm", GroupSet::LIGHT, 1.5)];

        let out = apply_preset_to_current(&mut state, 0).expect("must produce an edit");
        assert_eq!(out.stack.global.exposure, 1.5);
        assert!(out.commit, "a preset apply is always a committed edit");
        assert_eq!(out.kind, OpKind::LocalAdjustments);
    }

    #[test]
    fn apply_preset_to_current_tags_lens_correction_when_owns_includes_lens() {
        let mut state = AppState::for_test();
        state.viewer = Some(viewer_with_stack(ferrolite_pipeline::OpStack::default()));
        let mut preset = preset_with_exposure("Lensy", GroupSet::LENS, 0.0);
        preset.doc.lens = Some(ferrolite_pipeline::LensCorrection {
            lens_id: None,
            focal_len: 24.0,
            aperture: 8.0,
            crop_factor: 1.0,
            distortion: ferrolite_pipeline::Correction {
                enabled: true,
                amount: 0.5,
            },
            tca: ferrolite_pipeline::Correction::default(),
            vignetting: ferrolite_pipeline::Correction::default(),
        });
        state.presets = vec![preset];

        // Target already has a LensCorrection so `apply_lens_amounts` has
        // something to write into (mirrors `patch.rs`'s own LENS test).
        let target = ferrolite_pipeline::OpStack {
            lens: Some(ferrolite_pipeline::LensCorrection {
                lens_id: Some("x".into()),
                focal_len: 35.0,
                aperture: 4.0,
                crop_factor: 1.0,
                distortion: ferrolite_pipeline::Correction::default(),
                tca: ferrolite_pipeline::Correction::default(),
                vignetting: ferrolite_pipeline::Correction::default(),
            }),
            ..ferrolite_pipeline::OpStack::default()
        };
        state.viewer = Some(viewer_with_stack(target));

        let out = apply_preset_to_current(&mut state, 0).expect("must produce an edit");
        assert_eq!(out.kind, OpKind::LensCorrection);
    }

    #[test]
    fn apply_preset_to_current_is_none_without_a_viewer() {
        let mut state = AppState::for_test();
        state.presets = vec![preset_with_exposure("Warm", GroupSet::LIGHT, 1.5)];
        assert!(apply_preset_to_current(&mut state, 0).is_none());
    }

    #[test]
    fn apply_preset_to_current_is_none_for_a_stale_index() {
        let mut state = AppState::for_test();
        state.viewer = Some(viewer_with_stack(ferrolite_pipeline::OpStack::default()));
        state.presets = vec![preset_with_exposure("Warm", GroupSet::LIGHT, 1.5)];
        assert!(apply_preset_to_current(&mut state, 5).is_none());
    }

    /// A preset that would leave the document byte-identical (already
    /// applied, or an empty-`owns` patch) must not push a spurious undo step
    /// or trigger a sidecar write — it returns `None` and only notifies.
    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn apply_preset_to_current_is_a_no_op_when_already_applied() {
        let mut state = AppState::for_test();
        let mut stack = ferrolite_pipeline::OpStack::default();
        stack.global.exposure = 1.5;
        state.viewer = Some(viewer_with_stack(stack));
        state.presets = vec![preset_with_exposure("Warm", GroupSet::LIGHT, 1.5)];

        assert!(apply_preset_to_current(&mut state, 0).is_none());
        assert_eq!(
            state.notifications.iter_newest_first().count(),
            1,
            "must still tell the user why nothing happened"
        );
    }

    /// Regardless of the immediately preceding edit's kind, applying a
    /// preset must start a FRESH undo step — it must never silently coalesce
    /// into whatever the user was dragging a moment ago.
    ///
    /// `History::push` only coalesces when `last_kind` still equals the
    /// incoming kind at push time. In the real commit path an
    /// `OpKind::LocalAdjustments` push always has its own coalescing broken
    /// immediately afterward (`AppController::apply_edit`), so this test
    /// pushes the "prior edit" directly (bypassing that auto-break) to build
    /// the at-risk precondition `apply_preset_to_current`'s own
    /// `break_coalesce()` call must guard against on its own — mirroring the
    /// real risk on the `OpKind::LensCorrection` branch, whose pushes get no
    /// such auto-break.
    #[test]
    fn apply_preset_to_current_breaks_coalescing_with_the_prior_edit() {
        let mut state = AppState::for_test();
        let mut v = viewer_with_stack(ferrolite_pipeline::OpStack::default());
        v.history
            .push(OpKind::LocalAdjustments, local_adjustments_stack());
        state.viewer = Some(v);
        state.presets = vec![preset_with_exposure("Warm", GroupSet::LIGHT, 1.5)];

        let out = apply_preset_to_current(&mut state, 0).expect("must produce an edit");
        // Push the resulting stack under the SAME kind the preset apply
        // reports, exactly as `AppController::apply_edit` would, and confirm
        // it did NOT coalesce into the simulated prior step.
        let v = state.viewer.as_mut().unwrap();
        v.history.push(out.kind, out.stack.clone());
        assert!(
            v.history.undo().is_some(),
            "the preset apply must be its own undo step, not merged into the prior one"
        );
        assert!(
            v.history.undo().is_some(),
            "the simulated prior edit must still be a separate step underneath it"
        );
    }

    fn local_adjustments_stack() -> ferrolite_pipeline::OpStack {
        let mut la = ferrolite_pipeline::LocalAdjustments::default();
        la.layers.push(ferrolite_pipeline::MaskLayer {
            name: "m".into(),
            visible: true,
            mask: Default::default(),
            adjustments: Default::default(),
        });
        ferrolite_pipeline::OpStack::default().set_op(Op::LocalAdjustments(la))
    }

    /// Sanity check that `EditPatch::apply_to` behaves the way this module
    /// assumes for the equality no-op guard above (belt and braces against a
    /// future change to `apply_to`'s semantics).
    #[test]
    fn empty_owns_patch_is_a_true_no_op() {
        let mut source = ferrolite_pipeline::EditDoc::default();
        source.global.exposure = 9.0;
        let patch = EditPatch::from_doc(&source, GroupSet::EMPTY);
        let target = ferrolite_pipeline::EditDoc::default();
        assert_eq!(patch.apply_to(&target), target);
    }
}
