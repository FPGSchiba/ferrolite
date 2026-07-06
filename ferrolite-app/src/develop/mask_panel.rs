//! Develop right-panel Masks section (design §9.2): the masks list + Create,
//! per-row visibility / invert / rename / delete, and selection. The selected
//! mask's Light+Color set + component tools live in `selected_section` (Task 8).
//! Discrete actions emit a committed `EditOutcome` (kind = LocalAdjustments);
//! the app pushes one history entry each (per-gesture sealing, Task 3).

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::mask_edit;
use crate::develop::mask_ui::{MaskTool, MaskUiState};
use crate::theme;
use crate::widgets::slider::EguiSlider;
use ferrolite_mask::{CompositeMode, MaskComponent};
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

/// The selected mask's component tools + Light/Color adjustments (design §9.2).
/// Non-canvas components (Luma-range, using the current slider values) can be
/// added directly from the panel; Brush/Linear/Radial/Color-range are captured
/// on the canvas (Tasks 10-12) — this picker only selects the tool + composite
/// mode so the canvas overlay knows what to create.
pub(crate) fn selected_section(
    ui: &mut egui::Ui,
    stack: &OpStack,
    mask: &mut MaskUiState,
    idx: usize,
) -> Option<EditOutcome> {
    let la = mask_edit::layers(stack);
    let layer = &la.layers[idx];
    let mut out: Option<EditOutcome> = None;
    let commit = |s: OpStack| EditOutcome {
        stack: s,
        kind: OpKind::LocalAdjustments,
        commit: true,
    };

    ui.separator();

    // ── Component tool picker + composite mode ──
    ui.horizontal(|ui| {
        for (tool, label) in [
            (MaskTool::Brush, "Brush"),
            (MaskTool::Linear, "Linear"),
            (MaskTool::Radial, "Radial"),
            (MaskTool::LumaRange, "Luma"),
            (MaskTool::ColorRange, "Color"),
        ] {
            if ui.selectable_label(mask.tool == tool, label).clicked() {
                mask.tool = tool;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Add mode")
                .size(11.0)
                .color(theme::TEXT_DIM),
        );
        for (m, label) in [
            (CompositeMode::Add, "Add"),
            (CompositeMode::Subtract, "Subtract"),
            (CompositeMode::Intersect, "Intersect"),
        ] {
            if ui.selectable_label(mask.next_mode == m, label).clicked() {
                mask.next_mode = m;
            }
        }
    });

    // Brush params: captured live by the canvas overlay (Task 11), so no "Add"
    // button here — these sliders just set the radius/hardness/flow/erase used
    // for the NEXT stroke (and shown by the cursor ring while brushing).
    if mask.tool == MaskTool::Brush {
        ui.add(EguiSlider {
            label: "Radius",
            value: &mut mask.brush_radius,
            min: 0.005,
            max: 0.5,
            default: 0.08,
            step: 0.005,
            decimals: 3,
            unit: "",
            bipolar: false,
            signed: false,
        });
        ui.add(EguiSlider {
            label: "Hardness",
            value: &mut mask.brush_hardness,
            min: 0.0,
            max: 1.0,
            default: 0.5,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: false,
            signed: false,
        });
        ui.add(EguiSlider {
            label: "Flow",
            value: &mut mask.brush_flow,
            min: 0.0,
            max: 1.0,
            default: 1.0,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: false,
            signed: false,
        });
        ui.checkbox(&mut mask.brush_erase, "Erase");
    }

    // Luma-range can be added directly from the panel with the current slider
    // values (it needs no canvas gesture). The other tools are captured on the
    // canvas (Tasks 10-12); the tool+mode selection above tells the overlay what
    // to create. Show the range params + an "Add component" button when Luma is
    // the active tool.
    if mask.tool == MaskTool::LumaRange {
        ui.add(EguiSlider {
            label: "Lo",
            value: &mut mask.range_lo,
            min: 0.0,
            max: 1.0,
            default: 0.3,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: false,
            signed: false,
        });
        ui.add(EguiSlider {
            label: "Hi",
            value: &mut mask.range_hi,
            min: 0.0,
            max: 1.0,
            default: 0.7,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: false,
            signed: false,
        });
        ui.add(EguiSlider {
            label: "Softness",
            value: &mut mask.range_softness,
            min: 0.0,
            max: 0.5,
            default: 0.1,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: false,
            signed: false,
        });
        if ui.button("Add Luma range").clicked() {
            let c = MaskComponent::LumaRange {
                lo: mask.range_lo,
                hi: mask.range_hi,
                softness: mask.range_softness,
            };
            out = Some(commit(mask_edit::add_component(
                stack,
                idx,
                c,
                mask.next_mode,
            )));
        }
    }

    // Color-range: samples are collected by clicking the canvas with the
    // eyedropper (mask_overlay's `route_color_eyedropper`, UI state only — no
    // OpStack edit on pick). Show the collected swatches + Tolerance/Softness,
    // then "Add Color range" commits the component and clears the samples.
    if mask.tool == MaskTool::ColorRange {
        if mask.color_samples.is_empty() {
            ui.label(
                egui::RichText::new("Click the image to sample colors")
                    .size(11.0)
                    .color(theme::TEXT_FAINT),
            );
        } else {
            ui.horizontal(|ui| {
                for s in &mask.color_samples {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    ui.painter().rect_filled(
                        rect,
                        2.0,
                        egui::Color32::from_rgb(
                            (s.r.clamp(0.0, 1.0) * 255.0) as u8,
                            (s.g.clamp(0.0, 1.0) * 255.0) as u8,
                            (s.b.clamp(0.0, 1.0) * 255.0) as u8,
                        ),
                    );
                }
                if ui.small_button("Clear").clicked() {
                    mask.color_samples.clear();
                }
            });
        }
        ui.add(EguiSlider {
            label: "Tolerance",
            value: &mut mask.color_tolerance,
            min: 0.0,
            max: 1.0,
            default: 0.15,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: false,
            signed: false,
        });
        ui.add(EguiSlider {
            label: "Softness",
            value: &mut mask.color_softness,
            min: 0.0,
            max: 0.5,
            default: 0.1,
            step: 0.01,
            decimals: 2,
            unit: "",
            bipolar: false,
            signed: false,
        });
        let can_add = !mask.color_samples.is_empty();
        if ui
            .add_enabled(can_add, egui::Button::new("Add Color range"))
            .clicked()
        {
            let c = MaskComponent::ColorRange {
                samples: mask.color_samples.clone(),
                tolerance: mask.color_tolerance,
                softness: mask.color_softness,
            };
            out = Some(commit(mask_edit::add_component(
                stack,
                idx,
                c,
                mask.next_mode,
            )));
            mask.color_samples.clear();
        }
    }

    ui.label(
        egui::RichText::new(format!("{} components", layer.mask.components.len()))
            .size(11.0)
            .color(theme::TEXT_FAINT),
    );

    ui.separator();

    // ── Light + Color adjustments (each slider carries its own reset column) ──
    let mut a = layer.adjustments;
    let mut changed = false;
    let mut commit_now = false;
    let slider = |ui: &mut egui::Ui,
                  label: &str,
                  v: &mut f32,
                  min: f32,
                  max: f32,
                  bip: bool,
                  changed: &mut bool,
                  commit_now: &mut bool| {
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
        });
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
    });
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
