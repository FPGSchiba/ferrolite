//! The Develop tool/tab extensibility API (design §4). A `DevelopTool` is a palette
//! entry with an optional canvas overlay + temporary panel tab(s); a `PanelTab` is one
//! control group in the right panel. The `DevelopToolRegistry` (built once, on
//! `FerroliteApp`) owns the always-present base adjustment tabs + the ordered canvas
//! tools. Adding a tool = implement `DevelopTool` + push it in `standard()`; adding a
//! tab = implement `PanelTab` + include it in `base_tabs()` or a tool's `tabs()`.

use crate::develop::adjustment_panel::EditOutcome;
use crate::state::AppState;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolId {
    Adjust,
    Crop,
    Mask,
    Heal,
}

/// Stable per-tab id (e.g. `TabId("light")`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TabId(pub &'static str);

/// Read-only context a tool reads to decide its enabled-state.
pub struct DevelopCtx<'a> {
    pub state: &'a AppState,
}

/// One control group in the right panel.
pub trait PanelTab {
    fn id(&self) -> TabId;
    fn label(&self) -> &str;
    /// Render this tab's controls; return an op edit if one was made this frame.
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome>;
}

/// A Develop canvas tool: a palette entry + its overlay + the temporary tab(s) it
/// injects while active. `Adjust` is the default no-canvas-tool state (base tabs
/// only, no overlay). `Heal` is disabled (P5).
pub trait DevelopTool {
    fn id(&self) -> ToolId;
    fn icon(&self) -> &'static str;
    fn label(&self) -> &'static str;
    fn enabled(&self, ctx: &DevelopCtx) -> bool;
    /// Temporary tab(s) appended after the base tabs while this tool is active.
    fn tabs(&self) -> Vec<Box<dyn PanelTab>> {
        Vec::new()
    }
    /// Optional canvas overlay/affordances; returns an op edit if a gesture produced
    /// one this frame. `None` = no overlay.
    fn canvas(
        &self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        state: &mut AppState,
    ) -> Option<EditOutcome> {
        let _ = (ui, image_rect, state);
        None
    }
}

/// Base adjustment tabs + the ordered canvas tools shown in the palette. Built once.
pub struct DevelopToolRegistry {
    tools: Vec<Box<dyn DevelopTool>>,
    base_tabs: Vec<Box<dyn PanelTab>>,
}

impl DevelopToolRegistry {
    pub fn new(base_tabs: Vec<Box<dyn PanelTab>>, tools: Vec<Box<dyn DevelopTool>>) -> Self {
        Self { tools, base_tabs }
    }
    pub fn tools(&self) -> &[Box<dyn DevelopTool>] {
        &self.tools
    }
    pub fn base_tabs(&self) -> &[Box<dyn PanelTab>] {
        &self.base_tabs
    }
    pub fn get(&self, id: ToolId) -> Option<&dyn DevelopTool> {
        self.tools.iter().find(|t| t.id() == id).map(|b| b.as_ref())
    }
}

impl DevelopToolRegistry {
    /// The shipped tool set: Adjust (default), Crop, Mask, Heal (disabled).
    pub fn standard() -> Self {
        use crate::develop::tools::{
            adjust::AdjustTool, crop::CropTool, heal::HealTool, mask::MaskTool,
        };
        Self::new(
            crate::develop::base_tabs::base_tabs(),
            vec![
                Box::new(AdjustTool),
                Box::new(CropTool),
                Box::new(MaskTool),
                Box::new(HealTool),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTab(TabId, &'static str);
    impl PanelTab for DummyTab {
        fn id(&self) -> TabId {
            self.0
        }
        fn label(&self) -> &str {
            self.1
        }
        fn show(&self, _ui: &mut egui::Ui, _state: &mut AppState) -> Option<EditOutcome> {
            None
        }
    }

    struct DummyTool {
        id: ToolId,
        enabled: bool,
        tabs: Vec<TabId>,
    }
    impl DevelopTool for DummyTool {
        fn id(&self) -> ToolId {
            self.id
        }
        fn icon(&self) -> &'static str {
            "x"
        }
        fn label(&self) -> &'static str {
            "dummy"
        }
        fn enabled(&self, _ctx: &DevelopCtx) -> bool {
            self.enabled
        }
        fn tabs(&self) -> Vec<Box<dyn PanelTab>> {
            self.tabs
                .iter()
                .map(|t| Box::new(DummyTab(*t, "t")) as Box<dyn PanelTab>)
                .collect()
        }
    }

    fn dummy_registry() -> DevelopToolRegistry {
        DevelopToolRegistry::new(
            vec![Box::new(DummyTab(TabId("light"), "Light")) as Box<dyn PanelTab>],
            vec![
                Box::new(DummyTool {
                    id: ToolId::Adjust,
                    enabled: true,
                    tabs: vec![],
                }) as Box<dyn DevelopTool>,
                Box::new(DummyTool {
                    id: ToolId::Crop,
                    enabled: true,
                    tabs: vec![TabId("crop")],
                }),
                Box::new(DummyTool {
                    id: ToolId::Heal,
                    enabled: false,
                    tabs: vec![],
                }),
            ],
        )
    }

    #[test]
    fn registry_get_resolves_by_id_and_reports_enabled() {
        let reg = dummy_registry();
        assert_eq!(reg.tools().len(), 3);
        let crop = reg.get(ToolId::Crop).expect("crop present");
        assert_eq!(crop.id(), ToolId::Crop);
        assert_eq!(reg.base_tabs().len(), 1);
        assert!(
            reg.get(ToolId::Mask).is_none(),
            "mask not in this dummy registry"
        );
    }

    #[test]
    fn standard_registry_has_the_shipped_tools_in_order() {
        let reg = DevelopToolRegistry::standard();
        let ids: Vec<ToolId> = reg.tools().iter().map(|t| t.id()).collect();
        assert_eq!(
            ids,
            vec![ToolId::Adjust, ToolId::Crop, ToolId::Mask, ToolId::Heal]
        );
        assert_eq!(
            reg.base_tabs().len(),
            7,
            "Light/Color/Grade/Curve/Effects/Detail/Optics"
        );
        // Heal is the only always-disabled tool; assert via its (empty) tabs rather
        // than `enabled()` to avoid constructing a full `AppState` here.
        assert!(
            reg.get(ToolId::Heal).unwrap().tabs().is_empty(),
            "Heal has no tabs"
        );
    }
}
