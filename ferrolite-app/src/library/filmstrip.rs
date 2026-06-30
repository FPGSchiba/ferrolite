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
// wired in Task 3
#[allow(dead_code)]
pub fn show(ui: &mut egui::Ui, state: &mut AppState, current_id: Option<i64>) -> Option<i64> {
    let mut clicked: Option<i64> = None;
    // Snapshot the ids/decode-status up front so we don't hold an immutable
    // borrow of `state.images` while mutably borrowing `state` for thumbnails.
    let ids: Vec<(i64, bool)> = state
        .images
        .iter()
        .map(|r| {
            (
                r.id,
                r.decode_status != ferrolite_catalog::DecodeStatus::Failed,
            )
        })
        .collect();

    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for (id, decodable) in ids {
                    // Lazy-load the thumbnail (same path as the grid).
                    if !state.textures.contains(id) && decodable {
                        if let Ok(Some(thumb)) = state.reads.get_thumbnail(id) {
                            state.upload_thumbnail(ui.ctx(), id, thumb.bytes);
                        }
                    }
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(THUMB_W, THUMB_H), egui::Sense::click());
                    if let Some(tex) = state.textures.get(id) {
                        egui::Image::new(tex)
                            .fit_to_exact_size(rect.size())
                            .paint_at(ui, rect);
                    } else {
                        ui.painter().rect_filled(rect, 2.0, theme::BG_PANEL);
                    }
                    if Some(id) == current_id {
                        ui.painter()
                            .rect_stroke(rect, 2.0, egui::Stroke::new(2.0, theme::ACCENT));
                        // Keep the open image in view as navigation moves it.
                        ui.scroll_to_rect(rect, None);
                    }
                    if resp.clicked() {
                        clicked = Some(id);
                    }
                }
            });
        });
    clicked
}
