//! The Crop tool: a canvas overlay (crop handles + rule-of-thirds grid) plus a
//! dedicated panel (design 2026-07-29 §C3 / V2 README:69) — while Crop is active
//! the shared Light/Color/Effects tab row disappears entirely (`tool_panel.rs`'s
//! Crop branch), replaced by this panel's two sections: CROP & TRANSFORM (angle,
//! aspect combo + chips, reset) and GEOMETRY (keystone V/H, disabled Auto
//! Perspective/Guided Upright). Both wrap existing, already-tested code
//! (`crop_overlay::show`, the former adjustment-panel "Geometry" section) so
//! this migration is behavior-preserving for angle/aspect/reset.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::crop_math;
use crate::develop::tool::{DevelopCtx, DevelopTool, PanelTab, TabId, ToolId};
use crate::state::AppState;
use crate::widgets::section_header;
use crate::widgets::slider::EguiSlider;
use ferrolite_pipeline::{Aspect, CropRect, Geometry, Op, OpKind, OpStack};

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
        // Pre-extract the OpStack and the aspect dims from the viewer before
        // calling into the overlay, since a later `apply_edit` call needs an
        // exclusive borrow of `state`.
        let (stack, dims) = {
            let v = state.viewer.as_ref()?;
            (v.op_stack.clone(), v.image_dims.unwrap_or((1, 1)))
        };
        match crate::develop::crop_overlay::show(ui, image_rect, &stack, dims)? {
            crate::develop::crop_overlay::CropOverlayAction::Edit(outcome) => Some(*outcome),
            // A drag that started outside the crop rect pans the canvas (QoL:
            // pan keeps working in crop mode). Applied here — the overlay is
            // egui-only and has no access to the viewer's view transform.
            crate::develop::crop_overlay::CropOverlayAction::Pan(delta) => {
                if let Some(v) = state.viewer.as_mut() {
                    v.view = crate::viewer::apply_pan(v.view, (delta.x, delta.y));
                    v.idle = false;
                }
                ui.ctx().request_repaint();
                None
            }
        }
    }
}

