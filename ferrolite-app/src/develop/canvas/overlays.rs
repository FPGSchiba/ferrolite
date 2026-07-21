//! Canvas overlays manager.

use crate::develop::tool::DevelopToolRegistry;
use crate::state::AppState;

/// Draw the read-only, GPU-computed histogram as a floating, non-interactive
/// overlay anchored to the Develop canvas's top-right corner (spec 4.1 §7.1).
pub fn draw_histogram(ui: &egui::Ui, state: &AppState) {
    const MARGIN: f32 = 12.0;
    const WIDTH: f32 = 220.0;

    let canvas_rect = ui.min_rect();
    let bins = state
        .viewer
        .as_ref()
        .and_then(|v| v.histogram.bins.as_deref());

    let pos = egui::pos2(
        canvas_rect.right() - WIDTH - MARGIN,
        canvas_rect.top() + MARGIN,
    );

    egui::Area::new(egui::Id::new("develop_histogram_overlay"))
        .order(egui::Order::Middle)
        .fixed_pos(pos)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            ui.set_width(WIDTH);
            egui::Frame::none()
                .fill(egui::Color32::from_black_alpha(160))
                .rounding(4.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.set_width(WIDTH - 12.0);
                    crate::develop::histogram_widget::show(ui, bins);
                });
        });
}

/// Draw the floating EXIF info chip.
pub fn draw_info(ui: &egui::Ui, state: &AppState) {
    if state.settings.show_info_overlay
        && state.tool_state.active_tab != crate::develop::tool::TabId("info")
    {
        if let Some(v) = state.viewer.as_ref() {
            if let (Some(meta), Some(dims)) = (v.meta.as_ref(), v.image_dims) {
                let fit = ferrolite_vt::ViewTransform::fit(dims, v.viewport).zoom;
                let facts = crate::develop::info::ImageFacts::build(meta, v.view.zoom, fit, dims);
                crate::develop::info_overlay::draw(ui, &facts);
            }
        }
    }
}

/// Draw the floating tool palette.
pub fn draw_tool_palette(
    ui: &mut egui::Ui,
    state: &AppState,
    tool_registry: &DevelopToolRegistry,
) -> Option<crate::develop::tool_palette::PaletteAction> {
    if state.settings.show_tool_palette && state.viewer.is_some() {
        let ts = state.tool_state;
        let can_undo = state.viewer.as_ref().is_some_and(|v| v.history.can_undo());
        let can_redo = state.viewer.as_ref().is_some_and(|v| v.history.can_redo());
        let ctx_ro = crate::develop::tool::DevelopCtx { state };
        crate::develop::tool_palette::show(ui, tool_registry, ts, &ctx_ro, can_undo, can_redo)
    } else {
        None
    }
}
