//! Develop top-bar filmstrip: a horizontally-scrolling row of the current
//! folder's image thumbnails (same order as the grid), with the open image
//! outlined in the accent colour. Clicking a thumbnail returns its id so the
//! app can switch the viewer to it. Reuses the catalog thumbnail cache and the
//! grid's lazy-load path.

use crate::state::AppState;
use crate::theme;

/// Thumbnail cell size (3:2) and gap, in points.
const THUMB_W: f32 = 96.0;
const THUMB_H: f32 = 64.0;
const GAP: f32 = 6.0;

/// Render the strip; return the image id clicked this frame, if any.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, current_id: Option<i64>) -> Option<i64> {
    let mut clicked: Option<i64> = None;
    // Snapshot the ids/decode-status/rating/flag up front so we don't hold an
    // immutable borrow of `state.images` while mutably borrowing `state` for
    // thumbnails.
    let cells: Vec<(i64, bool, u8, ferrolite_image::Flag)> = state
        .images
        .iter()
        .map(|r| {
            (
                r.id,
                r.decode_status != ferrolite_catalog::DecodeStatus::Failed,
                r.rating.get(),
                r.flag,
            )
        })
        .collect();

    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for (id, decodable, rating, flag) in cells {
                    // Always reserve the cell's space so the scroll extent and
                    // `scroll_to_rect` stay correct, but only do the expensive
                    // thumbnail work (DB read + JPEG decode + GPU upload + paint)
                    // for cells actually on screen. Without this, opening the
                    // viewer would synchronously decode EVERY image's thumbnail on
                    // the first Develop frame, blocking the UI thread for seconds.
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(THUMB_W, THUMB_H), egui::Sense::click());
                    if ui.is_rect_visible(rect) {
                        // Lazy-load the thumbnail (same path as the grid), visible-only.
                        if !state.textures.contains(id) && decodable {
                            if let Ok(Some(thumb)) = state.reads.get_thumbnail(id) {
                                state.upload_thumbnail(ui.ctx(), id, thumb.bytes);
                            }
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
                                egui::Stroke::new(2.0, theme::ACCENT),
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
                                theme::ACCENT,
                                true,
                            );
                        }
                        let flag_color = match flag {
                            ferrolite_image::Flag::Pick => Some(theme::SEMANTIC_GREEN),
                            ferrolite_image::Flag::Reject => Some(theme::SEMANTIC_RED),
                            ferrolite_image::Flag::None => None,
                        };
                        if let Some(c) = flag_color {
                            crate::library::icons::flag(
                                ui.painter(),
                                rect.left_top() + egui::vec2(7.0, 12.0),
                                10.0,
                                true,
                                c,
                                true,
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
    clicked
}
