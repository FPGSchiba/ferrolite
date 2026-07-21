//! The Crop tool: a canvas overlay (crop handles + rule-of-thirds grid) plus a
//! "Crop" panel tab (angle + aspect controls). Both wrap existing, already-tested
//! code (`crop_overlay::show`, the former adjustment-panel "Geometry" section) so
//! this migration is behavior-preserving.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::tool::{DevelopCtx, DevelopTool, PanelTab, TabId, ToolId};
use crate::state::AppState;
use crate::widgets::slider::EguiSlider;
use ferrolite_pipeline::{Aspect, Geometry, Op, OpKind};

pub struct CropTool;

impl DevelopTool for CropTool {
    fn id(&self) -> ToolId {
        ToolId::Crop
    }
    fn icon(&self) -> &'static str {
        crate::icons::CROP
    }
    fn label(&self) -> &'static str {
        "Crop"
    }
    fn enabled(&self, ctx: &DevelopCtx) -> bool {
        ctx.state.viewer.is_some()
    }
    fn tabs(&self) -> Vec<Box<dyn PanelTab>> {
        vec![Box::new(CropTab)]
    }
    fn canvas(
        &self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        state: &mut AppState,
    ) -> Option<EditOutcome> {
        // Wrap the existing crop overlay verbatim (mirrors app.rs:3689-3705): pre-extract
        // the OpStack and the aspect dims from the viewer before calling into the overlay,
        // since a later `apply_edit` call needs an exclusive borrow of `state`.
        let (stack, dims) = {
            let v = state.viewer.as_ref()?;
            (v.op_stack.clone(), v.image_dims.unwrap_or((1, 1)))
        };
        crate::develop::crop_overlay::show(ui, image_rect, &stack, dims)
    }
}

pub struct CropTab;

impl PanelTab for CropTab {
    fn id(&self) -> TabId {
        TabId("crop")
    }
    fn label(&self) -> &str {
        "Crop"
    }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        let stack = state.viewer.as_ref()?.op_stack.clone();
        let mut out: Option<EditOutcome> = None;

        // ── Geometry ── (angle + aspect; the crop overlay lives on the canvas)
        // Moved verbatim from adjustment_panel.rs's former "Geometry" CollapsingHeader
        // body. `crop_active` is NOT set here — the app derives it from whether this
        // tool is the active tool (ToolState.active == Crop), not from section state.
        let geo = stack.geometry().unwrap_or(Geometry {
            crop: ferrolite_pipeline::CropRect::full(),
            angle_deg: 0.0,
            aspect: Aspect::Original,
        });
        let mut angle = geo.angle_deg;
        let r = ui.add(EguiSlider {
            label: "Angle",
            value: &mut angle,
            min: -45.0,
            max: 45.0,
            default: 0.0,
            step: 0.1,
            decimals: 1,
            unit: "\u{b0}",
            bipolar: true,
            signed: true,
            custom_label_w: None,
        });
        let mut aspect = geo.aspect;
        egui::ComboBox::from_label("Aspect")
            .selected_text(format!("{aspect:?}"))
            .show_ui(ui, |ui| {
                for a in [
                    Aspect::Original,
                    Aspect::Free,
                    Aspect::Square,
                    Aspect::ThreeTwo,
                    Aspect::FourThree,
                    Aspect::SixteenNine,
                ] {
                    ui.selectable_value(&mut aspect, a, format!("{a:?}"));
                }
            });
        if r.changed() || aspect != geo.aspect {
            let new_geo = Geometry {
                crop: geo.crop,
                angle_deg: angle,
                aspect,
            };
            let s = if new_geo.angle_deg == 0.0
                && new_geo.aspect == Aspect::Original
                && new_geo.crop == ferrolite_pipeline::CropRect::full()
            {
                stack.reset(OpKind::Geometry)
            } else {
                stack.set_op(Op::Geometry(new_geo))
            };
            out = Some(EditOutcome {
                stack: s,
                kind: OpKind::Geometry,
                commit: r.drag_stopped() || !r.dragged() || aspect != geo.aspect,
            });
        }
        if ui.small_button("Reset crop").clicked() {
            out = Some(EditOutcome {
                stack: stack.reset(OpKind::Geometry),
                kind: OpKind::Geometry,
                commit: true,
            });
        }

        out
    }
}
