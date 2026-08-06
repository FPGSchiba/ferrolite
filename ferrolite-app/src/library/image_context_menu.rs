//! Reusable right-click metadata menu for a single image (Rating / Flag / Tags /
//! Add-to-collection). Shared by the grid, the Develop filmstrip, and the loupe.

use crate::library::filter::ViewSource;
use crate::metadata::MetaEdit;
use crate::state::AppState;
use ferrolite_image::{Flag, Rating};

/// Selection scoping for context-menu actions: if the right-clicked image is
/// part of the current grid multi-selection, act on the whole selection; else
/// act on just that image. `single_image` (loupe/filmstrip) always scopes to
/// the one image. Returns a sorted id list (stable, dedup'd via the set).
pub fn regen_target_ids(
    single_image: bool,
    right_clicked: i64,
    selection: &std::collections::HashSet<i64>,
) -> Vec<i64> {
    if !single_image && selection.contains(&right_clicked) {
        let mut ids: Vec<i64> = selection.iter().copied().collect();
        ids.sort_unstable();
        ids
    } else {
        vec![right_clicked]
    }
}

/// Render the menu for `image_id` inside a `context_menu` closure.
///
/// `single_image` controls scoping:
/// - `true` — always edit `image_id` only (Develop loupe and filmstrip).
/// - `false` — if `image_id` is in the current multi-selection, edit all selected
///   images; otherwise edit `image_id` only (grid).
pub fn show(ui: &mut egui::Ui, state: &mut AppState, image_id: i64, single_image: bool) {
    let ctx = ui.ctx().clone();
    // When single_image is true we always route through the single-image path,
    // ignoring whatever selection the grid may have left in state.
    let use_selection = !single_image && state.selection.contains(&image_id);
    let tags = state.tags.clone();
    let collections = state.collections.clone();
    let image_tags = state
        .visible_tags
        .get(&image_id)
        .cloned()
        .unwrap_or_default();

    // Helper: apply to the multi-selection or just this image depending on scope.
    let apply = |state: &mut AppState, edit: MetaEdit| {
        if use_selection {
            state.apply_metadata_edit(&ctx, edit);
        } else {
            state.apply_metadata_edit_to_image(&ctx, image_id, edit);
        }
    };

    ui.menu_button("Rating", |ui| {
        if ui.button("No rating").clicked() {
            apply(state, MetaEdit::SetRating(Rating::new(0)));
            ui.close_menu();
        }
        for n in 1u8..=5 {
            let label = format!("{n} star{}", if n == 1 { "" } else { "s" });
            if ui.button(label).clicked() {
                apply(state, MetaEdit::SetRating(Rating::new(n)));
                ui.close_menu();
            }
        }
    });
    ui.menu_button("Flag", |ui| {
        if ui.button("Pick").clicked() {
            apply(state, MetaEdit::SetFlag(Flag::Pick));
            ui.close_menu();
        }
        if ui.button("Reject").clicked() {
            apply(state, MetaEdit::SetFlag(Flag::Reject));
            ui.close_menu();
        }
        if ui.button("Unflag").clicked() {
            apply(state, MetaEdit::SetFlag(Flag::None));
            ui.close_menu();
        }
    });
    if !tags.is_empty() {
        ui.menu_button("Tags", |ui| {
            for t in &tags {
                let has = image_tags.contains(&t.id);
                if ui.selectable_label(has, &t.name).clicked() {
                    apply(state, MetaEdit::ToggleTag(t.id));
                    ui.close_menu();
                }
            }
        });
    }
    if !collections.is_empty() {
        let target_ids: Vec<i64> = if use_selection {
            let mut v: Vec<i64> = state.selection.iter().copied().collect();
            v.sort_unstable();
            v
        } else {
            vec![image_id]
        };
        let membership = state.visible_collections.clone();
        let addable = crate::library::collection_menu::addable_collections(
            &collections,
            &target_ids,
            &membership,
        );
        let removable = crate::library::collection_menu::removable_collections(
            &collections,
            &target_ids,
            &membership,
        );

        if !addable.is_empty() {
            ui.menu_button("Add to collection", |ui| {
                for c in collections.iter().filter(|c| addable.contains(&c.id)) {
                    if ui.button(&c.name).clicked() {
                        if use_selection {
                            state.add_selection_to_collection(c.id);
                        } else {
                            state.add_image_to_collection_now(image_id, c.id);
                        }
                        ui.close_menu();
                    }
                }
            });
        }

        if !removable.is_empty() {
            ui.menu_button("Remove from collection", |ui| {
                for c in collections.iter().filter(|c| removable.contains(&c.id)) {
                    if ui.button(&c.name).clicked() {
                        if use_selection {
                            state.remove_selection_from_collection(c.id);
                        } else {
                            state.remove_image_from_collection_now(image_id, c.id);
                        }
                        ui.close_menu();
                    }
                }
            });
        }
    }

    if let ViewSource::Collection(coll_id) = state.source {
        if ui.button("Remove from this collection").clicked() {
            if use_selection {
                state.remove_selection_from_collection(coll_id);
            } else {
                state.remove_image_from_collection_now(image_id, coll_id);
            }
            ui.close_menu();
        }
    }

    ui.separator();
    if ui.button("Add to export queue").clicked() {
        if use_selection {
            let ids: Vec<i64> = state.selection.iter().copied().collect();
            let n = ids.len();
            state.queue_add_many(&ids);
            state.notify(
                crate::notifications::Level::Info,
                format!("Added {n} to export queue."),
            );
        } else {
            state.queue_add(image_id);
            state.notify(crate::notifications::Level::Info, "Added to export queue.");
        }
        ui.close_menu();
    }

    if ui.button("Regenerate thumbnail").clicked() {
        let ids = regen_target_ids(single_image, image_id, &state.selection);
        let n = ids.len();
        state.pending_thumb_regen.extend(ids);
        state.notify(
            crate::notifications::Level::Info,
            if n == 1 {
                "Regenerating thumbnail…".to_string()
            } else {
                format!("Regenerating {n} thumbnails…")
            },
        );
        ui.close_menu();
    }

    show_edit_settings_items(ui, state, image_id, single_image, &ctx);
}

