//! Virtualized thumbnail grid. Realizes only the visible window of cells, pulls
//! ready thumbnails from the read pool on demand, and promotes the visible
//! window's pending thumbnail jobs to `Visible` priority.

use crate::library::cell_state::{cell_state, CellState};
use crate::library::grid_layout::{layout, CachedGridLayout, LayoutSig};
use crate::library::icons;
use crate::state::AppState;
use crate::theme;
use ferrolite_catalog::ImageRecord;
use ferrolite_image::Flag;
use ferrolite_jobs::Priority;
use std::collections::HashSet;

const GAP: f32 = 8.0;
const SEL_ROUND: f32 = 6.0;
/// Height of the meta-label band (filename + capture date) under each cell.
const LABEL_H: f32 = 30.0;
/// Gap between the thumbnail and its label band.
const LABEL_PAD: f32 = 3.0;
/// Outer padding around the grid (left, right, top, bottom) so cells don't hug
/// the panel edges.
const MARGIN: f32 = 14.0;
/// Upper bound on how wide a filename label may push a cell, so one very long
/// name can't blow out a whole row.
const MAX_LABEL_W: f32 = 240.0;

pub fn show(ui: &mut egui::Ui, state: &mut AppState, cell: f32) -> Option<i64> {
    let avail_w = (ui.available_width() - 2.0 * MARGIN).max(1.0);
    let target_h = cell;

    // Rebuild the justified-rows layout only when the image set, width, or cell
    // size changed. Taken out of `state` for the render pass so `paint_cell` can
    // borrow `state` mutably without aliasing; restored at the end.
    let sig = LayoutSig {
        images_rev: state.images_rev,
        item_count: state.images.len(),
        avail_w: avail_w.round() as u32,
        target_h: target_h.round() as u32,
    };
    let mut cache = state.grid_layout.take();
    if cache.as_ref().map(|c| c.sig) != Some(sig) {
        let aspects: Vec<f32> = state.images.iter().map(cell_aspect).collect();
        // Per-cell minimum width = its label width, so portrait filenames aren't
        // clipped (the cell widens to the text and the image is centered in it).
        let min_widths: Vec<f32> = state.images.iter().map(|r| label_width(ui, r)).collect();
        cache = Some(CachedGridLayout {
            sig,
            layout: layout(&aspects, &min_widths, avail_w, target_h, GAP, LABEL_H),
        });
    }
    let cache = cache.expect("layout built above");

    let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
    let mut opened: Option<i64> = None;
    scroll.show_viewport(ui, |ui, viewport| {
        ui.set_height(cache.layout.total_height + 2.0 * MARGIN);
        // Content is offset down/right by MARGIN, so map the viewport into the
        // layout's own (0-based) coordinate space before picking visible rows.
        let scroll_top = (viewport.min.y - MARGIN).max(0.0);
        let vh = viewport.height() + MARGIN;
        let rows = cache.layout.visible_rows(scroll_top, vh);

        // Promote visible pending thumbnail jobs; demote ones that scrolled away.
        // Compute visible set immutably first (borrow checker: reprioritize borrows
        // state immutably; the mut paint loop comes after).
        let mut now_visible: HashSet<i64> = HashSet::new();
        for ri in rows.clone() {
            for item in &cache.layout.rows[ri].items {
                now_visible.insert(state.images[item.index].id);
            }
        }
        reprioritize(state, &now_visible);
        // Fetch tag associations for the visible window (only missing ids queried).
        state.ensure_tags_for(&now_visible);
        state.last_visible = now_visible;

        let origin = ui.min_rect().left_top() + egui::vec2(MARGIN, MARGIN);
        for ri in rows {
            let row = &cache.layout.rows[ri];
            for item in &row.items {
                let rec = state.images[item.index].clone();
                let cell_x = origin.x + item.x;
                let cell_y = origin.y + row.y;
                // Image centered within its (possibly wider) cell footprint.
                let img_x = cell_x + (item.width - item.img_width) * 0.5;
                let img_rect = egui::Rect::from_min_size(
                    egui::pos2(img_x, cell_y),
                    egui::vec2(item.img_width, row.img_height),
                );
                if let Some(id) = paint_cell(ui, state, &rec, img_rect) {
                    opened = Some(id);
                }
                let label_rect = egui::Rect::from_min_size(
                    egui::pos2(cell_x, img_rect.bottom() + LABEL_PAD),
                    egui::vec2(item.width, LABEL_H - LABEL_PAD),
                );
                paint_meta(ui, &rec, label_rect);
            }
        }
    });
    state.grid_layout = Some(cache);
    opened
}

