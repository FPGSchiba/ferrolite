//! The Mask tool: a canvas overlay (coverage tint + brush/gradient/range
//! affordances) plus a "Mask" panel tab (masks list + selected-mask controls).
//! Both wrap existing, already-tested code (`mask_overlay::show`,
//! `mask_panel::show`) so this migration is behavior-preserving.
//!
//! NOTE: the app must call `rebuild_mask_overlay_if_needed(ctx)` before this
//! tool's `canvas()` runs while Mask is active, so `v.mask_overlay_tex` is
//! current — that glue is wired in a later task (Task 11); not this one's job.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::tool::{DevelopCtx, DevelopTool, PanelTab, TabId, ToolId};
use crate::state::AppState;

pub struct MaskTool;

impl DevelopTool for MaskTool {
    fn id(&self) -> ToolId {
        ToolId::Mask
    }
    fn icon(&self) -> &'static str {
        crate::icons::MASK
    }
    fn label(&self) -> &'static str {
        "Mask"
    }
    fn enabled(&self, ctx: &DevelopCtx) -> bool {
        ctx.state.viewer.is_some()
    }
    fn tabs(&self) -> Vec<Box<dyn PanelTab>> {
        vec![Box::new(MaskTab)]
    }
    fn canvas(
        &self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        state: &mut AppState,
    ) -> Option<EditOutcome> {
        // Wrap mask_overlay::show verbatim (mirrors app.rs:3724-3745): pre-extract
        // the shared bits out of the viewer first (all cheap Arc/handle clones),
        // releasing the borrow, then take &mut v.mask for the call.
        let (stack, dims, tex, preview_source) = {
            let v = state.viewer.as_ref()?;
            (
                v.op_stack.clone(),
                v.image_dims.unwrap_or((1, 1)),
                v.mask_overlay_tex.clone(),
                v.preview_source.clone(),
            )
        };
        let v = state.viewer.as_mut()?;
        crate::develop::mask_overlay::show(
            ui,
            image_rect,
            &stack,
            &mut v.mask,
            tex.as_ref(),
            dims,
            preview_source.as_ref(),
        )
    }
}

pub struct MaskTab;

impl PanelTab for MaskTab {
    fn id(&self) -> TabId {
        TabId("mask")
    }
    fn label(&self) -> &str {
        "Mask"
    }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        // Mirrors the current call (adjustment_panel.rs:244-252): pull the
        // OpStack out, then take &mut v.mask.
        let stack = state.viewer.as_ref()?.op_stack.clone();
        let keymap = state.settings.keymap.clone();
        let v = state.viewer.as_mut()?;
        crate::develop::mask_panel::show(ui, &stack, &mut v.mask, &keymap)
    }
}
