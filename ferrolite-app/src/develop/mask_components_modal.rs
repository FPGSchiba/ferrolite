//! Non-blocking window for a selected mask's components: list + delete (any) +
//! edit (Luma/Color via set_component) + add-new (all types). Keeps the 296px
//! panel uncluttered (design §9.2). Editing happens IN the window (its own
//! sliders), not by routing back to the panel's Luma/Color sliders — the
//! window is self-contained. Non-blocking (not suppressed via `modal_active`)
//! so the canvas stays live behind it for brush drawing, gradient handles, and
//! color-eyedropper sampling while adding a new component.
//!
//! Also owns the component-CREATION add-flow (moved from `mask_panel` — see
//! Task 5): a type picker, composite-mode selector, and per-type params.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::mask_panel::{BRUSH_RADIUS_MAX, BRUSH_RADIUS_MIN};
use crate::develop::mask_ui::MaskTool;
use crate::develop::{mask_edit, mask_ui::MaskUiState};
use crate::theme;
use crate::widgets::EguiSlider;
use ferrolite_mask::{CompositeMode, MaskComponent};
use ferrolite_pipeline::{OpKind, OpStack};

/// Render the modal if `mask.components_modal_open`. Returns an edit if one was made.
pub fn show(ctx: &egui::Context, stack: &OpStack, mask: &mut MaskUiState) -> Option<EditOutcome> {
    if !mask.components_modal_open {
        return None;
    }
    let Some(mask_idx) = mask.selected else {
        mask.components_modal_open = false; // nothing selected -> close
        mask.editing_component = None;
        mask.preview_component = None;
        return None;
    };
    let layers = mask_edit::layers(stack);
    let Some(layer) = layers.layers.get(mask_idx) else {
        mask.components_modal_open = false;
        mask.editing_component = None;
        mask.preview_component = None;
        return None;
    };
    let components = layer.mask.components.clone();
    let layer_name = layer.name.clone();

    let mut out: Option<EditOutcome> = None;
    let mut open = true;
    egui::Window::new(format!("Components — {layer_name}"))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            if components.is_empty() {
                ui.label(
                    egui::RichText::new("No components yet")
                        .size(11.0)
                        .color(crate::theme::TEXT_FAINT),
                );
            }
            for (i, (comp, mode)) in components.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "{}. {}  [{:?}]",
                        i + 1,
                        component_label(comp),
                        mode
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::widgets::tool_button(
                            ui,
                            crate::icons::DELETE,
                            "Remove",
                            false,
                            true,
                            None,
                        )
                        .clicked()
                        {
                            out =
                                Some(commit_edit(mask_edit::remove_component(stack, mask_idx, i)));
                            if mask.editing_component == Some(i) {
                                mask.editing_component = None;
                            }
                        }
                        if is_editable(comp)
                            && crate::widgets::tool_button(
                                ui,
                                crate::icons::EDIT,
                                "Edit",
                                mask.editing_component == Some(i),
                                true,
                                None,
                            )
                            .clicked()
                        {
                            mask.editing_component = Some(i);
                            load_component_into_state(comp, mask); // prime the sliders
                        }
                    });
                });
            }
            // Inline editor for the component being edited (Luma/Color only).
            if let Some(i) = mask.editing_component {
                if let Some((comp, _mode)) = components.get(i) {
                    ui.separator();
                    if let Some(updated) = edit_component_ui(ui, comp, mask) {
                        out = Some(commit_edit(mask_edit::set_component(
                            stack, mask_idx, i, updated,
                        )));
                        mask.editing_component = None;
                    }
                }
            }

            // ── Add new component (relocated from `mask_panel::selected_section`,
            // Task 5) ── Suppressed while editing an existing component: the
            // add section binds the same `mask.range_*`/`mask.color_*` slider
            // fields as the edit-in-place UI above and writes
            // `mask.preview_component` every frame for Luma/Color, so running
            // it during an edit would overlay a phantom duplicate of the
            // component being edited on top of the true edit result.
            if mask.editing_component.is_none() {
                ui.separator();
                ui.label(
                    egui::RichText::new("Add new component")
                        .size(11.0)
                        .color(theme::TEXT_DIM),
                );
                if let Some(o) = add_component_ui(ui, stack, mask, mask_idx) {
                    out = Some(o);
                }
            } else {
                // While editing an existing component, no add-preview.
                mask.preview_component = None;
            }
        });
    if !open {
        mask.components_modal_open = false;
        mask.editing_component = None;
        mask.preview_component = None;
    }
    out
}

/// Human-readable label for a component's type (design §9.2 list rows).
fn component_label(c: &MaskComponent) -> &'static str {
    match c {
        MaskComponent::Brush { .. } => "Brush",
        MaskComponent::LinearGradient { .. } => "Linear gradient",
        MaskComponent::RadialGradient { .. } => "Radial gradient",
        MaskComponent::LumaRange { .. } => "Luminance range",
        MaskComponent::ColorRange { .. } => "Color range",
        MaskComponent::Imported { .. } => "Imported",
    }
}

