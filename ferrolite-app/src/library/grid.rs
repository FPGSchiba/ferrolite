//! Virtualized thumbnail grid. Realizes only the visible window of cells, pulls
//! ready thumbnails from the read pool on demand, and promotes the visible
//! window's pending thumbnail jobs to `Visible` priority.

use crate::library::cell_state::{cell_state, CellState};
use crate::library::grid_layout::{metrics, visible_items};
use crate::state::AppState;
use crate::theme;
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

    // Rating stars (bottom-left).
    if rec.rating.get() > 0 {
        let stars: String = "★".repeat(rec.rating.get() as usize);
        painter.text(
            rect.left_bottom() + egui::vec2(4.0, -4.0),
            egui::Align2::LEFT_BOTTOM,
            stars,
            egui::FontId::proportional(11.0),
            theme::ACCENT,
        );
    }
    // Flag glyph (top-left).
    let flag_glyph = match rec.flag {
        ferrolite_image::Flag::Pick => Some(("⚑", theme::SEMANTIC_GREEN)),
        ferrolite_image::Flag::Reject => Some(("⚐", theme::SEMANTIC_RED)),
        ferrolite_image::Flag::None => None,
    };
    if let Some((g, col)) = flag_glyph {
        painter.text(
            rect.left_top() + egui::vec2(4.0, 4.0),
            egui::Align2::LEFT_TOP,
            g,
            egui::FontId::proportional(12.0),
            col,
        );
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

    // Selection: ctrl/cmd-click toggles; plain click replaces; double-click opens.
    let resp = ui.interact(rect, ui.id().with(("cell", rec.id)), egui::Sense::click());
    if resp.clicked() {
        let multi = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
        if multi {
            if !state.selection.remove(&rec.id) {
                state.selection.insert(rec.id);
            }
        } else {
            state.selection.clear();
            state.selection.insert(rec.id);
        }
        state.selected = Some(rec.id);
    }
    let mut opened = None;
    if resp.double_clicked() {
        opened = Some(rec.id);
    }
    if state.selection.contains(&rec.id) || state.selected == Some(rec.id) {
        painter.rect_stroke(rect, 2.0, egui::Stroke::new(2.0, theme::ACCENT));
    }
    opened
}
