//! The Export queue (spec §8.4): a wrapping grid of thumbnail preview cells
//! with reorder + remove controls, mirroring the Library grid's lazy
//! thumbnail path so queued images decode off the UI thread on demand.

use crate::library::icons;
use crate::state::AppState;
use crate::theme;

/// Fixed thumbnail CELL box (3:2) and layout constants. Every cell allocates
/// this exact box whatever the image's shape — the uniform footprint is what
/// keeps `horizontal_wrapped`'s rows aligned (see `CELL_H`). The image itself is
/// letterboxed INSIDE the box (`grid_layout::fit_size`), never stretched to it.
const THUMB_W: f32 = 132.0;
const THUMB_H: f32 = 88.0;
const CELL_GAP: f32 = 10.0;
const ICON_BTN: f32 = 18.0;
const ICON_R: f32 = 5.0;

/// Height of the single-line filename label below the thumbnail. A couple of
/// px of slack over the `.small()` line height so the label can never exceed
/// its budget and re-introduce cross-axis drift in `horizontal_wrapped`.
const CELL_LABEL_H: f32 = 16.0;
/// Intra-cell vertical gap between thumbnail/label/remove-row.
const CELL_PAD: f32 = 4.0;
/// Exact, uniform cell height: every queue cell allocates this same box
/// (thumb + 2 gaps + label + remove-row), so `horizontal_wrapped` never sees
/// a taller-than-allocated cell and cannot drift rows out of alignment.
const CELL_H: f32 = THUMB_H + CELL_PAD + CELL_LABEL_H + CELL_PAD + ICON_BTN;

/// DnD payload id for the export-queue drag source (see [`egui::DragAndDrop`]).
/// Kept a plain `i64` image id — a future Library drag-to-collections feature
/// can reuse this same `dnd_drag_source`/`DragAndDrop::take_payload` pattern
/// with a multi-id payload type; no shared infra is built ahead of that need.
fn drag_id(id: i64) -> egui::Id {
    egui::Id::new(("export_queue_cell", id))
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    if state.export_queue.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(
                egui::RichText::new("Queue is empty.\nAdd images from Library or Develop.")
                    .color(crate::theme::TEXT_FAINT),
            );
        });
        return;
    }

    let ids = state.export_queue.clone();
    let recs = state.reads.images_by_ids(&ids).unwrap_or_default();
    let rec_of =
        |id: i64| -> Option<&ferrolite_catalog::ImageRecord> { recs.iter().find(|r| r.id == id) };

    let running = state.batch_running();
    let mut do_remove: Option<i64> = None;
    let mut cell_rects: Vec<egui::Rect> = Vec::new();

    let grid_resp = egui::Frame::none()
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(CELL_GAP, CELL_GAP);
                ui.horizontal_wrapped(|ui| {
                    for (idx, &id) in ids.iter().enumerate() {
                        let rec = rec_of(id);
                        let filename = rec
                            .map(|r| r.filename.clone())
                            .unwrap_or_else(|| format!("#{id}"));

                        // Exact-size, explicit top-down allocation: every cell
                        // reports an identical (THUMB_W, CELL_H) footprint
                        // regardless of content, so `horizontal_wrapped`'s
                        // cross-axis cursor advances uniformly and rows stay
                        // aligned (no more per-cell overflow drift).
                        ui.allocate_ui_with_layout(
                            egui::vec2(THUMB_W, CELL_H),
                            egui::Layout::top_down(egui::Align::Center),
                            |ui| {
                                ui.set_min_size(egui::vec2(THUMB_W, CELL_H));
                                ui.spacing_mut().item_spacing.y = CELL_PAD;

                                // The thumbnail itself is the drag source (not the
                                // whole cell), so the ✕ button below stays a plain
                                // click target unaffected by the drag sense. While
                                // a batch export is running, dragging is disabled
                                // and the thumbnail is painted without a source.
                                let thumb_rect = if running {
                                    paint_thumb(ui, state, id, rec, idx)
                                } else {
                                    let resp = ui
                                        .dnd_drag_source(drag_id(id), id, |ui| {
                                            paint_thumb(ui, state, id, rec, idx)
                                        })
                                        .response;
                                    resp.rect
                                };
                                cell_rects.push(thumb_rect);

                                // Single-line, non-wrapping, truncated filename:
                                // removes the 2nd-line height variance that fed
                                // the staircase. Full name still on hover.
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&filename)
                                            .small()
                                            .color(theme::TEXT_DIM),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(&filename);

                                ui.horizontal(|ui| {
                                    let remove =
                                        icon_button(ui, ICON_BTN, !running, |p, c, col| {
                                            icons::cross(p, c, ICON_R, col);
                                        })
                                        .on_hover_text("Remove");
                                    if remove.clicked() && !running {
                                        do_remove = Some(id);
                                    }
                                });
                            },
                        );
                    }
                });
            });
        });

    if !running {
        draw_drop_indicator_and_handle_release(
            ui,
            grid_resp.response.rect,
            &cell_rects,
            &ids,
            state,
        );
    }

    if let Some(id) = do_remove {
        state.queue_remove(id);
    }
}