/// Only Luma/Color ranges have re-editable scalar params; the others are
/// canvas-authored geometry/strokes with no modal editor (yet).
fn is_editable(c: &MaskComponent) -> bool {
    matches!(
        c,
        MaskComponent::LumaRange { .. } | MaskComponent::ColorRange { .. }
    )
}

/// Copy a Luma/Color component's params into the shared slider state so the
/// modal's editor (and `edit_component_ui`) starts from the component's
/// current values rather than whatever was last left in the panel sliders.
fn load_component_into_state(c: &MaskComponent, mask: &mut MaskUiState) {
    match c {
        MaskComponent::LumaRange { lo, hi, softness } => {
            mask.range_lo = *lo;
            mask.range_hi = *hi;
            mask.range_softness = *softness;
        }
        MaskComponent::ColorRange {
            samples,
            tolerance,
            softness,
        } => {
            mask.color_samples = samples.clone();
            mask.color_tolerance = *tolerance;
            mask.color_softness = *softness;
        }
        _ => {}
    }
}

/// Pure rebuild: a `LumaRange` component from the current slider state
/// (`load_component_into_state`'s inverse). Kept egui-free so it's unit-testable.
fn luma_from_state(mask: &MaskUiState) -> MaskComponent {
    MaskComponent::LumaRange {
        lo: mask.range_lo,
        hi: mask.range_hi,
        softness: mask.range_softness,
    }
}

/// Pure rebuild: a `ColorRange` component from the current slider/sample state
/// (`load_component_into_state`'s inverse). Kept egui-free so it's unit-testable.
fn color_from_state(mask: &MaskUiState) -> MaskComponent {
    MaskComponent::ColorRange {
        samples: mask.color_samples.clone(),
        tolerance: mask.color_tolerance,
        softness: mask.color_softness,
    }
}

