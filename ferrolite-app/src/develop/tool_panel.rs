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
    //    &mut AppState borrow used by the active tab's show().
    let Some(mut ts) = state.viewer.as_ref().map(|v| v.tool_state) else {
        return PanelOutcome {
            edit: None,
            working_space: ws_change,
        };
    };
    ts.ensure_valid_tab(reg);
    let bar = ts.tab_bar(reg);
    let base_len = reg.base_tabs().len();

    // Render the tab bar (base tabs, then a separator + the active tool's temp tab).
    ui.horizontal_wrapped(|ui| {
        for (i, id) in bar.iter().enumerate() {
            if i == base_len && i > 0 {
                ui.separator(); // visual break before the active tool's temp tab
            }
            let label = tab_label(reg, ts, *id);
            if ui.selectable_label(ts.active_tab == *id, label).clicked() {
                ts.select_tab(*id, reg);
            }
        }
    });
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
    if let Some(v) = state.viewer.as_mut() {
        v.tool_state = ts;
    }
    PanelOutcome {
        edit: out,
        working_space: ws_change,
    }
}

fn tab_label(
    reg: &DevelopToolRegistry,
    ts: crate::develop::tool_state::ToolState,
    id: crate::develop::tool::TabId,
) -> String {
    for tab in reg.base_tabs() {
        if tab.id() == id {
            return tab.label().to_string();
        }
    }
    if let Some(tool) = reg.get(ts.active) {
        for tab in tool.tabs() {
            if tab.id() == id {
                return tab.label().to_string();
            }
        }
    }
    id.0.to_string()
}
