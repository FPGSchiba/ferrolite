//! Reusable Library/Develop filter widgets, drawn with the `icons` helpers and
//! bound to `FilterState` fields. Shared by the Library toolbar and the Develop
//! filter strip so the two never duplicate widget logic.

use crate::library::icons;
use ferrolite_catalog::{SortKey, TagMode, TagRecord};
use ferrolite_image::{Flag, TagId};

/// Given the current value and the star index clicked (1-based), return the new
/// value: clicking the already-active value clears to 0, else sets to the clicked index.
pub fn star_value_clicked(current: u8, clicked: u8) -> u8 {
    if current == clicked {
        0
    } else {
        clicked
    }
}

const STAR_R: f32 = 5.0;
const STAR_GAP: f32 = 3.0;

/// Draw `max` clickable stars (first `current` filled, ACCENT; rest outlined, TEXT_FAINT).
/// Returns the new value if a star was clicked (active→0), else None.
pub fn clickable_stars(ui: &mut egui::Ui, current: u8, max: u8) -> Option<u8> {
    let width = icons::advance_width(STAR_R, STAR_GAP, max);
    let (rect, _resp) =
        ui.allocate_exact_size(egui::vec2(width, STAR_R * 2.0 + 4.0), egui::Sense::hover());
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let clicked_now = ui.input(|i| i.pointer.primary_clicked());
    let cell = STAR_R * 2.0 + STAR_GAP;
    let mut result = None;
    for n in 1..=max {
        let cx = rect.left() + STAR_R + (n as f32 - 1.0) * cell;
        let center = egui::pos2(cx, rect.center().y);
        let filled = n <= current;
        let color = if filled {
            crate::theme::ACCENT
        } else {
            crate::theme::TEXT_FAINT
        };
        icons::star(ui.painter(), center, STAR_R, filled, color);
        let hit = egui::Rect::from_center_size(center, egui::vec2(cell, rect.height()));
        if clicked_now && pointer.map(|p| hit.contains(p)).unwrap_or(false) {
            result = Some(star_value_clicked(current, n));
        }
    }
    result
}

/// Rating-threshold control bound to `FilterState.min_rating`.
pub fn rating_threshold(ui: &mut egui::Ui, min_rating: &mut u8) -> bool {
    if let Some(v) = clickable_stars(ui, *min_rating, 5) {
        *min_rating = v;
        true
    } else {
        false
    }
}

/// Flag-filter toggles (Pick green, Reject red); filled when active.
pub fn flag_filters(ui: &mut egui::Ui, flags: &mut Vec<Flag>) -> bool {
    let mut changed = false;
    for (f, color) in [
        (Flag::Pick, crate::theme::SEMANTIC_GREEN),
        (Flag::Reject, crate::theme::SEMANTIC_RED),
    ] {
        let active = flags.contains(&f);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
        if active {
            ui.painter()
                .rect_filled(rect, 2.0, crate::theme::ACCENT_BG_SEL);
        }
        icons::flag(
            ui.painter(),
            rect.center() + egui::vec2(0.0, 4.0),
            11.0,
            active,
            color,
        );
        if resp.clicked() {
            if let Some(p) = flags.iter().position(|x| *x == f) {
                flags.remove(p);
            } else {
                flags.push(f);
            }
            changed = true;
        }
    }
    changed
}

/// Tag multi-select dropdown with Any/All mode.
pub fn tag_filter_dropdown(
    ui: &mut egui::Ui,
    tag_ids: &mut Vec<TagId>,
    mode: &mut TagMode,
    tags: &[TagRecord],
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt("tagfilter")
        .selected_text(format!("Tags ({})", tag_ids.len()))
        .show_ui(ui, |ui| {
            let all = matches!(mode, TagMode::All);
            if ui.selectable_label(!all, "Any").clicked() {
                *mode = TagMode::Any;
                changed = true;
            }
            if ui.selectable_label(all, "All").clicked() {
                *mode = TagMode::All;
                changed = true;
            }
            ui.separator();
            for t in tags {
                let mut on = tag_ids.contains(&t.id);
                if ui.checkbox(&mut on, &t.name).changed() {
                    if let Some(p) = tag_ids.iter().position(|x| *x == t.id) {
                        tag_ids.remove(p);
                    } else {
                        tag_ids.push(t.id);
                    }
                    changed = true;
                }
            }
        });
    changed
}

/// Sort-key combo + ascending/descending caret.
pub fn sort_controls(ui: &mut egui::Ui, key: &mut SortKey, desc: &mut bool) -> bool {
    let mut changed = false;
    let label = |k: SortKey| match k {
        SortKey::CaptureTime => "Capture Time",
        SortKey::Filename => "Filename",
        SortKey::Rating => "Rating",
        SortKey::AddedAt => "Date Added",
    };
    egui::ComboBox::from_id_salt("sort")
        .selected_text(label(*key))
        .show_ui(ui, |ui| {
            for k in [
                SortKey::CaptureTime,
                SortKey::Filename,
                SortKey::Rating,
                SortKey::AddedAt,
            ] {
                if ui.selectable_value(key, k, label(k)).clicked() {
                    changed = true;
                }
            }
        });
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    icons::caret(
        ui.painter(),
        rect.center(),
        4.0,
        crate::theme::TEXT_PRIMARY,
        *desc,
    );
    if resp.clicked() {
        *desc = !*desc;
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_click_sets_or_clears() {
        assert_eq!(star_value_clicked(0, 3), 3); // set
        assert_eq!(star_value_clicked(3, 3), 0); // clicking active clears
        assert_eq!(star_value_clicked(2, 5), 5); // change
        assert_eq!(star_value_clicked(5, 1), 1); // lower
    }
}
