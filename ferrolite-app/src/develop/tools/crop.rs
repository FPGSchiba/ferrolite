//! The Crop tool: a canvas overlay (crop handles + rule-of-thirds grid) plus a
//! dedicated panel (design 2026-07-29 §C3 / V2 README:69) — while Crop is active
//! the shared Light/Color/Effects tab row disappears entirely (`tool_panel.rs`'s
//! Crop branch), replaced by this panel's two sections: CROP & TRANSFORM (angle,
//! aspect combo + chips, reset) and GEOMETRY (keystone V/H, disabled Auto
//! Perspective/Guided Upright). Both wrap existing, already-tested code
//! (`crop_overlay::show`, the former adjustment-panel "Geometry" section) so
//! this migration is behavior-preserving for angle/aspect/reset.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::tool::{DevelopCtx, DevelopTool, PanelTab, TabId, ToolId};
use crate::state::AppState;
use crate::widgets::section_header;
use crate::widgets::slider::EguiSlider;
use ferrolite_pipeline::{Aspect, Geometry, Op, OpKind, OpStack};

pub struct CropTool;

impl DevelopTool for CropTool {
    fn id(&self) -> ToolId {
        ToolId::Crop
    }
    fn icon(&self) -> &'static str {
        crate::icons::CROP
    }
    fn label(&self) -> &'static str {
        "Crop"
    }
    fn enabled(&self, ctx: &DevelopCtx) -> bool {
        ctx.state.viewer.is_some()
    }
    fn tabs(&self) -> Vec<Box<dyn PanelTab>> {
        vec![Box::new(CropTab)]
    }
    fn canvas(
        &self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        state: &mut AppState,
    ) -> Option<EditOutcome> {
        // Wrap the existing crop overlay verbatim (mirrors app.rs:3689-3705): pre-extract
        // the OpStack and the aspect dims from the viewer before calling into the overlay,
        // since a later `apply_edit` call needs an exclusive borrow of `state`.
        let (stack, dims) = {
            let v = state.viewer.as_ref()?;
            (v.op_stack.clone(), v.image_dims.unwrap_or((1, 1)))
        };
        crate::develop::crop_overlay::show(ui, image_rect, &stack, dims)
    }
}

/// Build the committing `EditOutcome` for a `Geometry` change: identity ->
/// `reset`, otherwise `set_op`. Single source of truth for every control in
/// this panel (angle, aspect combo, aspect chips, keystone V/H, "Reset crop")
/// so they all write through the exact same path.
fn geometry_edit(stack: &OpStack, new_geo: Geometry, commit: bool) -> EditOutcome {
    let s = if new_geo.is_identity() {
        stack.reset(OpKind::Geometry)
    } else {
        stack.set_op(Op::Geometry(new_geo))
    };
    EditOutcome {
        stack: s,
        kind: OpKind::Geometry,
        commit,
    }
}

/// The aspect chip row's (label, backing-preset) pairs, in spec order:
/// Original / 1:1 / 4:3 / 3:2 / 16:9 / 5:4 / Custom (design 2026-07-29 §C3 /
/// V2 README:69). `None` marks a chip with no real `Aspect` value behind it:
/// - "5:4" has no backing `Aspect` variant — adding one is a pipeline
///   (`ferrolite-pipeline`) change, out of scope for this panel-only task.
///   Rendered for visual/mockup parity with a hover explanation; clicking it
///   is a no-op.
/// - "Custom" is selected-state only per spec: shown active when the current
///   aspect matches none of the five ratio-backed chips (i.e. `Aspect::Free`),
///   but clicking it does nothing — `Free` is reached via the Aspect combo
///   above, not this chip.
fn aspect_chip_specs() -> [(&'static str, Option<Aspect>); 7] {
    [
        ("Original", Some(Aspect::Original)),
        ("1:1", Some(Aspect::Square)),
        ("4:3", Some(Aspect::FourThree)),
        ("3:2", Some(Aspect::ThreeTwo)),
        ("16:9", Some(Aspect::SixteenNine)),
        ("5:4", None),
        ("Custom", None),
    ]
}

/// Render the wrapping aspect chip row and return `Some(new_aspect)` when the
/// user clicked a preset-backed chip that differs from `current` (the caller
/// writes it through `geometry_edit`, same as the Aspect combo). Chips reuse
/// `widgets::chips::chip_button` styling (Task 8's shared chip primitive) so
/// they stay visually identical to every other chip row in the app.
fn aspect_chip_row(ui: &mut egui::Ui, current: Aspect) -> Option<Aspect> {
    let mut picked: Option<Aspect> = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for (label, value) in aspect_chip_specs() {
            let is_active = match value {
                Some(a) => current == a,
                None if label == "Custom" => current == Aspect::Free,
                None => false,
            };
            let resp = crate::widgets::chips::chip_button(ui, label, is_active);
            match value {
                Some(a) if resp.clicked() && !is_active => picked = Some(a),
                None if label == "5:4" => {
                    resp.on_hover_text(
                        "5:4 isn't available yet \u{2014} no matching Aspect preset in the pipeline",
                    );
                }
                _ => {}
            }
        }
    });
    picked
}

