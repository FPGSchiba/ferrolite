//! Library top toolbar: search, sort, rating/flag/tag filters, and the
//! thumbnail-size slider pinned to the right.  All widgets drive `state.filter`
//! and `state.include_subfolders` directly; the caller sets `state.dirty` when
//! the returned `changed` flag is true (so the read pool re-queries off-thread).

use crate::state::AppState;
use crate::widgets::EguiSlider;
use ferrolite_catalog::{SortKey, TagMode};
use ferrolite_image::Flag;

/// Width of the thumbnail-size slider's box on the right.
const SIZE_SLIDER_W: f32 = 208.0;

/// Returns `true` if any filter/sort/source field changed this frame.
pub fn show(ui: &mut egui::Ui, thumb_size: &mut f32, state: &mut AppState) -> bool {
    let mut changed = false;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;

        // Search (debounced upstream by the dirty flag; query runs off-thread).
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.filter.search)
                .hint_text("Search filename or tag…")
                .desired_width(206.0),
        );
        if resp.changed() {
            changed = true;
        }

        // Sort key + direction.
        egui::ComboBox::from_id_salt("sort")
            .selected_text(sort_label(state.filter.sort_key))
            .show_ui(ui, |ui| {
                for (k, lbl) in [
                    (SortKey::CaptureTime, "Capture Time"),
                    (SortKey::Filename, "Filename"),
                    (SortKey::Rating, "Rating"),
                    (SortKey::AddedAt, "Date Added"),
                ] {
                    if ui
                        .selectable_value(&mut state.filter.sort_key, k, lbl)
                        .clicked()
                    {
                        changed = true;
                    }
                }
            });
        if ui
            .button(if state.filter.sort_desc { "▼" } else { "▲" })
            .clicked()
        {
            state.filter.sort_desc = !state.filter.sort_desc;
            changed = true;
        }

        // Rating threshold: click star N to require ≥N; click the active one to clear.
        for n in 1..=5u8 {
            let on = state.filter.min_rating >= n;
            let star = if on { "★" } else { "☆" };
            if ui.small_button(star).clicked() {
                state.filter.min_rating = if state.filter.min_rating == n { 0 } else { n };
                changed = true;
            }
        }

        // Flag filter toggles.
        for (f, lbl) in [(Flag::Pick, "⚑"), (Flag::Reject, "⚐")] {
            let on = state.filter.flags.contains(&f);
            if ui.selectable_label(on, lbl).clicked() {
                toggle_flag(&mut state.filter.flags, f);
                changed = true;
            }
        }

        // Tag filter dropdown (multi-select over the global vocabulary) + Any/All.
        egui::ComboBox::from_id_salt("tagfilter")
            .selected_text(format!("Tags ({})", state.filter.tag_ids.len()))
            .show_ui(ui, |ui| {
                let mode_all = matches!(state.filter.tag_mode, TagMode::All);
                if ui.selectable_label(!mode_all, "Any").clicked() {
                    state.filter.tag_mode = TagMode::Any;
                    changed = true;
                }
                if ui.selectable_label(mode_all, "All").clicked() {
                    state.filter.tag_mode = TagMode::All;
                    changed = true;
                }
                ui.separator();
                for t in &state.tags {
                    let mut on = state.filter.tag_ids.contains(&t.id);
                    if ui.checkbox(&mut on, &t.name).changed() {
                        toggle_tag(&mut state.filter.tag_ids, t.id);
                        changed = true;
                    }
                }
            });

        if ui
            .checkbox(&mut state.include_subfolders, "Subfolders")
            .changed()
        {
            changed = true;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(SIZE_SLIDER_W, ui.available_height()),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add(EguiSlider {
                        label: "Size",
                        value: thumb_size,
                        min: 0.0,
                        max: 100.0,
                        default: 46.0,
                        step: 1.0,
                        decimals: 0,
                        unit: "",
                        bipolar: false,
                        signed: false,
                    });
                },
            );
        });
    });
    changed
}

fn sort_label(k: SortKey) -> &'static str {
    match k {
        SortKey::CaptureTime => "Capture Time",
        SortKey::Filename => "Filename",
        SortKey::Rating => "Rating",
        SortKey::AddedAt => "Date Added",
    }
}

fn toggle_flag(flags: &mut Vec<Flag>, f: Flag) {
    if let Some(p) = flags.iter().position(|x| *x == f) {
        flags.remove(p);
    } else {
        flags.push(f);
    }
}

fn toggle_tag(ids: &mut Vec<ferrolite_image::TagId>, id: ferrolite_image::TagId) {
    if let Some(p) = ids.iter().position(|x| *x == id) {
        ids.remove(p);
    } else {
        ids.push(id);
    }
}
