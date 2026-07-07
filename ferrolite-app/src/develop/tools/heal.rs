use crate::develop::tool::{DevelopCtx, DevelopTool, ToolId};

/// Inert P5 placeholder — registered but always disabled (greyed in the palette with
/// a "coming in P5" hover reason, supplied by the palette rendering).
pub struct HealTool;

impl DevelopTool for HealTool {
    fn id(&self) -> ToolId {
        ToolId::Heal
    }
    fn icon(&self) -> &'static str {
        crate::icons::HEAL
    }
    fn label(&self) -> &'static str {
        "Heal (P5)"
    }
    fn enabled(&self, _ctx: &DevelopCtx) -> bool {
        false
    }
}
