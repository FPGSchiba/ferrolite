//! Develop right-panel Masks section (design §9.2): the masks list + Create,
//! per-row visibility / invert / rename / delete, and selection. The selected
//! mask's Light+Color set + component tools live in `selected_section` (Task 8).
//! Discrete actions emit a committed `EditOutcome` (kind = LocalAdjustments);
//! the app pushes one history entry each (per-gesture sealing, Task 3).

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::mask_edit;
use crate::develop::mask_ui::MaskUiState;
use crate::theme;
use ferrolite_pipeline::{OpKind, OpStack};

pub fn show(ui: &mut egui::Ui, stack: &OpStack, mask: &mut MaskUiState) -> Option<EditOutcome> {
    let la = mask_edit::layers(stack);
    mask.clamp_selection(la.layers.len());
    let mut out: Option<EditOutcome> = None;

    let commit = |s: OpStack| EditOutcome {
        stack: s,
        kind: OpKind::LocalAdjustments,
        commit: true,
    };

    if ui.button("Create New Mask").clicked() {
        let name = format!("Mask {}", la.layers.len() + 1);
        mask.selected = Some(la.layers.len()); // select the new one
        out = Some(commit(mask_edit::create_mask(stack, name)));
    }

    ui.add_space(4.0);

    // Masks list. Short (a handful of layers) so plain iteration is fine.
    for (i, layer) in la.layers.iter().enumerate() {
        ui.horizontal(|ui| {
            // Visibility toggle (eye).
            let mut vis = layer.visible;
            if ui.checkbox(&mut vis, "").changed() {
                out = Some(commit(mask_edit::set_visible(stack, i, vis)));
            }
            // Invert toggle.
            let mut inv = layer.mask.invert;
            if ui.selectable_label(inv, "Inv").clicked() {
                inv = !inv;
                out = Some(commit(mask_edit::set_invert(stack, i, inv)));
            }
            // Name / rename.
            let renaming = matches!(&mask.rename_buf, Some((idx, _)) if *idx == i);
            if renaming {
                if let Some((_, buf)) = mask.rename_buf.as_mut() {
                    let te = ui.text_edit_singleline(buf);
                    if te.lost_focus() {
                        let name = buf.clone();
                        mask.rename_buf = None;
                        if !name.trim().is_empty() {
                            out = Some(commit(mask_edit::rename(stack, i, name)));
                        }
                    }
                }
            } else {
                let selected = mask.selected == Some(i);
                let resp = ui.selectable_label(selected, &layer.name);
                if resp.clicked() {
                    mask.selected = Some(i);
                }
                if resp.double_clicked() {
                    mask.rename_buf = Some((i, layer.name.clone()));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("\u{1f5d1}")
                    .on_hover_text("Delete mask")
                    .clicked()
                {
                    out = Some(commit(mask_edit::delete_mask(stack, i)));
                    if mask.selected == Some(i) {
                        mask.selected = None;
                    }
                }
            });
        });
    }

    if la.layers.is_empty() {
        ui.label(
            egui::RichText::new("No masks yet")
                .color(theme::TEXT_FAINT)
                .size(11.0),
        );
    }

    ui.add_space(6.0);
    // Selected-mask section (component tools + Light+Color) — Task 8.
    if let Some(idx) = mask.selected {
        if idx < la.layers.len() {
            if let Some(o) = selected_section(ui, stack, mask, idx) {
                out = Some(o);
            }
        }
    }

    out
}

/// Placeholder until Task 8 fills it in. Returns None so the list works standalone.
pub(crate) fn selected_section(
    _ui: &mut egui::Ui,
    _stack: &OpStack,
    _mask: &mut MaskUiState,
    _idx: usize,
) -> Option<EditOutcome> {
    None
}