/// Render the Luma/Color editor for `comp` (values seeded via
/// `load_component_into_state`). Returns `Some(rebuilt component)` when
/// "Update" is clicked, or `None` (with `editing_component` cleared) when
/// "Cancel" is clicked or nothing happened yet this frame.
fn edit_component_ui(
    ui: &mut egui::Ui,
    comp: &MaskComponent,
    mask: &mut MaskUiState,
) -> Option<MaskComponent> {
    let mut result = None;
    match comp {
        MaskComponent::LumaRange { .. } => {
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
            ui.horizontal(|ui| {
                if ui.button("Update").clicked() {
                    result = Some(luma_from_state(mask));
                }
                if ui.button("Cancel").clicked() {
                    mask.editing_component = None;
                }
            });
        }
        MaskComponent::ColorRange { .. } => {
            if !mask.color_samples.is_empty() {
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
            let pick_label = if mask.picking_color {
                format!("{} Picking… (click image)", crate::icons::EYEDROPPER)
            } else {
                format!("{} Pick color", crate::icons::EYEDROPPER)
            };
            if ui
                .selectable_label(mask.picking_color, pick_label)
                .clicked()
            {
                mask.picking_color = !mask.picking_color;
                if mask.picking_color {
                    mask.tool = MaskTool::ColorRange; // so mask_overlay::show routes the eyedropper
                }
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
            ui.horizontal(|ui| {
                if ui.button("Update").clicked() {
                    result = Some(color_from_state(mask));
                    mask.picking_color = false;
                }
                if ui.button("Cancel").clicked() {
                    mask.editing_component = None;
                    mask.picking_color = false;
                }
            });
        }
        _ => {}
    }
    result
}

/// The "Add new component" section: type picker + composite-mode selector,
/// then per-type params (relocated verbatim from
/// `mask_panel::selected_section` — Task 5). `LumaRange`/`ColorRange` commit
/// an edit directly from here; `Brush`/`Linear`/`Radial` only set `mask.tool`
/// so the existing canvas affordances (drawn on the image) create them — this
/// window stays open while the user draws since it's non-blocking.
fn add_component_ui(
    ui: &mut egui::Ui,
    stack: &OpStack,
    mask: &mut MaskUiState,
    idx: usize,
) -> Option<EditOutcome> {
    let mut out: Option<EditOutcome> = None;

    // ── Component type picker + composite mode ──
    ui.horizontal(|ui| {
        for (tool, icon, tip) in [
            (MaskTool::Brush, crate::icons::BRUSH, "Brush"),
            (
                MaskTool::Linear,
                crate::icons::LINEAR_GRADIENT,
                "Linear gradient",
            ),
            (
                MaskTool::Radial,
                crate::icons::RADIAL_GRADIENT,
                "Radial gradient",
            ),
            (MaskTool::LumaRange, crate::icons::LUMA, "Luminance range"),
            (MaskTool::ColorRange, crate::icons::COLOR, "Color range"),
        ] {
            if crate::widgets::tool_button(ui, icon, tip, mask.tool == tool, true, None).clicked() {
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

    // Live preview (Task 6): while tuning a Luma/Color add, feed the tentative
    // component to `mask.preview_component` each frame so the canvas overlay can
    // composite the prospective full mask (existing components + this one at its
    // mode). Brush/Linear/Radial are canvas-authored geometry with no scalar
    // params to preview here, so no preview is set for them.
    match mask.tool {
        MaskTool::LumaRange => {
            mask.preview_component = Some((luma_from_state(mask), mask.next_mode));
        }
        MaskTool::ColorRange => {
            mask.preview_component = Some((color_from_state(mask), mask.next_mode));
        }
        MaskTool::Brush | MaskTool::Linear | MaskTool::Radial => {
            mask.preview_component = None;
        }
    }

    // Brush params: captured live by the canvas overlay, so no "Add" button
    // here — these sliders just set the radius/hardness/flow/erase used for
    // the NEXT stroke (and shown by the cursor ring while brushing).
    if mask.tool == MaskTool::Brush {
        ui.add(EguiSlider {
            label: "Radius",
            value: &mut mask.brush_radius,
            min: BRUSH_RADIUS_MIN,
            max: BRUSH_RADIUS_MAX,
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

    // Canvas-authored geometry: no Add button, just a hint — selecting the
    // type above already set `mask.tool`, so the existing canvas affordances
    // (drag-to-create for Linear/Radial, stroke capture for Brush) create it.
    if matches!(
        mask.tool,
        MaskTool::Brush | MaskTool::Linear | MaskTool::Radial
    ) {
        ui.label(
            egui::RichText::new("Draw on the image to add this component")
                .size(11.0)
                .color(theme::TEXT_FAINT),
        );
    }

    // Luma-range can be added directly with the current slider values (it
    // needs no canvas gesture).
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
            out = Some(commit_edit(mask_edit::add_component(
                stack,
                idx,
                c,
                mask.next_mode,
            )));
            mask.preview_component = None;
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
        let pick_label = if mask.picking_color {
            format!("{} Picking… (click image)", crate::icons::EYEDROPPER)
        } else {
            format!("{} Pick color", crate::icons::EYEDROPPER)
        };
        if ui
            .selectable_label(mask.picking_color, pick_label)
            .clicked()
        {
            mask.picking_color = !mask.picking_color;
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
            out = Some(commit_edit(mask_edit::add_component(
                stack,
                idx,
                c,
                mask.next_mode,
            )));
            mask.color_samples.clear();
            mask.picking_color = false;
            mask.preview_component = None;
        }
    }

    out
}

/// Wrap a resulting `OpStack` as a committed mask edit (mask edits share
/// `OpKind::LocalAdjustments`, one history entry per gesture — mirrors
/// `mask_panel::show`'s local `commit` closure).
fn commit_edit(stack: OpStack) -> EditOutcome {
    EditOutcome {
        stack,
        kind: OpKind::LocalAdjustments,
        commit: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luma_component_round_trips_through_state() {
        let mut st = MaskUiState::default();
        let c = MaskComponent::LumaRange {
            lo: 0.2,
            hi: 0.7,
            softness: 0.15,
        };
        load_component_into_state(&c, &mut st);
        let rebuilt = luma_from_state(&st);
        assert_eq!(rebuilt, c);
    }

    #[test]
    fn color_component_round_trips_through_state() {
        let mut st = MaskUiState::default();
        let c = MaskComponent::ColorRange {
            samples: vec![ferrolite_mask::Rgb::new(0.1, 0.2, 0.3)],
            tolerance: 0.33,
            softness: 0.22,
        };
        load_component_into_state(&c, &mut st);
        let rebuilt = color_from_state(&st);
        assert_eq!(rebuilt, c);
    }

    #[test]
    fn component_label_covers_all_variants() {
        assert_eq!(
            component_label(&MaskComponent::Brush { strokes: vec![] }),
            "Brush"
        );
        assert_eq!(
            component_label(&MaskComponent::LinearGradient {
                start: ferrolite_mask::Vec2::new(0.0, 0.0),
                end: ferrolite_mask::Vec2::new(1.0, 1.0),
            }),
            "Linear gradient"
        );
        assert_eq!(
            component_label(&MaskComponent::RadialGradient {
                center: ferrolite_mask::Vec2::new(0.5, 0.5),
                radius: ferrolite_mask::Vec2::new(0.3, 0.3),
                rotation: 0.0,
                feather: 0.1,
                invert: false,
            }),
            "Radial gradient"
        );
    }

    #[test]
    fn only_luma_and_color_are_editable() {
        assert!(is_editable(&MaskComponent::LumaRange {
            lo: 0.0,
            hi: 1.0,
            softness: 0.0
        }));
        assert!(is_editable(&MaskComponent::ColorRange {
            samples: vec![],
            tolerance: 0.0,
            softness: 0.0
        }));
        assert!(!is_editable(&MaskComponent::Brush { strokes: vec![] }));
        assert!(!is_editable(&MaskComponent::LinearGradient {
            start: ferrolite_mask::Vec2::new(0.0, 0.0),
            end: ferrolite_mask::Vec2::new(1.0, 1.0),
        }));
    }
}
