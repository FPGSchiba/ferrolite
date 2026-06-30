//! Virtualized thumbnail grid. Realizes only the visible window of cells, pulls
//! ready thumbnails from the read pool on demand, and promotes the visible
//! window's pending thumbnail jobs to `Visible` priority.

use crate::library::cell_state::{cell_state, CellState};
use crate::library::grid_layout::{metrics, visible_items};
use crate::library::icons;
use crate::metadata::MetaEdit;
use crate::state::AppState;
use crate::theme;
use ferrolite_image::{Flag, Rating};
use ferrolite_jobs::Priority;
use std::collections::HashSet;

const GAP: f32 = 8.0;

pub fn show(ui: &mut egui::Ui, state: &mut AppState, cell: f32) -> Option<i64> {
    let avail_w = ui.available_width();
    let m = metrics(avail_w, cell, GAP);
    let item_count = state.images.len();
    let total_rows = item_count.div_ceil(m.columns.max(1));
    let total_height = total_rows as f32 * m.row_height;

    let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
    let mut opened: Option<i64> = None;
    let out = scroll.show_viewport(ui, |ui, viewport| {
        ui.set_height(total_height);
        let scroll_top = viewport.min.y.max(0.0);
        let vh = viewport.height();
        let range = visible_items(scroll_top, vh, &m, item_count);

        // Promote visible pending thumbnail jobs; demote ones that scrolled away.
        // Compute visible set immutably first (borrow checker: reprioritize borrows
        // state immutably; the mut paint loop comes after).
        let mut now_visible: HashSet<i64> = HashSet::new();
        for idx in range.clone() {
            now_visible.insert(state.images[idx].id);
        }
        reprioritize(state, &now_visible);
        // Fetch tag associations for the visible window (only missing ids queried).
        state.ensure_tags_for(&now_visible);
        state.last_visible = now_visible;

        for idx in range {
            let rec = state.images[idx].clone();
            let row = idx / m.columns;
            let col = idx % m.columns;
            // col stride equals row_height because cells are square (cell+GAP == row_height).
            let x = ui.min_rect().left() + col as f32 * m.row_height;
            let y = ui.min_rect().top() + row as f32 * m.row_height;
            let rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cell, cell));
            if let Some(id) = paint_cell(ui, state, &rec, rect) {
                opened = Some(id);
            }
        }
    });
    let _ = out;
    opened
}

fn reprioritize(state: &AppState, now_visible: &HashSet<i64>) {
    for id in now_visible.difference(&state.last_visible) {
        if let Some(job_id) = state.thumb_jobs.get(id) {
            state.jobs.reprioritize(*job_id, Priority::Visible);
        }
    }
    for id in state.last_visible.difference(now_visible) {
        if let Some(job_id) = state.thumb_jobs.get(id) {
            state.jobs.reprioritize(*job_id, Priority::Background);
        }
    }
}

/// Compute the inclusive range of image indices between `anchor_idx` and
/// `target_idx` (order-independent). Returns both endpoints and all in between.
pub fn range_indices(anchor_idx: usize, target_idx: usize) -> std::ops::RangeInclusive<usize> {
    anchor_idx.min(target_idx)..=anchor_idx.max(target_idx)
}

