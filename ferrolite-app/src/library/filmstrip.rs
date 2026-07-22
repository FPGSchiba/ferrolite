//! Develop top-bar filmstrip: a horizontally-scrolling row of the current
//! folder's image thumbnails (same order as the grid), with the open image
//! outlined in the accent colour. Clicking a thumbnail returns its id so the
//! app can switch the viewer to it. Reuses the catalog thumbnail cache and the
//! grid's lazy-load path.

use crate::state::AppState;
use crate::theme;

pub const MIN_FILMSTRIP_HEIGHT: f32 = 64.0;
pub const MAX_FILMSTRIP_HEIGHT: f32 = 220.0;
#[allow(dead_code)]
pub const DEFAULT_FILMSTRIP_HEIGHT: f32 = 96.0;

/// Clamp height between 64.0 and 220.0 pixels.
pub fn clamp_filmstrip_height(h: f32) -> f32 {
    h.clamp(MIN_FILMSTRIP_HEIGHT, MAX_FILMSTRIP_HEIGHT)
}

const GAP: f32 = 10.0;
/// Clamp on cell width (as a multiple of thumbnail height) so extreme panoramas or
/// super-tall portraits can't break the strip's layout.
const MIN_ASPECT: f32 = 0.4;
const MAX_ASPECT: f32 = 2.5;