/// Measured pixel width of a cell's meta label (the wider of filename/date), so
/// the layout can widen narrow cells enough to show the name. Capped so one long
/// name can't dominate a row. egui caches galleys, so repeated calls are cheap.
fn label_width(ui: &egui::Ui, rec: &ImageRecord) -> f32 {
    let name = ui.fonts(|f| {
        f.layout_no_wrap(
            rec.filename.clone(),
            egui::FontId::proportional(11.0),
            theme::TEXT_PRIMARY,
        )
        .size()
        .x
    });
    let date = format_capture_date(rec.capture_time.as_deref()).map_or(0.0, |d| {
        ui.fonts(|f| {
            f.layout_no_wrap(d, egui::FontId::proportional(10.0), theme::TEXT_DIM)
                .size()
                .x
        })
    });
    (name.max(date) + 6.0).min(MAX_LABEL_W)
}

/// Upright aspect ratio (width / height) of an image, applying its orientation
/// so portrait/landscape cells match what the thumbnail actually shows. Falls
/// back to square (1.0) when dimensions are unknown.
pub(crate) fn cell_aspect(rec: &ImageRecord) -> f32 {
    let w = rec.width.unwrap_or(0).max(1) as f32;
    let h = rec.height.unwrap_or(0).max(1) as f32;
    let (w, h) = if rec.orientation.swaps_dimensions() {
        (h, w)
    } else {
        (w, h)
    };
    (w / h).clamp(0.1, 10.0)
}

/// Draw the per-cell meta label centered under the (centered) thumbnail:
/// filename on top, capture date below. The cell footprint is at least the label
/// width (see `label_width`), so the centered text is never clipped.
/// Non-interactive — the thumbnail above is the click target.
fn paint_meta(ui: &egui::Ui, rec: &ImageRecord, rect: egui::Rect) {
    let p = ui.painter_at(rect);
    let cx = rect.center().x;
    p.text(
        egui::pos2(cx, rect.top()),
        egui::Align2::CENTER_TOP,
        &rec.filename,
        egui::FontId::proportional(11.0),
        theme::TEXT_PRIMARY,
    );
    if let Some(date) = format_capture_date(rec.capture_time.as_deref()) {
        p.text(
            egui::pos2(cx, rect.top() + 14.0),
            egui::Align2::CENTER_TOP,
            date,
            egui::FontId::proportional(10.0),
            theme::TEXT_DIM,
        );
    }
}

