//! The Develop right-panel shell (design §7): global chrome + a shared tab bar (base
//! adjustment tabs ++ the active canvas tool's temporary tab) + active-tab dispatch.
//! Replaces the flat CollapsingHeader `adjustment_panel::show`.

use crate::develop::adjustment_panel::{EditOutcome, PanelOutcome};
use crate::develop::tool::{DevelopToolRegistry, ToolId};
use crate::state::AppState;
use crate::theme;
use ferrolite_color::WorkingSpace;

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
    // Seed with the mask panel's edit (if any) so a mask-list/component action
    // this frame isn't lost when the active base tab itself produces no edit.
    let mut out: Option<EditOutcome> = mask_panel_edit;
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