/// Render the strip; return the image id clicked this frame, if any.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, current_id: Option<i64>) -> Option<i64> {
    let mut clicked: Option<i64> = None;
    let panel_h = clamp_filmstrip_height(state.settings.filmstrip_height);
    let thumb_h = (panel_h - 24.0).clamp(32.0, 196.0);

    // Snapshot the ids/decode-status/aspect/rating/flag up front so we don't
    // hold an immutable borrow of `state.images` while mutably borrowing
    // `state` for thumbnails.
    let queued_ids: std::collections::HashSet<i64> = state.export_queue.iter().copied().collect();
    let cells: Vec<(i64, bool, f32, u8, ferrolite_image::Flag, bool, bool)> = state
        .images
        .iter()
        .map(|r| {
            (
                r.id,
                // Gated on `Done` (not just `!= Failed`), matching the grid's
                // `paint_cell` guard: a `Pending` row has no thumbnail blob yet,
                // so requesting one would submit a job that immediately finds
                // nothing (wasted one-shot lazy-load job on a cold `Pending`
                // cell). `Done` implies the blob is present.
                r.decode_status == ferrolite_catalog::DecodeStatus::Done,
                crate::library::grid::cell_aspect(r),
                r.rating.get(),
                r.flag,
                r.has_edits,
                queued_ids.contains(&r.id),
            )
        })
        .collect();

    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for (id, decodable, aspect, rating, flag, has_edits, queued) in cells {
                    // Always reserve the cell's space so the scroll extent and
                    // `scroll_to_rect` stay correct, but only do the expensive
                    // thumbnail work (DB read + JPEG decode + GPU upload + paint)
                    // for cells actually on screen. Without this, opening the
                    // viewer would synchronously decode EVERY image's thumbnail on
                    // the first Develop frame, blocking the UI thread for seconds.
                    let cell_w = (aspect * thumb_h)
                        .round()
                        .clamp(MIN_ASPECT * thumb_h, MAX_ASPECT * thumb_h);
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(cell_w, thumb_h), egui::Sense::click());
                    if ui.is_rect_visible(rect) {
                        // Lazy-load the thumbnail (same path as the grid), visible-only.
                        // The DB read + JPEG decode run off-thread; decoded pixels
                        // arrive over the event channel. NO UI-thread decode here.
                        if !state.textures.contains(id) && decodable {
                            state.request_thumbnail(ui.ctx(), id);
                        }
                        if let Some(tex) = state.textures.get(id) {
                            egui::Image::new(tex)
                                .fit_to_exact_size(rect.size())
                                .paint_at(ui, rect);
                        } else {
                            ui.painter().rect_filled(rect, 2.0, theme::BG_PANEL);
                        }
                        if Some(id) == current_id {
                            ui.painter().rect_stroke(
                                rect,
                                2.0,
                                egui::Stroke::new(2.0_f32, theme::ACCENT),
                            );
                        }
                        if rating > 0 {
                            crate::library::icons::rating_stars(
                                ui.painter(),
                                rect.left_bottom() + egui::vec2(3.0, -6.0),
                                3.0,
                                1.5,
                                rating,
                                rating,
                                theme::STAR,
                                true,
                            );
                        }
                        let flag_color = match flag {
                            ferrolite_image::Flag::Pick => Some((theme::SEMANTIC_GREEN, false)),
                            ferrolite_image::Flag::Reject => Some((theme::SEMANTIC_RED, true)),
                            ferrolite_image::Flag::None => None,
                        };
                        if let Some((c, reject)) = flag_color {
                            crate::library::icons::flag(
                                ui.painter(),
                                rect.left_top() + egui::vec2(7.0, 12.0),
                                10.0,
                                true,
                                c,
                                true,
                                reject,
                            );
                        }
                        // "Edited" pip (top-right) when the image carries edits.
                        if has_edits {
                            let c = rect.right_top() + egui::vec2(-7.0, 7.0);
                            ui.painter()
                                .circle_filled(c, 3.0, crate::theme::ACCENT_BRIGHT);
                        }
                        // Export-queue badge (bottom-right): the other three
                        // corners are already used by flag/edited/rating.
                        if queued {
                            crate::library::icons::queued_badge(
                                ui.painter(),
                                rect.right_bottom() + egui::vec2(-2.0, -10.0),
                                10.0,
                                theme::TEXT_PRIMARY,
                                theme::ACCENT,
                            );
                        }
                    }
                    // Keep the current image centered in the strip. egui clamps
                    // scrolling to the content bounds, so near the ends the cell
                    // naturally sits toward the left/right edge instead of forcing
                    // an over-scroll. (Runs even when off-screen so an off-screen
                    // current is pulled to center, then loads next frame.)
                    if Some(id) == current_id {
                        ui.scroll_to_rect(rect, Some(egui::Align::Center));
                    }
                    if resp.clicked() {
                        clicked = Some(id);
                    }
                    let menu_id = id;
                    resp.context_menu(|ui| {
                        crate::library::image_context_menu::show(ui, state, menu_id, true);
                    });
                }
            });
        });

    // Horizontal drag handle / splitter bar at the bottom edge of the filmstrip.
    let (handle_rect, handle_resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::drag());

    if handle_resp.hovered() || handle_resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }

    if handle_resp.dragged() {
        let delta = handle_resp.drag_delta().y;
        if delta != 0.0 {
            state.settings.filmstrip_height =
                clamp_filmstrip_height(state.settings.filmstrip_height + delta);
        }
    }

    let line_y = handle_rect.center().y;
    let stroke_color = if handle_resp.hovered() || handle_resp.dragged() {
        theme::ACCENT
    } else {
        egui::Color32::from_rgb(0x38, 0x38, 0x38)
    };
    ui.painter().line_segment(
        [
            egui::pos2(handle_rect.min.x, line_y),
            egui::pos2(handle_rect.max.x, line_y),
        ],
        egui::Stroke::new(1.0_f32, stroke_color),
    );

    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filmstrip_height_drag_clamping() {
        assert_eq!(clamp_filmstrip_height(50.0), MIN_FILMSTRIP_HEIGHT);
        assert_eq!(clamp_filmstrip_height(64.0), 64.0);
        assert_eq!(clamp_filmstrip_height(96.0), 96.0);
        assert_eq!(clamp_filmstrip_height(150.0), 150.0);
        assert_eq!(clamp_filmstrip_height(220.0), 220.0);
        assert_eq!(clamp_filmstrip_height(300.0), MAX_FILMSTRIP_HEIGHT);

        let mut height = DEFAULT_FILMSTRIP_HEIGHT;
        height = clamp_filmstrip_height(height + 50.0);
        assert_eq!(height, 146.0);
        height = clamp_filmstrip_height(height + 100.0);
        assert_eq!(height, MAX_FILMSTRIP_HEIGHT);
        height = clamp_filmstrip_height(height - 200.0);
        assert_eq!(height, MIN_FILMSTRIP_HEIGHT);
    }
}
