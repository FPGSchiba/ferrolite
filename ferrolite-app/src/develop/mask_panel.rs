//! Develop right-panel Masks section (design §9.2): the masks list + Create,
//! per-row visibility / invert / rename / delete, and selection. The selected
//! mask's Light+Color set + component tools live in `selected_section` (Task 8).
//! Discrete actions emit a committed `EditOutcome` (kind = LocalAdjustments);
//! the app pushes one history entry each (per-gesture sealing, Task 3).

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::mask_edit;
use crate::develop::mask_ui::MaskUiState;
use crate::settings::keymap::{Action, Keymap};
use crate::theme;
use crate::widgets::slider::EguiSlider;
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
    // Selected-mask section (component tools + Light+Color) — Task 8.
    if let Some(idx) = mask.selected {
        if idx < la.layers.len() {
            if let Some(o) = selected_section(ui, stack, mask, idx, keymap) {
                out = Some(o);
            }
        }
    }

    out
}

/// The selected mask's Light/Color adjustments + a link to the Components
/// window (design §9.2). Component creation (type picker, brush/luma/color
/// params, add buttons) lives in `mask_components_modal` (Task 5) — this
/// section only shows the component count and a button to open that window.
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

    ui.separator();

    // ── Light + Color adjustments (each slider carries its own reset column) ──
    let mut a = layer.adjustments;
    let mut changed = false;
    let mut commit_now = false;
    let mut adjusting = false;
    let slider = |ui: &mut egui::Ui,
                  label: &str,
                  v: &mut f32,
                  min: f32,
                  max: f32,
                  bip: bool,
                  changed: &mut bool,
                  commit_now: &mut bool,
                  adjusting: &mut bool| {
        let r = ui.add(EguiSlider {
            label,
            value: v,
            min,
            max,
            default: 0.0,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: bip,
            signed: bip,
            custom_label_w: None,
        });
        if r.dragged() {
            *adjusting = true;
        }
        if r.changed() {
            *changed = true;
            if r.drag_stopped() || !r.dragged() {
                *commit_now = true;
            }
        }
    };

    ui.label(
        egui::RichText::new("Light")
            .size(11.0)
            .color(theme::TEXT_DIM),
    );
    slider(
        ui,
        "Exposure",
        &mut a.exposure,
        -5.0,
        5.0,
        true,
        &mut changed,
        &mut commit_now,
        &mut adjusting,
    );
    slider(
        ui,
        "Contrast",
        &mut a.contrast,
        -1.0,
        1.0,
        true,
        &mut changed,
        &mut commit_now,
        &mut adjusting,
    );
    slider(
        ui,
        "Highlights",
        &mut a.highlights,
        -1.0,
        1.0,
        true,
        &mut changed,
        &mut commit_now,
        &mut adjusting,
    );
    slider(
        ui,
        "Shadows",
        &mut a.shadows,
        -1.0,
        1.0,
        true,
        &mut changed,
        &mut commit_now,
        &mut adjusting,
    );
    slider(
        ui,
        "Whites",
        &mut a.whites,
        -1.0,
        1.0,
        true,
        &mut changed,
        &mut commit_now,
        &mut adjusting,
    );
    slider(
        ui,
        "Blacks",
        &mut a.blacks,
        -1.0,
        1.0,
        true,
        &mut changed,
        &mut commit_now,
        &mut adjusting,
    );

    ui.label(
        egui::RichText::new("Color")
            .size(11.0)
            .color(theme::TEXT_DIM),
    );
    slider(
        ui,
        "Temp",
        &mut a.temp,
        -1.0,
        1.0,
        true,
        &mut changed,
        &mut commit_now,
        &mut adjusting,
    );
    slider(
        ui,
        "Tint",
        &mut a.tint,
        -1.0,
        1.0,
        true,
        &mut changed,
        &mut commit_now,
        &mut adjusting,
    );
    slider(
        ui,
        "Saturation",
        &mut a.saturation,
        -1.0,
        1.0,
        true,
        &mut changed,
        &mut commit_now,
        &mut adjusting,
    );
    slider(
        ui,
        "Hue",
        &mut a.hue,
        -1.0,
        1.0,
        true,
        &mut changed,
        &mut commit_now,
        &mut adjusting,
    );
    // "Color" swatch amount (RGB picked via the swatch below).
    let mut amt = a.color.amount;
    let r = ui.add(EguiSlider {
        label: "Color",
        value: &mut amt,
        min: 0.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: false,
        signed: false,
        custom_label_w: None,
    });
    if r.dragged() {
        adjusting = true;
    }
    if r.changed() {
        a.color.amount = amt;
        changed = true;
        if r.drag_stopped() || !r.dragged() {
            commit_now = true;
        }
    }
    let mut rgb = [a.color.r, a.color.g, a.color.b];
    if ui.color_edit_button_rgb(&mut rgb).changed() {
        a.color.r = rgb[0];
        a.color.g = rgb[1];
        a.color.b = rgb[2];
        changed = true;
        commit_now = true;
    }
    mask.adjusting = adjusting;

    // ── Reserved neighborhood controls: greyed, hover reason (design §9.2) ──
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("Effects")
            .size(11.0)
            .color(theme::TEXT_DIM),
    );
    for name in ["Texture", "Clarity", "Dehaze", "Sharpness", "Noise"] {
        let mut dummy = 0.0f32;
        ui.add_enabled_ui(false, |ui| {
            ui.add(EguiSlider {
                label: name,
                value: &mut dummy,
                min: -1.0,
                max: 1.0,
                default: 0.0,
                step: 0.01,
                decimals: 2,
                unit: "",
                bipolar: true,
                signed: true,
                custom_label_w: None,
            });
        })
        .response
        .on_hover_text("Coming in a later phase (needs neighborhood processing)");
    }

    if changed {
        out = Some(EditOutcome {
            stack: mask_edit::set_adjustments(stack, idx, a),
            kind: OpKind::LocalAdjustments,
            commit: commit_now,
        });
    }
    out
}