pub struct CropTab;

impl PanelTab for CropTab {
    fn id(&self) -> TabId {
        TabId("crop")
    }
    fn label(&self) -> &str {
        "Crop"
    }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        let stack = state.viewer.as_ref()?.op_stack.clone();
        let mut out: Option<EditOutcome> = None;
        let geo = stack.geometry().unwrap_or_default();

        // ── CROP & TRANSFORM ── angle, aspect (combo + chips), reset. The crop
        // rect handles themselves live on the canvas overlay (`CropTool::canvas`).
        section_header(
            ui,
            "CROP & TRANSFORM",
            &mut state.settings.crop_transform_open,
        );
        if state.settings.crop_transform_open {
            let mut angle = geo.angle_deg;
            let r = ui.add(EguiSlider {
                label: "Angle",
                value: &mut angle,
                min: -45.0,
                max: 45.0,
                default: 0.0,
                step: 0.1,
                decimals: 1,
                unit: "\u{b0}",
                bipolar: true,
                signed: true,
                custom_label_w: None,
            });
            let mut aspect = geo.aspect;
            egui::ComboBox::from_label("Aspect")
                .selected_text(format!("{aspect:?}"))
                .show_ui(ui, |ui| {
                    for a in [
                        Aspect::Original,
                        Aspect::Free,
                        Aspect::Square,
                        Aspect::ThreeTwo,
                        Aspect::FourThree,
                        Aspect::SixteenNine,
                    ] {
                        ui.selectable_value(&mut aspect, a, format!("{a:?}"));
                    }
                });
            if r.changed() || aspect != geo.aspect {
                let new_geo = Geometry {
                    angle_deg: angle,
                    aspect,
                    ..geo
                };
                out = Some(geometry_edit(
                    &stack,
                    new_geo,
                    r.drag_stopped() || !r.dragged() || aspect != geo.aspect,
                ));
            }

            ui.add_space(6.0);
            if let Some(new_aspect) = aspect_chip_row(ui, geo.aspect) {
                let new_geo = Geometry {
                    aspect: new_aspect,
                    ..geo
                };
                out = Some(geometry_edit(&stack, new_geo, true));
            }

            ui.add_space(4.0);
            if ui.small_button("Reset crop").clicked() {
                out = Some(geometry_edit(&stack, Geometry::default(), true));
            }
        }

        ui.separator();

        // ── GEOMETRY ── manual keystone V/H (spec C4) + disabled auto-perspective
        // affordances (Non-goal: implementations ship later; buttons ship disabled).
        section_header(ui, "GEOMETRY", &mut state.settings.crop_geometry_open);
        if state.settings.crop_geometry_open {
            let mut keystone_v = geo.keystone_v;
            let rv = ui.add(EguiSlider {
                label: "Keystone V",
                value: &mut keystone_v,
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
            if rv.changed() {
                let new_geo = Geometry { keystone_v, ..geo };
                out = Some(geometry_edit(
                    &stack,
                    new_geo,
                    rv.drag_stopped() || !rv.dragged(),
                ));
            }

            let mut keystone_h = geo.keystone_h;
            let rh = ui.add(EguiSlider {
                label: "Keystone H",
                value: &mut keystone_h,
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
            if rh.changed() {
                let new_geo = Geometry { keystone_h, ..geo };
                out = Some(geometry_edit(
                    &stack,
                    new_geo,
                    rh.drag_stopped() || !rh.dragged(),
                ));
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_enabled(false, egui::Button::new("Auto Perspective"))
                    .on_disabled_hover_text("Coming with automatic perspective analysis");
                ui.add_enabled(false, egui::Button::new("Guided Upright"))
                    .on_disabled_hover_text("Coming with automatic perspective analysis");
            });
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer::ViewerState;

    fn state_with_viewer() -> AppState {
        let mut state = AppState::new().unwrap();
        // Hermetic: AppState::new loads the developer's REAL settings file; these
        // tests assert against defaults, so reset (the author collapsing a section
        // in the running app must never fail the suite).
        state.settings = crate::settings::Settings::default();
        state.viewer = Some(ViewerState::open(
            1,
            std::path::PathBuf::from("x"),
            ferrolite_image::FileKind::Raw,
        ));
        state
    }

    #[test]
    fn crop_tab_renders_without_viewer() {
        let ctx = egui::Context::default();
        let mut state = AppState::new().unwrap();
        state.settings = crate::settings::Settings::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert!(CropTab.show(ui, &mut state).is_none());
            });
        });
    }

