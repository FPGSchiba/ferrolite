//! Develop right-panel Masks section (design §9.2): the masks list + Create,
//! per-row visibility / invert / rename / delete, and selection. The Light/
//! Color/Effects adjustments for the selected mask are edited through the
//! shared scoped base tabs above this block (Task 6) — this module only owns
//! mask management: create/overlay, the mask list, and the component-tools
//! entry point. Discrete actions emit a committed `EditOutcome` (kind =
//! LocalAdjustments); the app pushes one history entry each (per-gesture
//! sealing, Task 3).

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::mask_edit;
use crate::develop::mask_ui::MaskUiState;
use crate::settings::keymap::{Action, Keymap};
use crate::theme;
use ferrolite_pipeline::{OpKind, OpStack};

/// Brush-radius slider bounds (fraction of the image's smaller edge). Shared
/// with the canvas Ctrl+scroll brush-size gesture (`mask_overlay::route_brush`)
/// so both entry points clamp to the exact same range.
pub const BRUSH_RADIUS_MIN: f32 = 0.005;
pub const BRUSH_RADIUS_MAX: f32 = 0.5;

pub fn show(
    ui: &mut egui::Ui,
    stack: &OpStack,
    mask: &mut MaskUiState,
    keymap: &Keymap,
) -> Option<EditOutcome> {
    let la = mask_edit::layers(stack);
    mask.clamp_selection(la.layers.len());
    let mut out: Option<EditOutcome> = None;

    let commit = |s: OpStack| EditOutcome {
        stack: s,
        kind: OpKind::LocalAdjustments,
        commit: true,
    };

    ui.horizontal(|ui| {
        if ui.button("Create New Mask").clicked() {
            let name = format!("Mask {}", la.layers.len() + 1);
            mask.selected = Some(la.layers.len()); // select the new one
            mask.overlay_on = true;
            mask.components_modal_open = false;
            mask.editing_component = None;
            mask.preview_component = None;
            out = Some(commit(mask_edit::create_mask(stack, name)));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (icon, tip) = if mask.overlay_on {
                (crate::icons::OVERLAY_ON, "Hide mask overlay")
            } else {
                (crate::icons::OVERLAY_OFF, "Show mask overlay")
            };
            let tip = format!("{} ({})", tip, keymap.hint(Action::ToggleMaskOverlay));
            if crate::widgets::tool_button(ui, icon, &tip, mask.overlay_on, true, None).clicked() {
                mask.overlay_on = !mask.overlay_on;
            }
        });
    });

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
                    mask.components_modal_open = false;
                    mask.editing_component = None;
                    mask.preview_component = None;
                }
                if resp.double_clicked() {
                    mask.rename_buf = Some((i, layer.name.clone()));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(
                        egui::RichText::new(crate::icons::DELETE).font(crate::icons::font(12.0)),
                    ))
                    .on_hover_text("Delete mask")
                    .clicked()
                {
                    out = Some(commit(mask_edit::delete_mask(stack, i)));
                    if mask.selected == Some(i) {
                        mask.selected = None;
                        mask.components_modal_open = false;
                        mask.editing_component = None;
                        mask.preview_component = None;
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

    // The color eyedropper stages samples in `mask.color_samples` independent of
    // any selected mask (mask_overlay's routing lets it sample regardless of
    // selection so arming it never dead-ends silently). But "Add Color range"
    // lives in `selected_section` below, which only renders with a selection —
    // so if samples were collected while a mask was selected and the user then
    // deselects it (switches rows, or the mask is deleted), the button
    // disappears with them still queued. Surface that here so it's never a
    // silent dead end.
    if !mask.color_samples.is_empty() && mask.selected.is_none() {
        ui.label(
            egui::RichText::new(format!(
                "{} color sample(s) queued — select or create a mask to add them",
                mask.color_samples.len()
            ))
            .size(11.0)
            .color(theme::TEXT_FAINT),
        );
    }

    ui.add_space(6.0);
    // Selected-mask component tools (component count, Components window link,
    // New Brush Layer). Light/Color/Effects adjustments live in the shared
    // scoped base tabs above this block (Task 6).
    if let Some(idx) = mask.selected {
        if idx < la.layers.len() {
            if let Some(o) = selected_section(ui, stack, mask, idx, keymap) {
                out = Some(o);
            }
        }
    }

    out
}

/// The selected mask's component tools: component count, a link to the
/// Components window, and "New Brush Layer" (design §9.2). Component creation
/// (type picker, brush/luma/color params, add buttons) lives in
/// `mask_components_modal` (Task 5). Light/Color/Effects adjustments for the
/// selected mask are no longer rendered here — they're the shared scoped base
/// tabs above this block in `tool_panel::show` (Task 6).
pub(crate) fn selected_section(
    ui: &mut egui::Ui,
    stack: &OpStack,
    mask: &mut MaskUiState,
    idx: usize,
    keymap: &Keymap,
) -> Option<EditOutcome> {
    let la = mask_edit::layers(stack);
    let layer = &la.layers[idx];
    let mut out: Option<EditOutcome> = None;

    ui.separator();

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{} components", layer.mask.components.len()))
                .size(11.0)
                .color(theme::TEXT_FAINT),
        );
        if crate::widgets::tool_button(ui, crate::icons::EDIT, "Components", false, true, None)
            .clicked()
        {
            mask.components_modal_open = true;
            mask.overlay_on = true; // show coverage/live-preview while working on components
        }
        let new_layer_label = format!("New Brush Layer ({})", keymap.hint(Action::NewBrushLayer));
        if ui
            .button(new_layer_label)
            .on_hover_text("Start a new, separately-deletable brush layer")
            .clicked()
        {
            out = Some(EditOutcome {
                stack: mask_edit::new_brush_layer(stack, idx),
                kind: OpKind::LocalAdjustments,
                commit: true,
            });
        }
    });

    out
}
