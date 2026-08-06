//! The Develop right-panel shell (design §7): global chrome + a shared tab bar (base
//! adjustment tabs ++ the active canvas tool's temporary tab) + active-tab dispatch.
//! Replaces the flat CollapsingHeader `adjustment_panel::show`.

use crate::develop::adjustment_panel::{EditOutcome, PanelOutcome};
use crate::develop::tool::{DevelopToolRegistry, ToolId};
use crate::state::AppState;
use crate::theme;
use ferrolite_color::WorkingSpace;

/// Whether the shared Light/Color/Effects tab row (+ the active tool's
/// temporary tabs) renders at all this frame. Crop replaces the tab row
/// entirely with its own dedicated panel (design 2026-07-29 §C3 / V2
/// README:69) — every other tool keeps the shared row (Mask only injects a
/// header ABOVE it via the branch above, never replaces it). Single source of
/// truth for the gate used both by `show()` below and by this module's tests.
fn tab_row_visible(active: ToolId) -> bool {
    active != ToolId::Crop
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    reg: &DevelopToolRegistry,
    working_space: WorkingSpace,
) -> PanelOutcome {
    // 1) Global chrome (not tool-specific) stays above the tab bar; also
    //    carries the Presets menu (P7 Task 8), which can itself produce an
    //    edit (applying a preset to the current image).
    let (ws_change, preset_edit) =
        crate::develop::adjustment_panel::chrome(ui, state, working_space);
    ui.separator();

    // 2) Copy ToolState out (it is Copy) so the tab-bar mutation doesn't fight the
    //    &mut AppState borrow used by the active tab's show(). Session-wide (on
    //    AppState, not ViewerState) so it survives image switches.
    if state.viewer.is_none() {
        return PanelOutcome {
            edit: preset_edit,
            working_space: ws_change,
        };
    }
    let mut ts = state.tool_state;
    ts.ensure_valid_tab(reg);

    // Mask mode: the mask-management block + a scope banner render ABOVE the
    // shared tab bar (design 2026-07-28 §3; Task 6) — there is no separate
    // "Mask" tab anymore; the same Light/Color/Effects tabs below edit the
    // selected mask through `ScopedEdit`.
    let mut mask_panel_edit: Option<EditOutcome> = None;
    if ts.active == ToolId::Mask {
        // Pre-extraction pattern MaskTab::show used (stack clone + keymap
        // clone + &mut v.mask), moved here verbatim from tools/mask.rs.
        let stack = state.viewer.as_ref().map(|v| v.op_stack.clone());
        let keymap = state.settings.keymap.clone();
        if let Some(stack) = stack {
            if let Some(v) = state.viewer.as_mut() {
                mask_panel_edit =
                    crate::develop::mask_panel::show(ui, &stack, &mut v.mask, &keymap);
            }
        }

        // Scope banner (accent = editing a mask; faint = nothing selected).
        match crate::develop::scope::current(state) {
            crate::develop::scope::EditScope::Mask(i) => {
                let name = state
                    .viewer
                    .as_ref()
                    .map(|v| {
                        crate::develop::mask_edit::layers(&v.op_stack).layers[i]
                            .name
                            .clone()
                    })
                    .unwrap_or_default();
                ui.label(
                    egui::RichText::new(format!(
                        "Editing: {name} — adjustments below apply only inside this mask"
                    ))
                    .color(theme::ACCENT)
                    .size(11.0),
                );
            }
            crate::develop::scope::EditScope::MaskNone => {
                ui.label(
                    egui::RichText::new(
                        "Create or select a mask — adjustments below edit the selected mask",
                    )
                    .color(theme::TEXT_FAINT)
                    .size(11.0),
                );
            }
            crate::develop::scope::EditScope::Global => {}
        }
        ui.separator();
    }

    // Crop: tabs disappear entirely (design 2026-07-29 §C3 / V2 README:69) —
    // replaced by `CropTab::show`'s own dedicated panel (CROP & TRANSFORM +
    // GEOMETRY sections), reached below through the SAME "active tool's temp
    // tabs" dispatch every other tool already uses (`ts.active_tab` still
    // resolves to `TabId("crop")` — only the visible tab ROW is suppressed).
    if tab_row_visible(ts.active) {
        let base_tabs = reg.base_tabs();
        let tool_tabs = if ts.active != crate::develop::tool::ToolId::Adjust {
            reg.get(ts.active).map(|t| t.tabs()).unwrap_or_default()
        } else {
            Vec::new()
        };

        let mut tab_items: Vec<(crate::develop::tool::TabId, &str)> = Vec::new();
        for tab in base_tabs.iter() {
            tab_items.push((tab.id(), tab.label()));
        }
        for tab in tool_tabs.iter() {
            tab_items.push((tab.id(), tab.label()));
        }

        let mut active_tab = ts.active_tab;
        let resp = crate::widgets::tabs::tab_row(ui, &mut active_tab, &tab_items);
        if resp.changed() {
            ts.select_tab(active_tab, reg);
        }
        ui.separator();
    }

    // 3) Dispatch the active tab's show(). Look the tab object up fresh (base ++ active
    //    tool tabs) and call it.
    let active = ts.active_tab;
    // Seed with the Presets menu's edit (if any), then the mask panel's edit,
    // so neither a preset apply nor a mask-list/component action this frame
    // is lost when the active base tab itself produces no edit. The two can
    // never both be `Some` in the same frame (the Presets menu closes on
    // click), so the precedence between them is moot in practice.
    let mut out: Option<EditOutcome> = preset_edit.or(mask_panel_edit);
    // Base tabs:
    let mut rendered = false;
    for tab in reg.base_tabs() {
        if tab.id() == active {
            if let Some(edit) = tab.show(ui, state) {
                out = Some(edit);
            }
            rendered = true;
            break;
        }
    }
    // Active tool's temp tabs:
    if !rendered && ts.active != crate::develop::tool::ToolId::Adjust {
        if let Some(tool) = reg.get(ts.active) {
            for tab in tool.tabs() {
                if tab.id() == active {
                    if let Some(edit) = tab.show(ui, state) {
                        out = Some(edit);
                    }
                    break;
                }
            }
        }
    }

    // Write ToolState back.
    state.tool_state = ts;
    PanelOutcome {
        edit: out,
        working_space: ws_change,
    }
}