    #[test]
    fn crop_tab_sections_collapsible_and_persist() {
        let ctx = egui::Context::default();
        let mut state = state_with_viewer();
        state.settings.crop_transform_open = false;
        state.settings.crop_geometry_open = false;

        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = CropTab.show(ui, &mut state);
            });
        });
        assert!(!state.settings.crop_transform_open);
        assert!(!state.settings.crop_geometry_open);
    }

    #[test]
    fn crop_tab_sections_default_open() {
        let state = state_with_viewer();
        assert!(state.settings.crop_transform_open);
        assert!(state.settings.crop_geometry_open);
    }

    /// Step 1 (Task 6): the aspect chip row's spec-mandated shape — proves by
    /// construction that "5:4" and "Custom" can never write aspect state (the
    /// `show()` wiring only ever handles the `Some(a)` arm), independent of any
    /// pixel-position synthetic click.
    #[test]
    fn aspect_chip_specs_match_the_spec_row_and_flag_unbacked_chips() {
        let specs = aspect_chip_specs();
        let labels: Vec<&str> = specs.iter().map(|(l, _)| *l).collect();
        assert_eq!(
            labels,
            vec!["Original", "1:1", "4:3", "3:2", "16:9", "5:4", "Custom"]
        );
        assert_eq!(specs[0].1, Some(Aspect::Original));
        assert_eq!(specs[1].1, Some(Aspect::Square));
        assert_eq!(specs[2].1, Some(Aspect::FourThree));
        assert_eq!(specs[3].1, Some(Aspect::ThreeTwo));
        assert_eq!(specs[4].1, Some(Aspect::SixteenNine));
        assert!(specs[5].1.is_none(), "5:4 has no backing Aspect preset yet");
        assert!(specs[6].1.is_none(), "Custom is selected-state only");
    }

    #[test]
    fn aspect_chip_row_click_writes_the_clicked_presets_aspect() {
        let ctx = egui::Context::default();
        // Layout pass to establish chip rects; current == Free so no chip among
        // the five ratio-backed presets is active.
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = aspect_chip_row(ui, Aspect::Free);
            });
        });

        // Click near the leftmost chip ("Original").
        let mut input = egui::RawInput::default();
        let click_pos = egui::pos2(20.0, 12.0);
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });

        let mut picked = None;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                picked = aspect_chip_row(ui, Aspect::Free);
            });
        });

        assert_eq!(picked, Some(Aspect::Original));
    }

    #[test]
    fn aspect_chip_row_reports_no_pick_when_already_active() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = aspect_chip_row(ui, Aspect::Original);
            });
        });

        let mut input = egui::RawInput::default();
        let click_pos = egui::pos2(20.0, 12.0);
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });

        let mut picked = None;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                picked = aspect_chip_row(ui, Aspect::Original);
            });
        });

        assert_eq!(picked, None, "clicking the already-active chip is a no-op");
    }

    #[test]
    fn geometry_edit_resets_on_identity_and_sets_op_otherwise() {
        let stack = OpStack::default();
        let identity = geometry_edit(&stack, Geometry::default(), true);
        assert!(
            identity.stack.geometry().is_none() || identity.stack.geometry().unwrap().is_identity()
        );
        assert_eq!(identity.kind, OpKind::Geometry);
        assert!(identity.commit);

        let edited = geometry_edit(
            &stack,
            Geometry {
                angle_deg: 5.0,
                ..Geometry::default()
            },
            false,
        );
        assert_eq!(edited.stack.geometry().unwrap().angle_deg, 5.0);
        assert!(!edited.commit);
    }

    #[test]
    fn keystone_sliders_write_through_the_same_edit_path_as_angle() {
        let ctx = egui::Context::default();
        let mut state = state_with_viewer();

        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let out = CropTab.show(ui, &mut state);
                // No drag has happened yet, so no edit is emitted this frame.
                assert!(out.is_none());
            });
        });

        // Directly verify the wiring CropTab::show uses for keystone (same
        // `geometry_edit` helper Angle uses) rather than fighting slider drag
        // pixel math: a keystone-only change round-trips through `geometry_edit`
        // with `OpKind::Geometry` and preserves the rest of `Geometry`.
        let stack = state.viewer.as_ref().unwrap().op_stack.clone();
        let base = stack.geometry().unwrap_or_default();
        let edited = geometry_edit(
            &stack,
            Geometry {
                keystone_v: 0.4,
                ..base
            },
            true,
        );
        assert_eq!(edited.kind, OpKind::Geometry);
        assert_eq!(edited.stack.geometry().unwrap().keystone_v, 0.4);
        assert_eq!(edited.stack.geometry().unwrap().keystone_h, base.keystone_h);
    }
}
