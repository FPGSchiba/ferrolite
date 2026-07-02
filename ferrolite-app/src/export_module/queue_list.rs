//! The Export queue (spec §8.4): a wrapping grid of thumbnail preview cells
//! with reorder + remove controls, mirroring the Library grid's lazy
//! thumbnail path so queued images decode off the UI thread on demand.

use crate::library::icons;
use crate::state::AppState;
use crate::theme;

/// Thumbnail cell size (≈3:2) and layout constants.
const THUMB_W: f32 = 132.0;
const THUMB_H: f32 = 88.0;
const CELL_GAP: f32 = 10.0;
const ICON_BTN: f32 = 18.0;
const ICON_R: f32 = 5.0;

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

    let running = state.batch.as_ref().is_some_and(|b| !b.is_done());
    let mut do_move: Option<(usize, isize)> = None;
    let mut do_remove: Option<i64> = None;

    egui::Frame::none()
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

                        ui.allocate_ui(egui::vec2(THUMB_W, THUMB_H + 46.0), |ui| {
                            ui.vertical(|ui| {
                                paint_thumb(ui, state, id, rec, idx);
                                ui.set_width(THUMB_W);
                                ui.label(
                                    egui::RichText::new(&filename)
                                        .small()
                                        .color(theme::TEXT_DIM),
                                )
                                .on_hover_text(&filename);

                                ui.horizontal(|ui| {
                                    let earlier =
                                        icon_button(ui, ICON_BTN, !running, |p, c, col| {
                                            icons::caret(p, c, ICON_R, col, false);
                                        })
                                        .on_hover_text("Move earlier");
                                    if earlier.clicked() && !running {
                                        do_move = Some((idx, -1));
                                    }

                                    let later = icon_button(ui, ICON_BTN, !running, |p, c, col| {
                                        icons::caret(p, c, ICON_R, col, true);
                                    })
                                    .on_hover_text("Move later");
                                    if later.clicked() && !running {
                                        do_move = Some((idx, 1));
                                    }

                                    let remove =
                                        icon_button(ui, ICON_BTN, !running, |p, c, col| {
                                            icons::cross(p, c, ICON_R, col);
                                        })
                                        .on_hover_text("Remove");
                                    if remove.clicked() && !running {
                                        do_remove = Some(id);
                                    }
                                });
                            });
                        });
                    }
                });
            });
        });

    if let Some((idx, delta)) = do_move {
        state.queue_move(idx, delta);
    }
    if let Some(id) = do_remove {
        state.queue_remove(id);
    }
}

/// Draws the thumbnail cell: requests the decode job lazily (same path as the
/// Library grid's `paint_cell`), then paints either the ready texture or a
/// filename placeholder, plus the 1-based sequence badge.
fn paint_thumb(
    ui: &mut egui::Ui,
    state: &mut AppState,
    id: i64,
    rec: Option<&ferrolite_catalog::ImageRecord>,
    idx: usize,
) {
    let (rect, _resp) = ui.allocate_exact_size(egui::vec2(THUMB_W, THUMB_H), egui::Sense::hover());

    let failed = rec
        .map(|r| r.decode_status == ferrolite_catalog::DecodeStatus::Failed)
        .unwrap_or(false);
    if !state.textures.contains(id) && !failed {
        state.request_thumbnail(ui.ctx(), id);
    }

    let painter = ui.painter_at(rect);
    if let Some(tex) = state.textures.get(id) {
        let img = egui::Image::new(tex).fit_to_exact_size(rect.size());
        img.paint_at(ui, rect);
    } else {
        painter.rect_filled(rect, 3.0, theme::BG_PANEL);
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

    // 1-based sequence index badge, top-left, over the thumbnail.
    let badge_rect = egui::Rect::from_min_size(rect.left_top(), egui::vec2(20.0, 15.0));
    painter.rect_filled(badge_rect, 2.0, egui::Color32::from_black_alpha(140));
    painter.text(
        badge_rect.left_top() + egui::vec2(4.0, 2.0),
        egui::Align2::LEFT_TOP,
        format!("{}", idx + 1),
        egui::FontId::monospace(10.0),
        theme::TEXT_PRIMARY,
    );
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