/// Build the committing `EditOutcome` for a `Geometry` change: identity ->
/// `reset`, otherwise `set_op`. Single source of truth for every control in
/// this panel (angle, aspect combo, aspect chips, keystone V/H, "Reset crop")
/// so they all write through the exact same path. `pub(crate)` so
/// `crop_overlay`'s drag handler (mid-drag AND commit outcomes, plus the
/// Escape-cancel restore) shares it too — it used to call `stack.set_op`
/// directly, which left `Some(Geometry::default())` instead of a normalized
/// `None` when a drag landed back exactly at the identity crop (Task 4
/// review finding), desyncing `EditDoc::is_identity()`.
pub(crate) fn geometry_edit(stack: &OpStack, new_geo: Geometry, commit: bool) -> EditOutcome {
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

/// The crop rect a just-picked aspect implies: conform the existing rect to
/// the new ratio IMMEDIATELY (keep center, preserve area, clamp to bounds —
/// `crop_math::conform_to_aspect`), instead of only constraining future
/// drags. The ratio is converted to normalized space by the source dims
/// first (`crop_math::normalized_aspect`) — the same conversion the overlay's
/// drag path uses — so "3:2" means 3:2 in PIXELS. `Aspect::Free` (the
/// "Custom" state) constrains nothing and leaves the rect untouched;
/// `Aspect::Original` conforms back to the full-image ratio.
fn conformed_crop(crop: CropRect, aspect: Aspect, dims: (u32, u32)) -> CropRect {
    match crop_math::normalized_aspect(aspect, dims.0, dims.1) {
        Some(ar) => crop_math::conform_to_aspect(crop, ar),
        None => crop,
    }
}

/// The opposite-orientation counterpart of a ratio-backed aspect, for the
/// crop panel's orientation-flip toggle: landscape 4:3 \u{2194} portrait 3:4,
/// 3:2 \u{2194} 2:3, 16:9 \u{2194} 9:16, 5:4 \u{2194} 4:5 (either direction —
/// the mapping is its own inverse). `Aspect::Original`, `Aspect::Square`, and
/// `Aspect::Free` have no orientation-specific counterpart (a square or an
/// unconstrained/full-frame crop doesn't have a "portrait" version) and map
/// to `None` — the flip toggle disables itself on these (see
/// `aspect_chip_row`'s wiring of the toggle's `enabled` state).
fn flipped(aspect: Aspect) -> Option<Aspect> {
    match aspect {
        Aspect::FourThree => Some(Aspect::ThreeFour),
        Aspect::ThreeFour => Some(Aspect::FourThree),
        Aspect::ThreeTwo => Some(Aspect::TwoThree),
        Aspect::TwoThree => Some(Aspect::ThreeTwo),
        Aspect::SixteenNine => Some(Aspect::NineSixteen),
        Aspect::NineSixteen => Some(Aspect::SixteenNine),
        Aspect::FiveFour => Some(Aspect::FourFive),
        Aspect::FourFive => Some(Aspect::FiveFour),
        Aspect::Original | Aspect::Free | Aspect::Square => None,
    }
}

/// The aspect chip row's (label, backing-preset) pairs, in spec order:
/// Original / 1:1 / 4:3 / 3:2 / 16:9 / 5:4 / Custom (design 2026-07-29 §C3 /
/// V2 README:69). Every ratio-named chip has a real backing `Aspect` value and
/// is fully clickable. `None` marks "Custom": selected-state only per spec —
/// shown active when the current aspect matches none of the six ratio-backed
/// chips (i.e. `Aspect::Free`), but clicking it does nothing — `Free` is
/// reached via the Aspect combo above, or by dragging the crop handles to a
/// non-preset ratio, not by clicking this chip.
fn aspect_chip_specs() -> [(&'static str, Option<Aspect>); 7] {
    [
        ("Original", Some(Aspect::Original)),
        ("1:1", Some(Aspect::Square)),
        ("4:3", Some(Aspect::FourThree)),
        ("3:2", Some(Aspect::ThreeTwo)),
        ("16:9", Some(Aspect::SixteenNine)),
        ("5:4", Some(Aspect::FiveFour)),
        ("Custom", None),
    ]
}

/// Whether an aspect chip (`value`, from [`aspect_chip_specs`]) should render
/// selected for the given `current` aspect. A landscape-backed chip is active
/// either when `current` IS that preset, OR when `current` is its PORTRAIT
/// counterpart (`flipped(current) == value`) — e.g. with `Aspect::ThreeFour`
/// active, the "4:3" chip renders selected, since the chip row keeps only its
/// seven landscape labels; the orientation-flip toggle rendered at the row's
/// end is what actually distinguishes portrait from landscape (see
/// [`flipped`] and its wiring in [`aspect_chip_row`]). "Custom" (`value ==
/// None`) is active only for `Aspect::Free`, independent of orientation.
fn chip_is_active(value: Option<Aspect>, current: Aspect) -> bool {
    match value {
        Some(a) => current == a || flipped(current) == Some(a),
        None => current == Aspect::Free,
    }
}

/// Render the wrapping aspect chip row PLUS the orientation-flip toggle at
/// its end, and return `Some(new_aspect)` when the user either clicked a
/// preset-backed chip that differs from `current` or clicked an ENABLED flip
/// toggle (the caller writes either through `geometry_edit`, same as the
/// Aspect combo — see `CropTab::show`). Chips reuse `widgets::chips::chip_button`
/// styling (Task 8's shared chip primitive) so they stay visually identical
/// to every other chip row in the app; the toggle reuses `widgets::tool_button`
/// for the same active/disabled visual language as every other icon button.
///
/// State mapping (documented per the crop-portrait feature): the row shows
/// only the 7 landscape labels — a portrait aspect has no chip of its own.
/// When `current` is a portrait variant, its LANDSCAPE COUNTERPART chip
/// renders selected (`chip_is_active`) AND the flip toggle renders
/// active/accented (`flip_active` below); clicking that already-selected
/// counterpart chip is a no-op (same "clicking the active chip does nothing"
/// rule as every other chip), so switching back to landscape goes through the
/// flip toggle. The toggle itself is disabled (with a hover reason) when
/// `current` has no counterpart at all (`Aspect::Original`/`Square`/`Free`).
fn aspect_chip_row(ui: &mut egui::Ui, current: Aspect) -> Option<Aspect> {
    let mut picked: Option<Aspect> = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for (label, value) in aspect_chip_specs() {
            let is_active = chip_is_active(value, current);
            let resp = crate::widgets::chips::chip_button(ui, label, is_active);
            match value {
                Some(a) if resp.clicked() && !is_active => picked = Some(a),
                None => {
                    resp.on_hover_text(
                        "Shown when the crop uses a free ratio \u{2014} set any ratio by \
                         dragging the crop handles",
                    );
                }
                _ => {}
            }
        }

        let flip_target = flipped(current);
        let flip_enabled = flip_target.is_some();
        let flip_active = matches!(
            current,
            Aspect::ThreeFour | Aspect::TwoThree | Aspect::NineSixteen | Aspect::FourFive
        );
        let flip_resp = crate::widgets::tool_button(
            ui,
            crate::icons::CROP_FLIP_ORIENTATION,
            "Flip crop orientation",
            flip_active,
            flip_enabled,
            Some("No portrait/landscape counterpart for this aspect"),
        );
        if flip_resp.clicked() && flip_enabled {
            picked = flip_target;
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
        let (stack, dims) = {
            let v = state.viewer.as_ref()?;
            (v.op_stack.clone(), v.image_dims.unwrap_or((1, 1)))
        };
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
                        Aspect::FiveFour,
                        // Portrait presets (Task "crop-portrait"): no chip of
                        // their own (see `aspect_chip_row`'s state mapping),
                        // but directly pickable here.
                        Aspect::ThreeFour,
                        Aspect::TwoThree,
                        Aspect::NineSixteen,
                        Aspect::FourFive,
                    ] {
                        ui.selectable_value(&mut aspect, a, format!("{a:?}"));
                    }
                });
            if r.changed() || aspect != geo.aspect {
                // A combo-picked aspect conforms the existing rect to the new
                // ratio right away (a discrete, committing action — the same
                // behavior as the chip row below); an angle drag leaves the
                // rect alone.
                let crop = if aspect != geo.aspect {
                    conformed_crop(geo.crop, aspect, dims)
                } else {
                    geo.crop
                };
                let new_geo = Geometry {
                    angle_deg: angle,
                    aspect,
                    crop,
                    ..geo
                };
                out = Some(geometry_edit(
                    &stack,
                    new_geo,
                    r.drag_stopped() || !r.dragged() || aspect != geo.aspect,
                ));
            }

            ui.add_space(6.0);
            // `aspect` (this frame's combo value), not `geo.aspect` (the
            // pre-frame value): the combo may have just changed `aspect` above
            // in this same frame, and the chip row must reflect that
            // immediately rather than lagging it by a frame.
            if let Some(new_aspect) = aspect_chip_row(ui, aspect) {
                let new_geo = Geometry {
                    aspect: new_aspect,
                    crop: conformed_crop(geo.crop, new_aspect, dims),
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
        let mut state = AppState::for_test();
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
        let mut state = AppState::for_test();
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

    /// Step 1 (Task 6, updated in the review-fix round): the aspect chip row's
    /// spec-mandated shape — every ratio chip (including "5:4", now backed by
    /// `Aspect::FiveFour`) maps to a real preset and is clickable; "Custom" is
    /// the only chip that can never write aspect state (the `show()` wiring
    /// only ever handles the `Some(a)` arm for it), independent of any
    /// pixel-position synthetic click.
    #[test]
    fn aspect_chip_specs_match_the_spec_row_and_only_custom_is_unbacked() {
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
        assert_eq!(
            specs[5].1,
            Some(Aspect::FiveFour),
            "5:4 is a real preset now"
        );
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

    // ── Task "crop-portrait": portrait aspect variants + orientation flip ──

    /// `flipped` pairs every landscape ratio-preset with its portrait
    /// counterpart, in BOTH directions (the mapping is its own inverse), and
    /// returns `None` for the three orientation-agnostic presets.
    #[test]
    fn flipped_pairs_every_landscape_and_portrait_preset_both_directions() {
        let pairs = [
            (Aspect::FourThree, Aspect::ThreeFour),
            (Aspect::ThreeTwo, Aspect::TwoThree),
            (Aspect::SixteenNine, Aspect::NineSixteen),
            (Aspect::FiveFour, Aspect::FourFive),
        ];
        for (landscape, portrait) in pairs {
            assert_eq!(
                flipped(landscape),
                Some(portrait),
                "{landscape:?} must flip to {portrait:?}"
            );
            assert_eq!(
                flipped(portrait),
                Some(landscape),
                "{portrait:?} must flip back to {landscape:?}"
            );
        }
    }

    #[test]
    fn flipped_is_none_for_orientation_agnostic_presets() {
        for a in [Aspect::Original, Aspect::Square, Aspect::Free] {
            assert_eq!(flipped(a), None, "{a:?} has no orientation counterpart");
        }
    }

    /// The chip-selected-state mapping (design note in `aspect_chip_row`):
    /// every ratio chip is active for its own preset, AND for that preset's
    /// portrait counterpart — so a portrait aspect surfaces as its landscape
    /// chip being selected, since the row has no separate portrait labels.
    #[test]
    fn chip_is_active_matches_own_preset_and_its_portrait_counterpart() {
        // Direct match.
        assert!(chip_is_active(Some(Aspect::FourThree), Aspect::FourThree));
        // Portrait current -> landscape counterpart chip is active.
        assert!(chip_is_active(Some(Aspect::FourThree), Aspect::ThreeFour));
        assert!(chip_is_active(Some(Aspect::ThreeTwo), Aspect::TwoThree));
        assert!(chip_is_active(
            Some(Aspect::SixteenNine),
            Aspect::NineSixteen
        ));
        assert!(chip_is_active(Some(Aspect::FiveFour), Aspect::FourFive));
        // A DIFFERENT landscape chip must not light up for an unrelated portrait aspect.
        assert!(!chip_is_active(Some(Aspect::ThreeTwo), Aspect::ThreeFour));
    }

    #[test]
    fn chip_is_active_custom_chip_only_active_for_free() {
        assert!(chip_is_active(None, Aspect::Free));
        assert!(!chip_is_active(None, Aspect::ThreeFour));
        assert!(!chip_is_active(None, Aspect::FourThree));
        assert!(!chip_is_active(None, Aspect::Original));
    }

    /// End-to-end render smoke test: the row (7 chips + flip toggle) must
    /// render without panicking for every portrait aspect, and produce no
    /// pick on an input-free frame (mirrors `aspect_chip_row_click_...`
    /// tests above, which cover the click mechanics on the ratio chips).
    #[test]
    fn aspect_chip_row_renders_for_every_portrait_aspect_without_panicking() {
        let ctx = egui::Context::default();
        for a in [
            Aspect::ThreeFour,
            Aspect::TwoThree,
            Aspect::NineSixteen,
            Aspect::FourFive,
        ] {
            let mut picked = None;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    picked = aspect_chip_row(ui, a);
                });
            });
            assert_eq!(picked, None, "no click happened, no pick expected");
        }
    }

    /// UX gap B: picking an aspect conforms the EXISTING rect immediately —
    /// and does so in PIXEL space (the ratio the chip names), i.e. the
    /// source-dims conversion is applied, not skipped like the pre-fix
    /// overlay drag path.
    #[test]
    fn conformed_crop_three_two_on_a_6000x4000_source_is_3_2_in_pixels() {
        let c = CropRect {
            x: 0.1,
            y: 0.2,
            w: 0.3,
            h: 0.5,
        };
        let r = conformed_crop(c, Aspect::ThreeTwo, (6000, 4000));
        let pixel_ratio = (r.w * 6000.0) / (r.h * 4000.0);
        assert!(
            (pixel_ratio - 1.5).abs() < 1e-3,
            "pixel rect must be 3:2, got {pixel_ratio}"
        );
        // Center kept (feasible here) and bounds respected.
        assert!((r.x + r.w * 0.5 - (c.x + c.w * 0.5)).abs() < 1e-4);
        assert!((r.y + r.h * 0.5 - (c.y + c.h * 0.5)).abs() < 1e-4);
        assert!(r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= 1.0 + 1e-6 && r.y + r.h <= 1.0 + 1e-6);
    }

    #[test]
    fn conformed_crop_free_changes_nothing() {
        let c = CropRect {
            x: 0.1,
            y: 0.2,
            w: 0.3,
            h: 0.5,
        };
        let r = conformed_crop(c, Aspect::Free, (6000, 4000));
        assert_eq!((r.x, r.y, r.w, r.h), (c.x, c.y, c.w, c.h));
    }

    #[test]
    fn conformed_crop_original_restores_the_full_image_ratio() {
        // An uncropped frame stays uncropped...
        let full = conformed_crop(CropRect::full(), Aspect::Original, (6000, 4000));
        assert_eq!((full.x, full.y, full.w, full.h), (0.0, 0.0, 1.0, 1.0));
        // ...and a partial crop becomes the source's own ratio in pixels.
        let c = CropRect {
            x: 0.1,
            y: 0.2,
            w: 0.3,
            h: 0.6,
        };
        let r = conformed_crop(c, Aspect::Original, (6000, 4000));
        let pixel_ratio = (r.w * 6000.0) / (r.h * 4000.0);
        assert!((pixel_ratio - 1.5).abs() < 1e-3, "got {pixel_ratio}");
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
