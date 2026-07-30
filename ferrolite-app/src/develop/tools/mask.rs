//! The Mask tool: a canvas overlay (coverage tint + brush/gradient/range
//! affordances). It injects no temporary panel tab of its own — the mask
//! management block + scope banner render above the shared base tabs in
//! `tool_panel::show` (design 2026-07-28 §3; Task 6), and the same
//! Light/Color/Effects tabs edit the selected mask via `ScopedEdit`.
//!
//! NOTE: the app calls `rebuild_mask_overlay_if_needed` before this tool's
//! `canvas()` runs while Mask is active, so `state.mask_overlay_native` (the
//! app-global GPU-native overlay texture id) is current.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::tool::{DevelopCtx, DevelopTool, PanelTab, ToolId};
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
        Vec::new()
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
        let (stack, dims, tex, highlight_tex, preview_source) = {
            let v = state.viewer.as_ref()?;
            (
                v.op_stack.clone(),
                v.image_dims.unwrap_or((1, 1)),
                state.mask_overlay_native,
                state.mask_overlay_highlight_native,
                v.preview_source.clone(),
            )
        };
        let v = state.viewer.as_mut()?;
        crate::develop::mask_overlay::show(
            ui,
            image_rect,
            &stack,
            &mut v.mask,
            tex,
            highlight_tex,
            dims,
            preview_source.as_ref(),
        )
    }
}