fn paint_cell(
    ui: &mut egui::Ui,
    state: &mut AppState,
    rec: &ferrolite_catalog::ImageRecord,
    rect: egui::Rect,
) -> Option<i64> {
    // Pull a ready thumbnail from the pool on demand if not yet cached.
    if !state.textures.contains(rec.id)
        && rec.decode_status != ferrolite_catalog::DecodeStatus::Failed
    {
        if let Ok(Some(thumb)) = state.reads.get_thumbnail(rec.id) {
            let jpeg = thumb.bytes;
            state.upload_thumbnail(ui.ctx(), rec.id, jpeg);
        }
    }
    let has_tex = state.textures.contains(rec.id);
    let painter = ui.painter_at(rect);
    match cell_state(rec, has_tex) {
        CellState::Ready => {
            if let Some(tex) = state.textures.get(rec.id) {
                let img = egui::Image::new(tex).fit_to_exact_size(rect.size());
                img.paint_at(ui, rect);
            }
        }
        CellState::Placeholder => {
            painter.rect_filled(rect, 2.0, theme::BG_PANEL);
        }
        CellState::Failed => {
            painter.rect_filled(rect, 2.0, theme::BG_PANEL);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "broken",
                egui::FontId::proportional(11.0),
                theme::SEMANTIC_RED,
            );
        }
    }

    // #8 — Rating stars (bottom-left): drawn shapes instead of glyphs.
    if rec.rating.get() > 0 {
        // origin = left-centre of the star row, sitting 8px above the bottom edge.
        let r = 4.0_f32;
        let gap = 2.0_f32;
        let row_y = rect.bottom() - 8.0;
        let origin = egui::pos2(rect.left() + 4.0 + r, row_y);
        icons::rating_stars(&painter, origin, r, gap, rec.rating.get(), 5, theme::ACCENT);
    }

    // #8 — Flag icon (top-left): drawn shapes instead of glyphs.
    match rec.flag {
        Flag::Pick => {
            icons::flag(
                &painter,
                egui::pos2(rect.left() + 6.0, rect.top() + 12.0),
                10.0,
                true,
                theme::SEMANTIC_GREEN,
            );
        }
        Flag::Reject => {
            icons::flag(
                &painter,
                egui::pos2(rect.left() + 6.0, rect.top() + 12.0),
                10.0,
                true,
                theme::SEMANTIC_RED,
            );
        }
        Flag::None => {}
    }

    // Tag colour dots (bottom-right), looked up from the loaded vocabulary.
    if let Some(tag_ids) = state.visible_tags.get(&rec.id) {
        let mut x = rect.right() - 8.0;
        for tid in tag_ids.iter().take(5) {
            if let Some(t) = state.tags.iter().find(|t| t.id == *tid) {
                let c = egui::Color32::from_rgb(t.color.r, t.color.g, t.color.b);
                painter.circle_filled(egui::pos2(x, rect.bottom() - 8.0), 4.0, c);
                x -= 11.0;
            }
        }
    }

    // Selection: ctrl/cmd-click toggles; shift-click range-select; plain click replaces.
    // Context menu on right-click.
    let resp = ui.interact(rect, ui.id().with(("cell", rec.id)), egui::Sense::click());
    if resp.clicked() {
        let (shift, multi) =
            ui.input(|i| (i.modifiers.shift, i.modifiers.command || i.modifiers.ctrl));
        if shift {
            // Range select: find anchor index (anchor → selected → this image).
            let anchor_id = state.selection_anchor.or(state.selected).unwrap_or(rec.id);
            let anchor_idx = state
                .images
                .iter()
                .position(|r| r.id == anchor_id)
                .unwrap_or(0);
            let target_idx = state
                .images
                .iter()
                .position(|r| r.id == rec.id)
                .unwrap_or(anchor_idx);
            state.selection = range_indices(anchor_idx, target_idx)
                .map(|i| state.images[i].id)
                .collect();
            // Anchor does not move on shift-click.
            state.selected = Some(rec.id);
        } else if multi {
            if !state.selection.remove(&rec.id) {
                state.selection.insert(rec.id);
            }
            state.selection_anchor = Some(rec.id);
            state.selected = Some(rec.id);
        } else {
            state.selection.clear();
            state.selection.insert(rec.id);
            state.selection_anchor = Some(rec.id);
            state.selected = Some(rec.id);
        }
    }
    let mut opened = None;
    if resp.double_clicked() {
        opened = Some(rec.id);
    }

    // #7 — Selection highlight: a clean blue outline (Lightroom feel), no fill.
    // Inset by 1px so the full stroke sits inside the cell rather than being
    // clipped at the thumbnail edge.
    if state.selection.contains(&rec.id) || state.selected == Some(rec.id) {
        painter.rect_stroke(
            rect.shrink(1.0),
            2.0,
            egui::Stroke::new(2.0, theme::ACCENT_BRIGHT),
        );
    }

    // #5 — Right-click context menu.
    // Clone small vecs before the closure to avoid simultaneous borrow of `state`.
    let tags_snapshot = state.tags.clone();
    let collections_snapshot = state.collections.clone();
    let image_tags = state.visible_tags.get(&rec.id).cloned().unwrap_or_default();
    let rec_id = rec.id;

    resp.context_menu(|ui| {
        // Scope: if the right-clicked image is not in the selection, restrict
        // the operation to just this image so apply_metadata_edit targets correctly.
        if !state.selection.contains(&rec_id) {
            state.selection.clear();
            state.selection.insert(rec_id);
            state.selected = Some(rec_id);
        }

        ui.menu_button("Rating", |ui| {
            if ui.button("No rating").clicked() {
                state.apply_metadata_edit(ui.ctx(), MetaEdit::SetRating(Rating::new(0)));
                ui.close_menu();
            }
            for n in 1u8..=5 {
                let label = format!("{n} star{}", if n == 1 { "" } else { "s" });
                if ui.button(label).clicked() {
                    state.apply_metadata_edit(ui.ctx(), MetaEdit::SetRating(Rating::new(n)));
                    ui.close_menu();
                }
            }
        });

        ui.menu_button("Flag", |ui| {
            if ui.button("Pick").clicked() {
                state.apply_metadata_edit(ui.ctx(), MetaEdit::SetFlag(Flag::Pick));
                ui.close_menu();
            }
            if ui.button("Reject").clicked() {
                state.apply_metadata_edit(ui.ctx(), MetaEdit::SetFlag(Flag::Reject));
                ui.close_menu();
            }
            if ui.button("Unflag").clicked() {
                state.apply_metadata_edit(ui.ctx(), MetaEdit::SetFlag(Flag::None));
                ui.close_menu();
            }
        });

        if !tags_snapshot.is_empty() {
            ui.menu_button("Tags", |ui| {
                for t in &tags_snapshot {
                    let has = image_tags.contains(&t.id);
                    if ui.selectable_label(has, &t.name).clicked() {
                        state.apply_metadata_edit(ui.ctx(), MetaEdit::ToggleTag(t.id));
                        ui.close_menu();
                    }
                }
            });
        }

        if !collections_snapshot.is_empty() {
            ui.menu_button("Add to collection", |ui| {
                for c in &collections_snapshot {
                    if ui.button(&c.name).clicked() {
                        state.add_selection_to_collection(c.id);
                        ui.close_menu();
                    }
                }
            });
        }
    });

    opened
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_indices_low_to_high() {
        let r: Vec<usize> = range_indices(2, 5).collect();
        assert_eq!(r, vec![2, 3, 4, 5]);
    }

    #[test]
    fn range_indices_high_to_low() {
        let r: Vec<usize> = range_indices(5, 2).collect();
        assert_eq!(r, vec![2, 3, 4, 5]);
    }

    #[test]
    fn range_indices_same_point() {
        let r: Vec<usize> = range_indices(3, 3).collect();
        assert_eq!(r, vec![3]);
    }
}
