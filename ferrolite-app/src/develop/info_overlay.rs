//! Compact read-only HUD of the key photographic facts + live zoom, drawn as a
//! hovering translucent-black box pinned to the image canvas — mirrors the
//! histogram overlay's style (`draw_histogram_overlay`), anchored bottom-left so
//! it never clashes with the top-right histogram. Consumes only a formatted
//! `ImageFacts`; `ui.min_rect()` is the canvas rect it pins against.

/// Facts-box background, matching the histogram overlay's translucent black.
const FILL_ALPHA: u8 = 160;
/// Inset from the canvas edges, matching the histogram overlay's margin.
const MARGIN: f32 = 12.0;
/// Bright near-white so text stays legible on the black box regardless of theme.
const TEXT: egui::Color32 = egui::Color32::from_gray(235);
/// Fallback height for the info overlay HUD box, used by `draw_toggle_button`
/// only before `draw()` has ever painted the overlay this session (e.g. the
/// very first frame, or the overlay has never been shown yet) — see
/// `resolve_overlay_height`. Once `draw()` has run at least once, the pill is
/// positioned from the overlay's REAL last-frame height instead, so this
/// number does not need to track the overlay's actual content height.
const OVERLAY_HEIGHT_FALLBACK: f32 = 108.0;

/// `egui` temp-data id used to hand the overlay's real painted height from
/// `draw()` to `draw_toggle_button()`. The two live in the same `Ui`/frame,
/// but `draw_toggle_button` is called BEFORE `draw()` each frame (see
/// `develop::canvas::overlays::draw_info`), so it can only ever see the
/// height recorded during the *previous* frame. That one-frame lag is fine
/// for a HUD whose height changes only when the user toggles it or switches
/// photos, not every frame.
fn overlay_height_temp_id() -> egui::Id {
    egui::Id::new("develop_info_overlay_actual_height")
}

/// Resolve the overlay's real last-frame height, falling back to
/// `OVERLAY_HEIGHT_FALLBACK` if `draw()` has never recorded one yet.
fn resolve_overlay_height(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp::<f32>(overlay_height_temp_id()))
        .unwrap_or(OVERLAY_HEIGHT_FALLBACK)
}

pub fn draw(ui: &egui::Ui, facts: &crate::develop::info::ImageFacts) {
    let canvas_rect = ui.min_rect();
    // Bottom-left corner of the box, `MARGIN` inside the canvas. The pivot makes
    // this the box's lower-left corner, so the box grows upward as lines are
    // added without needing to know its height in advance.
    let pos = egui::pos2(canvas_rect.left() + MARGIN, canvas_rect.bottom() - MARGIN);

    let area_response = egui::Area::new(egui::Id::new("develop_info_overlay"))
        .order(egui::Order::Middle)
        .fixed_pos(pos)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_black_alpha(FILL_ALPHA))
                .rounding(4.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    for line in [
                        &facts.focal,
                        &facts.aperture,
                        &facts.shutter,
                        &facts.iso,
                        &facts.zoom,
                    ] {
                        if !line.is_empty() {
                            ui.label(egui::RichText::new(line.as_str()).color(TEXT));
                        }
                    }
                });
        });

    // Record the REAL painted height (frame + all rows), so the toggle pill
    // can dock above the actual box instead of a hardcoded estimate — see
    // `overlay_height_temp_id`.
    let real_height = area_response.response.rect.height();
    ui.ctx()
        .data_mut(|d| d.insert_temp(overlay_height_temp_id(), real_height));
}

/// Y-offset (up from the canvas bottom edge) of the info pill's anchor.
/// Overlay visible: sit above the overlay box. Hidden: sit at the corner margin.
pub(crate) fn pill_bottom_offset(overlay_visible: bool, overlay_height: f32) -> f32 {
    if overlay_visible {
        overlay_height + 2.0 * MARGIN
    } else {
        MARGIN
    }
}

/// Draw the floating `ℹ Info` pill button on the canvas. When the overlay is
/// hidden, anchors at the bottom-left corner margin. When visible, positions
/// above the EXIF overlay's REAL last-frame height (see `resolve_overlay_height`).
/// Highlights with accent tint when `show_info_panel` is true, and clicking
/// toggles `show_info_panel`.
pub fn draw_toggle_button(ui: &egui::Ui, show_info_panel: &mut bool) {
    let canvas_rect = ui.min_rect();
    let overlay_height = resolve_overlay_height(ui.ctx());
    let offset = pill_bottom_offset(*show_info_panel, overlay_height);
    let pos = egui::pos2(canvas_rect.left() + MARGIN, canvas_rect.bottom() - offset);

    egui::Area::new(egui::Id::new("develop_info_pill_button"))
        // `Foreground` (strictly above the overlay's `Middle`) so the pill is
        // never painted underneath the overlay even during the one-frame lag
        // in `resolve_overlay_height`, or mid-transition when the overlay's
        // height is changing (e.g. facts gaining/losing a row).
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .show(ui.ctx(), |ui| {
            let (bg_color, stroke_color, text_color) = if *show_info_panel {
                (
                    crate::theme::ACCENT_BG_SEL,
                    egui::Stroke::new(1.0_f32, crate::theme::ACCENT),
                    crate::theme::ACCENT_BRIGHT,
                )
            } else {
                (
                    egui::Color32::from_black_alpha(FILL_ALPHA),
                    egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(30)),
                    TEXT,
                )
            };

            let btn = egui::Button::new(
                egui::RichText::new(format!("{} Info", crate::icons::INFO)).color(text_color),
            )
            .fill(bg_color)
            .stroke(stroke_color)
            .rounding(12.0);

            if ui.add(btn).clicked() {
                *show_info_panel = !*show_info_panel;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pill_docks_to_corner_when_overlay_hidden() {
        assert_eq!(pill_bottom_offset(false, 120.0), MARGIN);
        assert!(pill_bottom_offset(true, 120.0) > 120.0);
    }

    #[test]
    fn resolve_overlay_height_falls_back_before_any_frame_painted() {
        // No call to `draw()` has ever run against this context, so no real
        // height has been recorded — must fall back to the documented default
        // rather than e.g. panicking or returning 0.0 (which would dock the
        // pill at the very bottom, underneath the overlay it hasn't drawn yet).
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            assert_eq!(resolve_overlay_height(ctx), OVERLAY_HEIGHT_FALLBACK);
        });
    }

    #[test]
    fn resolve_overlay_height_uses_real_height_once_recorded() {
        // Once `draw()` has recorded a real painted height (as it does at the
        // end of every frame it runs in), `draw_toggle_button` must use that
        // instead of the fallback estimate.
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            ctx.data_mut(|d| d.insert_temp(overlay_height_temp_id(), 150.0_f32));
            assert_eq!(resolve_overlay_height(ctx), 150.0);
        });
    }
}
