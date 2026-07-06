//! Modal for managing a selected mask's components: list + delete (any) + edit
//! (Luma/Color via set_component). Keeps the 296px panel uncluttered (design §9.2).
//! Editing happens IN the modal (its own sliders), not by routing back to the
//! panel's Luma/Color sliders — the modal is self-contained.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::{mask_edit, mask_ui::MaskUiState};
use crate::widgets::EguiSlider;
use ferrolite_mask::MaskComponent;
use ferrolite_pipeline::{OpKind, OpStack};

/// Render the modal if `mask.components_modal_open`. Returns an edit if one was made.
pub fn show(ctx: &egui::Context, stack: &OpStack, mask: &mut MaskUiState) -> Option<EditOutcome> {
    if !mask.components_modal_open {
        return None;
    }
    let Some(mask_idx) = mask.selected else {
        mask.components_modal_open = false; // nothing selected -> close
        mask.editing_component = None;
        return None;
    };
    let layers = mask_edit::layers(stack);
    let Some(layer) = layers.layers.get(mask_idx) else {
        mask.components_modal_open = false;
        mask.editing_component = None;
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
        });
    if !open {
        mask.components_modal_open = false;
        mask.editing_component = None;
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
            ui.horizontal(|ui| {
                if ui.button("Update").clicked() {
                    result = Some(color_from_state(mask));
                }
                if ui.button("Cancel").clicked() {
                    mask.editing_component = None;
                }
            });
        }
        _ => {}
    }
    result
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
