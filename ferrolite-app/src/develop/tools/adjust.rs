//! The Adjust tool: the default no-canvas-tool state (base adjustment tabs only,
//! no overlay). Selecting it in the palette deselects any active tool.

use crate::develop::tool::{DevelopCtx, DevelopTool, ToolId};

/// The default no-canvas-tool state: base adjustment tabs only, no overlay. Selecting
/// it in the palette deselects any active tool.
pub struct AdjustTool;
impl DevelopTool for AdjustTool {
    fn id(&self) -> ToolId {
        ToolId::Adjust
    }
    fn icon(&self) -> &'static str {
        crate::icons::ADJUST
    }
    fn label(&self) -> &'static str {
        "Adjust"
    }
    fn enabled(&self, ctx: &DevelopCtx) -> bool {
        ctx.state.viewer.is_some()
    }
}