/// While a drag of our `i64` payload is active: if the pointer is over the
/// queue grid, paints a vertical insertion-indicator line at the computed
/// drop gap; on release, reorders the queue to that gap.
fn draw_drop_indicator_and_handle_release(
    ui: &egui::Ui,
    grid_rect: egui::Rect,
    cell_rects: &[egui::Rect],
    ids: &[i64],
    state: &mut AppState,
) {
    let Some(dragged_id) = egui::DragAndDrop::payload::<i64>(ui.ctx()) else {
        return;
    };
    let Some(pointer) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    if !grid_rect.contains(pointer) || cell_rects.is_empty() {
        return;
    }

    let target = compute_drop_index(pointer, cell_rects);

    // Vertical insertion-indicator line at the landing gap: left edge of the
    // target cell, or the right edge of the last cell when appending.
    let (x, y_top, y_bottom) = if target < cell_rects.len() {
        let r = cell_rects[target];
        (r.left(), r.top(), r.bottom())
    } else {
        let r = cell_rects[cell_rects.len() - 1];
        (r.right(), r.top(), r.bottom())
    };
    ui.painter().line_segment(
        [egui::pos2(x, y_top), egui::pos2(x, y_bottom)],
        egui::Stroke::new(2.0_f32, theme::ACCENT),
    );

    let released = ui.input(|i| i.pointer.any_released());
    if released {
        if let Some(from) = ids.iter().position(|&id| id == *dragged_id) {
            state.queue_reorder(from, target);
        }
    }
}

/// Draws the thumbnail cell: requests the decode job lazily (same path as the
/// Library grid's `paint_cell`), then paints either the ready texture
/// (letterboxed to its own aspect inside the fixed cell box) or a filename
/// placeholder, plus the 1-based sequence badge.
///
/// Returns the full allocated CELL box, not the letterboxed image rect — the
/// drop-indicator geometry and `compute_drop_index` want uniform, gap-free cell
/// bounds, so a portrait's narrow image rect must not shrink its drop target.
fn paint_thumb(
    ui: &mut egui::Ui,
    state: &mut AppState,
    id: i64,
    rec: Option<&ferrolite_catalog::ImageRecord>,
    idx: usize,
) -> egui::Rect {
    let (rect, _resp) = ui.allocate_exact_size(egui::vec2(THUMB_W, THUMB_H), egui::Sense::hover());

    // Gated on `Done` (not just `!= Failed`), matching the Library grid's
    // `paint_cell` guard: a `Pending` row has no thumbnail blob yet, so
    // requesting one would submit a job that immediately finds nothing
    // (wasted one-shot lazy-load job on a cold `Pending` cell). `Done` implies
    // the blob is present.
    let ready_to_load = rec
        .map(|r| r.decode_status == ferrolite_catalog::DecodeStatus::Done)
        .unwrap_or(false);
    if !state.textures.contains(id) && ready_to_load {
        state.request_thumbnail(ui.ctx(), id);
    }

    // Letterbox the image inside the fixed 3:2 cell box instead of stretching
    // it to fill. The cell box must stay exactly `THUMB_W x THUMB_H` (the
    // wrapping layout depends on a uniform footprint), so the ASPECT has to be
    // absorbed by the painted rect. Handing the box straight to `paint_at`
    // squashed every non-3:2 image — a 2:3 portrait was stretched 2.25x
    // horizontally — and `Image::fit_to_exact_size` did NOT prevent it: `fit`
    // is only read by `Image::ui`/`calc_size`, so on the `paint_at` path it is
    // a silent no-op. See `grid_layout::fit_size`.
    //
    // The aspect comes from the shared `cell_aspect`, so the queue agrees with
    // the Library grid and the filmstrip: it prefers the persisted thumbnail's
    // own upright dims (already crop- and orientation-corrected) and only falls
    // back to sensor dims + orientation swap. Derived from the RECORD, not the
    // texture, so the cell does not reflow when the thumbnail finishes loading.
    // Placed by the SAME helper the Library grid uses, so the two grids agree on
    // both halves of the treatment: letterboxed to the image's own aspect, and
    // bottom-aligned in the cell so every caption sits `CELL_PAD` under its
    // thumbnail instead of a distance that varies with the image's shape.
    let aspect = rec
        .map(crate::library::grid::cell_aspect)
        .unwrap_or(THUMB_W / THUMB_H);
    let img_rect = crate::library::grid::cell_image_rect(rect, aspect);

    let painter = ui.painter_at(rect);
    // NO cell ground: the grid is deliberately invisible in both the Library and
    // here, so an image that does not fill its cell shows the panel behind it
    // rather than a lighter slot.
    if let Some(tex) = state.textures.get(id) {
        egui::Image::new(tex).paint_at(ui, img_rect);
    } else {
        let label = rec
            .map(|r| r.filename.clone())
            .unwrap_or_else(|| format!("#{id}"));
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(11.0),
            theme::TEXT_FAINT,
        );
    }

    // 1-based sequence index badge, top-left of the IMAGE (not the cell box), so
    // it sits on the thumbnail rather than floating over a letterbox bar.
    let badge_rect = egui::Rect::from_min_size(img_rect.left_top(), egui::vec2(20.0, 15.0));
    painter.rect_filled(badge_rect, 2.0, egui::Color32::from_black_alpha(140));
    painter.text(
        badge_rect.left_top() + egui::vec2(4.0, 2.0),
        egui::Align2::LEFT_TOP,
        format!("{}", idx + 1),
        egui::FontId::monospace(10.0),
        theme::TEXT_PRIMARY,
    );

    rect
}

