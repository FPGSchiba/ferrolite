//! Bottom Develop bar: rate / flag / tag / add-to-collection the OPEN image.
//! Fast cull-while-viewing; current-image only.

use crate::library::{filter_widgets, icons};
use crate::metadata::MetaEdit;
use crate::state::AppState;
use ferrolite_image::{Flag, Rating};

pub fn show(ui: &mut egui::Ui, state: &mut AppState, ctx: &egui::Context, image_id: i64) {
    // Read the open image's current rating/flag from its in-memory row.
    let (cur_rating, cur_flag) = state
        .images
        .iter()
        .find(|r| r.id == image_id)
        .map(|r| (r.rating.get(), r.flag))
        .unwrap_or((0, Flag::None));
    let image_tags = state
        .visible_tags
        .get(&image_id)
        .cloned()
        .unwrap_or_default();

    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;

        // Rating: clickable stars (set N / clear on re-click).
        if let Some(v) = filter_widgets::clickable_stars(ui, cur_rating, 5) {
            state.apply_metadata_edit_to_image(ctx, image_id, MetaEdit::SetRating(Rating::new(v)));
        }

        // Flag: Pick / Reject toggle buttons.
        for (f, color, label) in [
            (Flag::Pick, crate::theme::SEMANTIC_GREEN, "Pick"),
            (Flag::Reject, crate::theme::SEMANTIC_RED, "Reject"),
        ] {
            let active = cur_flag == f;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
            if active {
                ui.painter()
                    .rect_filled(rect, 2.0, crate::theme::ACCENT_BG_SEL);
            }
            icons::flag(
                ui.painter(),
                rect.center() + egui::vec2(0.0, 5.0),
                12.0,
                active,
                color,
                false,
            );
            if resp.on_hover_text(label).clicked() {
                let new = if active { Flag::None } else { f };
                state.apply_metadata_edit_to_image(ctx, image_id, MetaEdit::SetFlag(new));
            }
        }

        // Tags dropdown: toggle tags on the open image.
        let tags = state.tags.clone();
        egui::ComboBox::from_id_salt("develop_tags")
            .selected_text("Tags")
            .show_ui(ui, |ui| {
                for t in &tags {
                    let has = image_tags.contains(&t.id);
                    if ui.selectable_label(has, &t.name).clicked() {
                        state.apply_metadata_edit_to_image(
                            ctx,
                            image_id,
                            MetaEdit::ToggleTag(t.id),
                        );
                    }
                }
            });

        // Add to collection.
        let collections = state.collections.clone();
        if !collections.is_empty() {
            ui.menu_button("Add to collection", |ui| {
                for c in &collections {
                    if ui.button(&c.name).clicked() {
                        state.add_image_to_collection_now(image_id, c.id);
                        ui.close_menu();
                    }
                }
            });
        }
    });
}
