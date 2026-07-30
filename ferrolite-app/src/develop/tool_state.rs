//! Pure, egui-free Develop tool/tab selection state (design §5). `Copy` so the app can
//! read it out of `AppState`, mutate a local while rendering, and write it back —
//! avoiding a multi-field borrow against `&mut AppState`. Owned by `AppState` (not
//! `ViewerState`) so the active tool/tab persists across image switches within a session.

use crate::develop::tool::{DevelopToolRegistry, TabId, ToolId};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ToolState {
    pub active: ToolId,
    pub active_tab: TabId,
    pub base_tab: TabId,
}

impl Default for ToolState {
    fn default() -> Self {
        Self {
            active: ToolId::Adjust,
            active_tab: TabId("light"),
            base_tab: TabId("light"),
        }
    }
}

impl ToolState {
    fn first_tab_of(&self, id: ToolId, reg: &DevelopToolRegistry) -> Option<TabId> {
        reg.get(id)
            .and_then(|t| t.tabs().first().map(|tab| tab.id()))
    }

    pub fn select_tool(&mut self, id: ToolId, enabled: bool, reg: &DevelopToolRegistry) {
        if !enabled {
            return;
        }
        self.active = id;
        self.active_tab = if id == ToolId::Adjust {
            self.base_tab
        } else {
            self.first_tab_of(id, reg).unwrap_or(self.base_tab)
        };
    }

    pub fn select_tab(&mut self, tab: TabId, reg: &DevelopToolRegistry) {
        self.active_tab = tab;
        if reg.base_tabs().iter().any(|t| t.id() == tab) {
            self.base_tab = tab;
        }
    }

    pub fn tab_bar(&self, reg: &DevelopToolRegistry) -> Vec<TabId> {
        let mut ids: Vec<TabId> = reg.base_tabs().iter().map(|t| t.id()).collect();
        if self.active != ToolId::Adjust {
            if let Some(t) = reg.get(self.active) {
                ids.extend(t.tabs().iter().map(|tab| tab.id()));
            }
        }
        ids
    }