/// Allocates a small clickable square and paints an icon into it via the
/// supplied `draw` closure, which receives the painter, the cell center, and
/// the resolved icon color (dimmed + hover-inert when `enabled` is false —
/// e.g. while a batch export is running).
fn icon_button(
    ui: &mut egui::Ui,
    size: f32,
    enabled: bool,
    draw: impl FnOnce(&egui::Painter, egui::Pos2, egui::Color32),
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    let hovered = enabled && resp.hovered();
    let color = if !enabled {
        theme::TEXT_FAINT
    } else if hovered {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_DIM
    };
    if hovered {
        ui.painter().rect_filled(rect, 3.0, theme::ACCENT_BG_SEL);
    }
    draw(&ui.painter_at(rect), rect.center(), color);
    resp
}

/// Given the drop `pointer` and the on-screen `cell_rects` (in queue order),
/// return the insertion index in `0..=cell_rects.len()`. Picks the cell whose
/// center is nearest the pointer, inserting before it if the pointer is left of
/// its center, else after. Empty → 0.
pub(crate) fn compute_drop_index(pointer: egui::Pos2, cell_rects: &[egui::Rect]) -> usize {
    if cell_rects.is_empty() {
        return 0;
    }
    let mut best = 0usize;
    let mut best_d = f32::INFINITY;
    for (i, r) in cell_rects.iter().enumerate() {
        let d = (r.center() - pointer).length_sq();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    if pointer.x < cell_rects[best].center().x {
        best
    } else {
        best + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2, Rect};

    /// Builds a 2-per-row grid of `n` cells, each 100x80 with a 10px gap,
    /// matching the wrapping thumbnail grid's layout shape.
    fn grid(n: usize) -> Vec<Rect> {
        const W: f32 = 100.0;
        const H: f32 = 80.0;
        const GAP: f32 = 10.0;
        const PER_ROW: usize = 2;
        (0..n)
            .map(|i| {
                let col = i % PER_ROW;
                let row = i / PER_ROW;
                let x = col as f32 * (W + GAP);
                let y = row as f32 * (H + GAP);
                Rect::from_min_size(pos2(x, y), vec2(W, H))
            })
            .collect()
    }

    #[test]
    fn empty_grid_drops_at_zero() {
        let rects: Vec<Rect> = Vec::new();
        assert_eq!(compute_drop_index(pos2(500.0, 500.0), &rects), 0);
    }

    #[test]
    fn single_row_left_of_first_center_is_zero() {
        let rects = grid(2);
        // cell0 center = (50, 40); pointer well left of it.
        assert_eq!(compute_drop_index(pos2(10.0, 40.0), &rects), 0);
    }

    #[test]
    fn single_row_right_of_last_center_is_len() {
        let rects = grid(2);
        // cell1 spans x in [110, 210), center = (160, 40); pointer right of it.
        assert_eq!(compute_drop_index(pos2(205.0, 40.0), &rects), 2);
    }

    #[test]
    fn between_two_cells_in_a_row_resolves_to_the_gap() {
        let rects = grid(2);
        // Pointer just left of cell1's center (160,40) but right of cell0's
        // center (50,40): nearest cell is cell1, pointer is left of its
        // center → insert before it (index 1), i.e. the gap between them.
        assert_eq!(compute_drop_index(pos2(120.0, 40.0), &rects), 1);
    }

    #[test]
    fn second_row_wrap_resolves_within_that_row_not_row_boundary() {
        // 4 cells: row0 = [0,1], row1 = [2,3]. Pointer over row1 cell0's
        // area should resolve to an index in {2,3}, not spill back into row0.
        let rects = grid(4);
        // cell2 center = (50, 130); pointer near it, slightly left.
        let idx = compute_drop_index(pos2(45.0, 130.0), &rects);
        assert_eq!(idx, 2);
        // pointer near cell3 center (160, 130), slightly right → insert after.
        let idx2 = compute_drop_index(pos2(165.0, 130.0), &rects);
        assert_eq!(idx2, 4);
    }

    #[test]
    fn drop_far_right_and_below_is_len() {
        let rects = grid(4);
        assert_eq!(compute_drop_index(pos2(9999.0, 9999.0), &rects), 4);
    }
}
