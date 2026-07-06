//! Develop right adjustment panel chrome (design-system §6/§7). The section
//! bodies that used to live here (Basic/Tone/HSL/Masks/Detail/Lens/Geometry) have
//! moved to `base_tabs.rs` (Task 5) and the Crop/Mask tools (Tasks 6/7); this file
//! now keeps only the shared `EditOutcome`/`PanelOutcome` result types and the
//! top-of-panel `chrome()` helper (camera/coverage line, save-state indicator,
//! working-space combo) rendered by `tool_panel::show` above the tab bar.

use crate::develop::coverage;
use crate::state::AppState;
use crate::theme;
use ferrolite_color::WorkingSpace;
use ferrolite_pipeline::{OpKind, OpStack};

pub struct EditOutcome {
    pub stack: OpStack,
    pub kind: OpKind,
    pub commit: bool,
}

/// What the adjustment panel produced this frame: an op edit and/or a working-space change.
pub struct PanelOutcome {
    pub edit: Option<EditOutcome>,
    pub working_space: Option<WorkingSpace>,
}

/// The Develop right-panel's global chrome (design §7): camera/coverage line,
/// save-state indicator, and the working-space combo. Rendered ABOVE the tab bar
/// by `tool_panel::show` — not tool-specific, so it stays outside the tab dispatch.
/// Returns `Some(new_ws)` when the working-space combo changed this frame.
pub(crate) fn chrome(
    ui: &mut egui::Ui,
    state: &mut AppState,
    working_space: WorkingSpace,
) -> Option<WorkingSpace> {
    let mut ws_change: Option<WorkingSpace> = None;

    // ── Camera info + color-profile coverage status (Spec 4.6 §3) ──
    // Read-only indicator (NOT an adjustable control → no per-control reset).
    // Shows "make model"; when a RAW decoded without a usable camera matrix
    // (sRGB fallback in effect), appends a warning chip + hover tooltip. All
    // reads are O(1) — no decode/I/O is triggered from the UI thread.
    if let Some(v) = state.viewer.as_ref() {
        let status = coverage::camera_coverage(v.kind, v.full_ready, v.color_profile.is_fallback);
        let name = v
            .meta
            .as_ref()
            .map(|m| format!("{} {}", m.make, m.model).trim().to_string())
            .filter(|s| !s.is_empty());
        if name.is_some() || status != coverage::CoverageStatus::NotApplicable {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if let Some(name) = &name {
                    ui.label(egui::RichText::new(name).color(theme::TEXT_DIM).size(11.0));
                }
                if let Some(label) = status.chip_label() {
                    let chip = ui.label(
                        egui::RichText::new(label)
                            .color(theme::SEMANTIC_RED)
                            .size(11.0),
                    );
                    if let Some(tip) = status.tooltip() {
                        chip.on_hover_text(tip);
                    }
                }
            });
            ui.add_space(4.0);
        }
    }

    // ── Save-state indicator ──
    // Edits auto-save: each commit calls persist_ops → spawn_ops_write off-thread.
    // This compact line surfaces the current save state so the author can confirm
    // that edits are being persisted (there is no manual Ctrl+S).
    {
        let image_id = state.viewer.as_ref().map(|v| v.image_id);
        let has_edits = image_id
            .and_then(|id| state.images.iter().find(|r| r.id == id))
            .map(|r| r.has_edits)
            .unwrap_or(false);

        let (label, color) = if state.ops_save_inflight > 0 {
            ("Saving\u{2026}", theme::TEXT_DIM)
        } else if state.ops_save_failed {
            ("Save failed", theme::SEMANTIC_RED)
        } else if has_edits {
            ("Saved", theme::SEMANTIC_GREEN)
        } else {
            ("No edits", theme::TEXT_FAINT)
        };

        ui.add_space(2.0);
        ui.label(egui::RichText::new(label).color(color).size(11.0));
        ui.add_space(4.0);
    }

    // ── Working space (spec §4.1) ── global preference; not an editable op, so no
    // per-control reset. Recomposes the ColorMatrixNode + display tail on change.
    {
        let mut ws = working_space;
        egui::ComboBox::from_label("Working space")
            .selected_text(format!("{ws:?}"))
            .show_ui(ui, |ui| {
                for w in WorkingSpace::ALL {
                    ui.selectable_value(&mut ws, w, format!("{w:?}"));
                }
            });
        if ws != working_space {
            ws_change = Some(ws);
        }
        ui.add_space(4.0);
    }

    ws_change
}