    pub fn ensure_valid_tab(&mut self, reg: &DevelopToolRegistry) {
        let bar = self.tab_bar(reg);
        if !bar.contains(&self.active_tab) {
            if let Some(first) = bar.first() {
                self.active_tab = *first;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::tool::{DevelopCtx, DevelopTool, PanelTab};
    use crate::state::AppState;

    struct DummyTab(TabId);
    impl PanelTab for DummyTab {
        fn id(&self) -> TabId {
            self.0
        }
        fn label(&self) -> &str {
            "t"
        }
        fn show(
            &self,
            _ui: &mut egui::Ui,
            _s: &mut AppState,
        ) -> Option<crate::develop::adjustment_panel::EditOutcome> {
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
            "d"
        }
        fn enabled(&self, _c: &DevelopCtx) -> bool {
            self.enabled
        }
        fn tabs(&self) -> Vec<Box<dyn PanelTab>> {
            self.tabs
                .iter()
                .map(|t| Box::new(DummyTab(*t)) as Box<dyn PanelTab>)
                .collect()
        }
    }
    fn reg() -> DevelopToolRegistry {
        DevelopToolRegistry::new(
            vec![
                Box::new(DummyTab(TabId("light"))) as Box<dyn PanelTab>,
                Box::new(DummyTab(TabId("color"))),
            ],
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
    fn default_is_adjust_with_a_base_tab() {
        let s = ToolState::default();
        assert_eq!(s.active, ToolId::Adjust);
        assert_eq!(s.active_tab, TabId("light"));
    }

    #[test]
    fn selecting_disabled_tool_is_a_no_op() {
        let reg = reg();
        let mut s = ToolState::default();
        s.select_tool(ToolId::Heal, false, &reg);
        assert_eq!(s.active, ToolId::Adjust, "disabled Heal ignored");
    }

    #[test]
    fn selecting_a_tool_auto_selects_its_first_temporary_tab() {
        let reg = reg();
        let mut s = ToolState::default();
        s.select_tool(ToolId::Crop, true, &reg);
        assert_eq!(s.active, ToolId::Crop);
        assert_eq!(
            s.active_tab,
            TabId("crop"),
            "auto-selects the crop temp tab"
        );
    }

    #[test]
    fn selecting_adjust_restores_remembered_base_tab() {
        let reg = reg();
        let mut s = ToolState::default();
        s.select_tab(TabId("color"), &reg); // remember color as base
        s.select_tool(ToolId::Crop, true, &reg);
        assert_eq!(s.active_tab, TabId("crop"));
        s.select_tool(ToolId::Adjust, true, &reg);
        assert_eq!(s.active, ToolId::Adjust);
        assert_eq!(s.active_tab, TabId("color"), "restores remembered base tab");
    }

    #[test]
    fn tab_bar_is_base_then_active_tool_tabs() {
        let reg = reg();
        let mut s = ToolState::default();
        assert_eq!(s.tab_bar(&reg), vec![TabId("light"), TabId("color")]);
        s.select_tool(ToolId::Crop, true, &reg);
        assert_eq!(
            s.tab_bar(&reg),
            vec![TabId("light"), TabId("color"), TabId("crop")]
        );
    }

    #[test]
    fn ensure_valid_tab_clamps_stale_tab() {
        let reg = reg();
        let mut s = ToolState {
            active: ToolId::Adjust,
            active_tab: TabId("gone"),
            base_tab: TabId("gone"),
        };
        s.ensure_valid_tab(&reg);
        assert_eq!(
            s.active_tab,
            TabId("light"),
            "stale tab clamps to first base tab"
        );
    }

    #[test]
    fn selecting_a_tab_survives_ensure_valid_when_still_present() {
        let reg = reg();
        let mut ts = ToolState::default();
        ts.select_tab(TabId("color"), &reg);
        assert_eq!(ts.active_tab, TabId("color"));
        ts.ensure_valid_tab(&reg); // simulates re-validation on image switch
        assert_eq!(
            ts.active_tab,
            TabId("color"),
            "valid tab kept across switch"
        );
    }

    /// Regression for spec C5 ("leaving crop feels safe" — exiting the Crop
    /// tool commits nothing by itself). `select_tool`'s signature IS the
    /// proof: it takes no `OpStack`/`EditDoc` and returns `()`, so there is
    /// no channel through which switching tools could itself produce an
    /// `EditOutcome` (committed or not). The only place a crop edit can come
    /// from is `CropTool::canvas` (which calls `crop_overlay::show`), and
    /// `develop/canvas/viewer.rs`'s dispatch only invokes the CURRENTLY
    /// ACTIVE tool's `canvas()` (`if let Some(id) = active_tool { ...
    /// tool.canvas(...) }`, keyed off this exact `ToolState.active` field).
    /// The instant this method flips `active` away from `ToolId::Crop`,
    /// `crop_overlay::show` stops being called — drag in progress or not —
    /// so there is nothing left that could commit. (See
    /// `crop_overlay::tests::a_recorded_drag_produces_no_outcome_without_show_running_again`
    /// for the other half of this guarantee: a stale mid-drag record left
    /// behind by such a switch is inert without `show()` running again.)
    #[test]
    fn switching_away_from_crop_is_a_pure_state_flip_with_no_edit_channel() {
        let reg = reg();
        let mut s = ToolState::default();
        s.select_tool(ToolId::Crop, true, &reg);
        assert_eq!(s.active, ToolId::Crop);

        // Switch away as if mid-drag (a real mid-drag has no bearing here —
        // this call has no OpStack parameter at all, so "did it commit
        // something" isn't even an expressible question).
        s.select_tool(ToolId::Adjust, true, &reg);
        assert_eq!(s.active, ToolId::Adjust);
        assert_eq!(s.active_tab, s.base_tab, "back to the remembered base tab");
    }
}