/// Test/verification-only mirror of the tab-row id list `show()` assembles
/// above (base tabs ++ the active tool's temporary tabs), gated by the same
/// `tab_row_visible`. Ids only, no labels: a `PanelTab::label()` borrows from
/// `tool.tabs()`'s freshly-allocated boxes, which can't outlive this
/// function — so this exists purely to make the Crop-suppresses-tabs
/// behavior assertable without spinning up an egui context.
#[cfg(test)]
fn tab_row_ids(
    ts: &crate::develop::tool_state::ToolState,
    reg: &DevelopToolRegistry,
) -> Vec<crate::develop::tool::TabId> {
    if !tab_row_visible(ts.active) {
        return Vec::new();
    }
    let mut ids: Vec<crate::develop::tool::TabId> =
        reg.base_tabs().iter().map(|t| t.id()).collect();
    if ts.active != ToolId::Adjust {
        if let Some(tool) = reg.get(ts.active) {
            ids.extend(tool.tabs().iter().map(|t| t.id()));
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::tool::{DevelopCtx, DevelopTool, PanelTab, TabId};
    use crate::develop::tool_state::ToolState;

    struct DummyTab(TabId);
    impl PanelTab for DummyTab {
        fn id(&self) -> TabId {
            self.0
        }
        fn label(&self) -> &str {
            "t"
        }
        fn show(&self, _ui: &mut egui::Ui, _s: &mut AppState) -> Option<EditOutcome> {
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
                Box::new(DummyTab(TabId("effects"))),
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
                    id: ToolId::Mask,
                    enabled: true,
                    tabs: vec![],
                }),
            ],
        )
    }

    #[test]
    fn tab_row_visible_is_false_only_for_crop() {
        assert!(tab_row_visible(ToolId::Adjust));
        assert!(tab_row_visible(ToolId::Mask));
        assert!(tab_row_visible(ToolId::Heal));
        assert!(!tab_row_visible(ToolId::Crop));
    }

    #[test]
    fn crop_active_shows_no_tab_row_items() {
        let reg = reg();
        let mut ts = ToolState::default();
        ts.select_tool(ToolId::Crop, true, &reg);
        assert!(
            tab_row_ids(&ts, &reg).is_empty(),
            "Crop suppresses the shared tab row entirely"
        );
    }

    #[test]
    fn adjust_and_mask_keep_the_unchanged_base_tab_row() {
        let reg = reg();
        let ts = ToolState::default();
        assert_eq!(
            tab_row_ids(&ts, &reg),
            vec![TabId("light"), TabId("color"), TabId("effects")]
        );

        let mut ts_mask = ToolState::default();
        ts_mask.select_tool(ToolId::Mask, true, &reg);
        assert_eq!(
            tab_row_ids(&ts_mask, &reg),
            vec![TabId("light"), TabId("color"), TabId("effects")],
            "Mask injects a header above the row (see the Mask branch), not tab \
             items — the base tab row itself is unchanged"
        );
    }
}
