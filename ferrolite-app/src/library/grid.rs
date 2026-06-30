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

pub fn show(ui: &mut egui::Ui, state: &mut AppState, cell: f32) {
    let avail_w = ui.available_width();
    let m = metrics(avail_w, cell, GAP);
    let item_count = state.images.len();
    let total_rows = item_count.div_ceil(m.columns.max(1));
    let total_height = total_rows as f32 * m.row_height;

    let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
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
        state.last_visible = now_visible;

        for idx in range {
            let rec = state.images[idx].clone();
            let row = idx / m.columns;
            let col = idx % m.columns;
            // col stride equals row_height because cells are square (cell+GAP == row_height).
            let x = ui.min_rect().left() + col as f32 * m.row_height;
            let y = ui.min_rect().top() + row as f32 * m.row_height;
            let rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cell, cell));
            paint_cell(ui, state, &rec, rect);
        }
    });
    let _ = out;
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
) {
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

    // Selection: single click selects; double-click opens the viewer.
    let resp = ui.interact(rect, ui.id().with(("cell", rec.id)), egui::Sense::click());
    if resp.clicked() {
        state.selected = Some(rec.id);
    }
    if resp.double_clicked() {
        if let Ok(Some(folder_path)) = state.reads.folder_path(rec.folder_id) {
            let path = std::path::PathBuf::from(folder_path).join(&rec.filename);
            // Cancel the previously-open viewer's in-flight decode jobs before
            // replacing it; its sparse VT tile jobs are cancelled in `app.rs`
            // once the holder is superseded (it needs the GPU render state).
            if let Some(old) = state.viewer.as_ref() {
                old.cancel_loads();
            }
            state.viewer = Some(crate::viewer::ViewerState::open(rec.id, path, rec.kind));
        }
    }
    if state.selected == Some(rec.id) {
        painter.rect_stroke(rect, 2.0, egui::Stroke::new(2.0, theme::ACCENT));
    }
}