/// Format an EXIF `DateTimeOriginal` ("YYYY:MM:DD HH:MM:SS") as "YYYY-MM-DD
/// HH:MM" for display. Returns `None` for missing/empty values; passes through
/// unexpected formats (first 16 chars) rather than failing.
fn format_capture_date(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim();
    if s.is_empty() {
        return None;
    }
    let out: String = s
        .chars()
        .take(16)
        .enumerate()
        .map(|(i, c)| {
            if (i == 4 || i == 7) && c == ':' {
                '-'
            } else {
                c
            }
        })
        .collect();
    Some(out)
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
    // Determine selection state early so we can adjust the thumbnail rect.
    let selected = state.selection.contains(&rec.id) || state.selected == Some(rec.id);

    // Request a thumbnail off-thread if not yet cached (visible cell only). The
    // DB read + JPEG decode happen in a `Visible`-priority job; the decoded
    // pixels arrive over the event channel and are uploaded there. NO UI-thread
    // decode here.
    if !state.textures.contains(rec.id)
        && rec.decode_status != ferrolite_catalog::DecodeStatus::Failed
    {
        state.request_thumbnail(ui.ctx(), rec.id);
    }
    let has_tex = state.textures.contains(rec.id);
    let painter = ui.painter_at(rect);

    // Thumbnail fills the full cell in both states; the gradient border is
    // drawn on top at the end of the function.
    let img_rect = rect;

    // Round the thumbnail corners to match the selection border so a square
    // corner never pokes outside the rounded border. Unselected cells stay square.
    let img_round = if selected { SEL_ROUND } else { 0.0 };
    match cell_state(rec, has_tex) {
        CellState::Ready => {
            if let Some(tex) = state.textures.get(rec.id) {
                let img = egui::Image::new(tex)
                    .fit_to_exact_size(img_rect.size())
                    .rounding(img_round);
                img.paint_at(ui, img_rect);
            }
        }
        CellState::Placeholder => {
            painter.rect_filled(img_rect, img_round.max(2.0), theme::BG_PANEL);
        }
        CellState::Failed => {
            painter.rect_filled(img_rect, img_round.max(2.0), theme::BG_PANEL);
            painter.text(
                img_rect.center(),
                egui::Align2::CENTER_CENTER,
                "broken",
                egui::FontId::proportional(11.0),
                theme::SEMANTIC_RED,
            );
        }
    }

    // #8 — Rating stars (bottom-left): drawn shapes instead of glyphs.
    // Overlays are anchored to img_rect so they hug the thumbnail in both states.
    if rec.rating.get() > 0 {
        // origin = left-centre of the star row, sitting 8px above the bottom edge.
        let r = 4.0_f32;
        let gap = 2.0_f32;
        let row_y = img_rect.bottom() - 8.0;
        let origin = egui::pos2(img_rect.left() + 4.0 + r, row_y);
        // Show only the filled stars (no empty outlines): the grid overlay is a
        // status indicator, not an editable control — empties would imply clicks
        // that the grid doesn't handle. Matches the filmstrip.
        icons::rating_stars(
            &painter,
            origin,
            r,
            gap,
            rec.rating.get(),
            rec.rating.get(),
            theme::STAR,
            true,
        );
    }

    // #8 — Flag icon (top-left): drawn shapes instead of glyphs.
    match rec.flag {
        Flag::Pick => {
            icons::flag(
                &painter,
                egui::pos2(img_rect.left() + 6.0, img_rect.top() + 12.0),
                10.0,
                true,
                theme::SEMANTIC_GREEN,
                true,
            );
        }
        Flag::Reject => {
            icons::flag(
                &painter,
                egui::pos2(img_rect.left() + 6.0, img_rect.top() + 12.0),
                10.0,
                true,
                theme::SEMANTIC_RED,
                true,
            );
        }
        Flag::None => {}
    }

    // Tag colour dots (bottom-right), looked up from the loaded vocabulary.
    if let Some(tag_ids) = state.visible_tags.get(&rec.id) {
        let mut x = img_rect.right() - 8.0;
        for tid in tag_ids.iter().take(5) {
            if let Some(t) = state.tags.iter().find(|t| t.id == *tid) {
                let c = egui::Color32::from_rgb(t.color.r, t.color.g, t.color.b);
                painter.circle_filled(egui::pos2(x, img_rect.bottom() - 8.0), 4.0, c);
                x -= 11.0;
            }
        }
    }

    // Selection: ctrl/cmd-click toggles; shift-click range-select; plain click replaces.
    // Context menu on right-click.
    // Hit area remains the full rect (unchanged).
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

    // Selection border: a bright-blue rounded ring with a ~1px dark keyline on
    // each side, so it stays distinct on both dark and light/bluish thumbnails.
    // The whole 4px band is inset 2px so it sits fully inside the cell — the
    // painter is clipped to `rect`, so a band centered nearer the edge would have
    // its outer half clipped away (which hid the halo before).
    if selected {
        let path = rect.shrink(2.0);
        painter.rect_stroke(
            path,
            SEL_ROUND,
            egui::Stroke::new(4.0, egui::Color32::from_black_alpha(200)),
        );
        painter.rect_stroke(
            path,
            SEL_ROUND,
            egui::Stroke::new(2.0, theme::ACCENT_BRIGHT),
        );
    }

    // #5 — Right-click context menu (shared helper).
    let rec_id = rec.id;
    resp.context_menu(|ui| {
        crate::library::image_context_menu::show(ui, state, rec_id, false);
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

    #[test]
    fn format_capture_date_converts_exif_to_display() {
        assert_eq!(
            format_capture_date(Some("2023:05:14 18:32:07")).as_deref(),
            Some("2023-05-14 18:32")
        );
    }

    #[test]
    fn format_capture_date_handles_missing_and_empty() {
        assert_eq!(format_capture_date(None), None);
        assert_eq!(format_capture_date(Some("   ")), None);
    }

    #[test]
    fn format_capture_date_passes_through_unexpected() {
        // Not the EXIF shape → first 16 chars, no colon swaps at non-date spots.
        assert_eq!(
            format_capture_date(Some("sometime")).as_deref(),
            Some("sometime")
        );
    }
}
