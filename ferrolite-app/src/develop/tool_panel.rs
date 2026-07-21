//! The Develop right-panel shell (design §7): global chrome + a shared tab bar (base
//! adjustment tabs ++ the active canvas tool's temporary tab) + active-tab dispatch.
//! Replaces the flat CollapsingHeader `adjustment_panel::show`.

use crate::develop::adjustment_panel::{EditOutcome, PanelOutcome};
use crate::develop::tool::DevelopToolRegistry;
use crate::state::AppState;
use ferrolite_color::WorkingSpace;
use ferrolite_pipeline::{OpKind, OpStack};

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    reg: &DevelopToolRegistry,
    working_space: WorkingSpace,
) -> PanelOutcome {
    // 1) Global chrome (not tool-specific) stays above the tab bar.
    let ws_change = crate::develop::adjustment_panel::chrome(ui, state, working_space);
    ui.separator();

    // 2) Copy ToolState out (it is Copy) so the tab-bar mutation doesn't fight the
    //    &mut AppState borrow used by the active tab's show(). Session-wide (on
    //    AppState, not ViewerState) so it survives image switches.
    if state.viewer.is_none() {
        return PanelOutcome {
            edit: None,
            working_space: ws_change,
        };
    }
    let mut ts = state.tool_state;
    ts.ensure_valid_tab(reg);

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

    // 3) Dispatch the active tab's show(). Look the tab object up fresh (base ++ active
    //    tool tabs) and call it.
    let active = ts.active_tab;
    let mut out: Option<EditOutcome> = None;
    // Base tabs:
    let mut rendered = false;
    for tab in reg.base_tabs() {
        if tab.id() == active {
            out = tab.show(ui, state);
            rendered = true;
            break;
        }
    }
    // Active tool's temp tabs:
    if !rendered && ts.active != crate::develop::tool::ToolId::Adjust {
        if let Some(tool) = reg.get(ts.active) {
            for tab in tool.tabs() {
                if tab.id() == active {
                    out = tab.show(ui, state);
                    break;
                }
            }
        }
    }

    // 4) Global chrome (not tool/tab-specific): reset the entire OpStack to default.
    //    Placed after the active tab's dispatch so a click here wins over any edit the
    //    tab produced this frame (last-writer-wins, matching the pre-migration behavior).
    ui.separator();
    if ui.button("Reset all").clicked() {
        out = Some(EditOutcome {
            stack: OpStack::default(),
            kind: OpKind::Exposure,
            commit: true,
        });
    }

    // Write ToolState back.
    state.tool_state = ts;
    PanelOutcome {
        edit: out,
        working_space: ws_change,
    }
}