/// The P7 copy / paste / preset block. Split out of `show` purely to keep both
/// functions inside the house size limit; it is one continuous menu section.
///
/// Copy and Save act on the RIGHT-CLICKED image alone (they read one source
/// document). Paste and Apply preset are multi-image actions and therefore go
/// through `regen_target_ids`, exactly like "Regenerate thumbnail" above — a
/// user right-clicking an unselected image must not silently hit their whole
/// selection.
fn show_edit_settings_items(
    ui: &mut egui::Ui,
    state: &mut AppState,
    image_id: i64,
    single_image: bool,
    ctx: &egui::Context,
) {
    ui.separator();

    // `has_edits` is the catalog's cache of "has a non-identity edit stack" —
    // enough to grey the item, and it costs no sidecar read to consult.
    let source_has_edits = state
        .images
        .iter()
        .find(|r| r.id == image_id)
        .is_some_and(|r| r.has_edits);

    let copy = ui.add_enabled(
        source_has_edits,
        egui::Button::new(format!("{} Copy settings", crate::icons::COPY_SETTINGS)),
    );
    if !source_has_edits {
        copy.on_disabled_hover_text("This image has no edits to copy");
    } else if copy.clicked() {
        crate::presets::menu::start_copy(state, ctx, image_id);
        ui.close_menu();
    }

    let has_clipboard = state.clipboard_patch.is_some();
    let paste = ui.add_enabled(
        has_clipboard,
        egui::Button::new(format!("{} Paste settings…", crate::icons::PASTE_SETTINGS)),
    );
    if !has_clipboard {
        paste.on_disabled_hover_text("Copy settings from an image first");
    } else if paste.clicked() {
        let ids = regen_target_ids(single_image, image_id, &state.selection);
        crate::presets::menu::start_paste(state, &ids);
        ui.close_menu();
    }

    // Applying a preset opens NO dialog — a preset already declares its own
    // groups (design §6.3) — so this is a plain submenu of one entry each.
    let preset_names: Vec<String> = state.presets.iter().map(|p| p.name.clone()).collect();
    let mut chosen: Option<usize> = None;
    ui.add_enabled_ui(!preset_names.is_empty(), |ui| {
        ui.menu_button(format!("{} Apply preset", crate::icons::PRESET), |ui| {
            for (i, name) in preset_names.iter().enumerate() {
                if ui.button(name).clicked() {
                    chosen = Some(i);
                    ui.close_menu();
                }
            }
        });
    })
    .response
    .on_disabled_hover_text("Save a preset first");
    if let Some(index) = chosen {
        let ids = regen_target_ids(single_image, image_id, &state.selection);
        crate::presets::menu::apply_preset(state, ctx, index, &ids);
        ui.close_menu();
    }

    let save = ui.add_enabled(
        source_has_edits,
        egui::Button::new(format!(
            "{} Save preset from this image…",
            crate::icons::PRESET
        )),
    );
    if !source_has_edits {
        save.on_disabled_hover_text("This image has no edits to save");
    } else if save.clicked() {
        crate::presets::menu::start_save_preset(state, ctx, image_id);
        ui.close_menu();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// `single_image = true` must never activate the multi-select path, even when
    /// `image_id` is present in a stale grid multi-selection.  Mirrors the
    /// `use_selection` computation inside `show`.
    #[test]
    fn single_image_ignores_selection() {
        let image_id: i64 = 42;
        let mut selection = std::collections::HashSet::new();
        selection.insert(image_id); // stale multi-select

        // Mirrors the formula in `show`: use_selection = !single_image && selection.contains(&id)
        let compute_use_selection =
            |single_image: bool| -> bool { !single_image && selection.contains(&image_id) };

        assert!(
            !compute_use_selection(true),
            "single_image=true must not use the selection even when image_id is selected"
        );
        assert!(
            compute_use_selection(false),
            "single_image=false with image_id in selection should use multi-select path"
        );
    }

    #[test]
    fn single_image_mode_targets_only_that_image() {
        let sel: HashSet<i64> = [1, 2, 3].into_iter().collect();
        assert_eq!(regen_target_ids(true, 2, &sel), vec![2]);
    }

    #[test]
    fn grid_right_click_inside_selection_targets_whole_selection() {
        let sel: HashSet<i64> = [1, 2, 3].into_iter().collect();
        assert_eq!(regen_target_ids(false, 2, &sel), vec![1, 2, 3]);
    }

    #[test]
    fn grid_right_click_outside_selection_targets_only_that_image() {
        let sel: HashSet<i64> = [1, 2, 3].into_iter().collect();
        assert_eq!(regen_target_ids(false, 9, &sel), vec![9]);
    }

    #[test]
    fn grid_right_click_with_empty_selection_targets_only_that_image() {
        let sel: HashSet<i64> = HashSet::new();
        assert_eq!(regen_target_ids(false, 5, &sel), vec![5]);
    }
}
