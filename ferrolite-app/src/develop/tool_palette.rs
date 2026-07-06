//! The floating Develop tool palette (design §6): a toggleable, interactive egui::Area
//! under the filmstrip holding the registered tools + undo/redo. Mirrors the histogram
//! overlay's Area pattern but is clickable. Plain vector drawing — cheap per frame.

use crate::develop::tool::{DevelopCtx, DevelopToolRegistry, ToolId};
use crate::develop::tool_state::ToolState;
use crate::widgets::tool_button;

pub enum PaletteAction {
    SelectTool(ToolId),
    Undo,
    Redo,
}

/// Render the floating tool palette anchored to the top-left of the Develop canvas
/// (`ui.min_rect()`, matching `draw_histogram_overlay`'s placement convention but
/// mirrored to the opposite corner). Interactive (`Order::Foreground`, unlike the
/// histogram's non-interactive `Order::Middle`) so its buttons receive clicks.
/// Returns the single action clicked this frame, if any.
pub fn show(
    ui: &egui::Ui,
    reg: &DevelopToolRegistry,
    ts: ToolState,
    ctx: &DevelopCtx,
    can_undo: bool,
    can_redo: bool,
) -> Option<PaletteAction> {
    const MARGIN: f32 = 12.0;
    let canvas_rect = ui.min_rect();
    let pos = egui::pos2(canvas_rect.left() + MARGIN, canvas_rect.top() + MARGIN);
    let mut action = None;

    egui::Area::new(egui::Id::new("develop_tool_palette"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_black_alpha(160))
                .rounding(4.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for tool in reg.tools() {
                            let enabled = tool.enabled(ctx);
                            let reason = (!enabled).then_some("Coming in P5");
                            if tool_button(
                                ui,
                                tool.icon(),
                                tool.label(),
                                ts.active == tool.id(),
                                enabled,
                                reason,
                            )
                            .clicked()
                                && enabled
                            {
                                action = Some(PaletteAction::SelectTool(tool.id()));
                            }
                        }
                        ui.separator();
                        if tool_button(ui, crate::icons::UNDO, "Undo", false, can_undo, None)
                            .clicked()
                            && can_undo
                        {
                            action = Some(PaletteAction::Undo);
                        }
                        if tool_button(ui, crate::icons::REDO, "Redo", false, can_redo, None)
                            .clicked()
                            && can_redo
                        {
                            action = Some(PaletteAction::Redo);
                        }
                    });
                });
        });
    action
}
