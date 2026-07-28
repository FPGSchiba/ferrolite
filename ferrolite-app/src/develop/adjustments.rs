//! The scoped-adjustment registry core (design 2026-07-28 §2): a declarative
//! `SliderSpec` (label, range, get/set into an `AdjustmentSet`, per-scope
//! readiness) plus `scoped_slider`, the single render+edit path the three
//! base tabs (Tasks 3-6) drive their sliders through instead of hand-rolling
//! `EguiSlider` + `ops_edit` calls per control.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::scope::{EditScope, ScopedEdit, MASK_NONE_HINT};
use crate::widgets::slider::EguiSlider;

/// Stable identifier for a registered adjustment control (e.g.
/// `AdjustmentId("exposure")`). Distinguishes registry entries independent of
/// their display label, which may change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AdjustmentId(pub &'static str);

/// A declarative description of one scoped slider control: its `EguiSlider`
/// layout, how to read/write it from an `AdjustmentSet`, the `OpKind` its
/// edits carry, and its readiness (enabled/disabled + hover reason) in each
/// scope.
pub struct SliderSpec {
    pub id: AdjustmentId,
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub step: f32,
    pub decimals: usize,
    pub unit: &'static str,
    pub bipolar: bool,
    pub get: fn(&ferrolite_pipeline::AdjustmentSet) -> f32,
    pub set: fn(&mut ferrolite_pipeline::AdjustmentSet, f32),
    pub kind: ferrolite_pipeline::OpKind,
    pub global_ready: bool,
    pub mask_ready: bool,
    /// Hover reason shown when greyed in that scope (empty ⇒ ready).
    pub global_reason: &'static str,
    pub mask_reason: &'static str,
}

/// Resolve whether `spec`'s control renders enabled for `scope`, and the
/// hover reason to show when it doesn't (brief: `Global`/`Mask(_)` use their
/// own `*_ready`/`*_reason` pair; `MaskNone` is ALWAYS disabled with a fixed
/// reason that overrides the spec's). Pulled out of `scoped_slider` so the
/// readiness rule is unit-testable without an egui context.
fn readiness(scope: EditScope, spec: &SliderSpec) -> (bool, &'static str) {
    match scope {
        EditScope::MaskNone => (false, MASK_NONE_HINT),
        EditScope::Mask(_) => (spec.mask_ready, spec.mask_reason),
        EditScope::Global => (spec.global_ready, spec.global_reason),
    }
}

/// Build the `EguiSlider` widget for `spec`, bound to `value`, exactly as
/// both the disabled and enabled paths of `scoped_slider` need it. Pulled
/// into one place so the "must match base_tabs exactly" layout invariant
/// can't drift between the two paths.
fn slider_widget<'a>(spec: &SliderSpec, value: &'a mut f32) -> EguiSlider<'a> {
    EguiSlider {
        label: spec.label,
        value,
        min: spec.min,
        max: spec.max,
        default: spec.default,
        step: spec.step,
        decimals: spec.decimals,
        unit: spec.unit,
        bipolar: spec.bipolar,
        signed: spec.bipolar,
        custom_label_w: None,
    }
}

/// Render one scoped slider and, if it produced an edit this frame, the
/// resulting `EditOutcome`.
///
/// Ready + interactive → renders an enabled `EguiSlider` reading
/// `spec.get(scoped.set())`; on change, builds a full new `AdjustmentSet` via
/// `spec.set` and writes it back through `scoped.write` (kind = `spec.kind`,
/// commit = drag-stopped or not-a-drag). While dragged, marks
/// `scoped.adjusting` so callers can suppress mask overlays.
///
/// Not ready (including a stale `Mask` index, which reads as unready even if
/// `mask_ready` is true) → renders the same slider disabled via
/// `add_enabled_ui(false, ..)`, showing the current value (or `spec.default`
/// if there is none to read), with the reason as a hover tooltip. Always
/// returns `None` in this path.
pub fn scoped_slider(
    ui: &mut egui::Ui,
    spec: &SliderSpec,
    scoped: &ScopedEdit<'_>,
) -> Option<EditOutcome> {
    let (ready, reason) = readiness(scoped.scope, spec);
    let current = scoped.set();
    let ready = ready && current.is_some();

    if !ready {
        let mut value = current.map(|s| (spec.get)(s)).unwrap_or(spec.default);
        ui.add_enabled_ui(false, |ui| {
            ui.add(slider_widget(spec, &mut value));
        })
        .response
        .on_hover_text(reason);
        return None;
    }

    // Invariant: `ready` above is only true when `current` is `Some`.
    let set = current.expect("ready implies a set to read");
    let mut value = (spec.get)(set);
    let r = ui.add(slider_widget(spec, &mut value));

    if r.dragged() {
        scoped.adjusting.set(true);
    }

    if !r.changed() {
        return None;
    }

    let mut new = set.clone();
    (spec.set)(&mut new, value);
    scoped.write(new, spec.kind, r.drag_stopped() || !r.dragged())
}

/// The `AdjustmentSet` field specs consumed by `LightTab::show`'s BASIC
/// SLIDERS section, in display order (design 2026-07-28 §2 table; Task 6's
/// invariant tests iterate this). Highlights/Shadows/Whites/Blacks are
/// mask-only for now (`global_ready: false`) — they read/write
/// `AdjustmentSet.highlights/shadows/whites/blacks` directly, distinct from
/// the legacy `ToneCurve.parametric` region sliders (`curve_widget_parametric`)
/// which remain the only global path to those four until the unified layer
/// engine (Phase 3) folds them together.
static LIGHT_SLIDERS: [SliderSpec; 8] = [
    SliderSpec {
        id: AdjustmentId("exposure"),
        label: "Exposure",
        min: -5.0,
        max: 5.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: " EV",
        bipolar: true,
        get: |s| s.exposure,
        set: |s, v| s.exposure = v,
        kind: ferrolite_pipeline::OpKind::Exposure,
        global_ready: true,
        mask_ready: true,
        global_reason: "",
        mask_reason: "",
    },
    SliderSpec {
        id: AdjustmentId("contrast"),
        label: "Contrast",
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        get: |s| s.contrast,
        set: |s, v| s.contrast = v,
        kind: ferrolite_pipeline::OpKind::Contrast,
        global_ready: true,
        mask_ready: true,
        global_reason: "",
        mask_reason: "",
    },
    SliderSpec {
        id: AdjustmentId("highlights"),
        label: "Highlights",
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        get: |s| s.highlights,
        set: |s, v| s.highlights = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: false,
        mask_ready: true,
        global_reason: "Global Highlights arrive with the unified layer engine (Phase 3)",
        mask_reason: "",
    },
    SliderSpec {
        id: AdjustmentId("shadows"),
        label: "Shadows",
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        get: |s| s.shadows,
        set: |s, v| s.shadows = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: false,
        mask_ready: true,
        global_reason: "Global Shadows arrive with the unified layer engine (Phase 3)",
        mask_reason: "",
    },
    SliderSpec {
        id: AdjustmentId("whites"),
        label: "Whites",
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        get: |s| s.whites,
        set: |s, v| s.whites = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: false,
        mask_ready: true,
        global_reason: "Global Whites arrive with the unified layer engine (Phase 3)",
        mask_reason: "",
    },
    SliderSpec {
        id: AdjustmentId("blacks"),
        label: "Blacks",
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        get: |s| s.blacks,
        set: |s, v| s.blacks = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: false,
        mask_ready: true,
        global_reason: "Global Blacks arrive with the unified layer engine (Phase 3)",
        mask_reason: "",
    },
    SliderSpec {
        id: AdjustmentId("temp"),
        label: "Temp",
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        get: |s| s.temp,
        set: |s, v| s.temp = v,
        kind: ferrolite_pipeline::OpKind::WhiteBalance,
        global_ready: true,
        mask_ready: true,
        global_reason: "",
        mask_reason: "",
    },
    SliderSpec {
        id: AdjustmentId("tint"),
        label: "Tint",
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        get: |s| s.tint,
        set: |s, v| s.tint = v,
        kind: ferrolite_pipeline::OpKind::WhiteBalance,
        global_ready: true,
        mask_ready: true,
        global_reason: "",
        mask_reason: "",
    },
];

/// The Light tab's BASIC SLIDERS specs, in display order.
pub fn light_sliders() -> &'static [SliderSpec] {
    &LIGHT_SLIDERS
}

/// The Color tab's COLOR MIX specs, in display order (design 2026-07-28 §2
/// table; Task 6's invariant tests iterate this). None are global-live yet
/// (`global_ready: false` on all four) — Saturation/Hue/Color already have a
/// per-mask shader (`local_adjust.wgsl`/the swatch overlay) so `mask_ready:
/// true`; Vibrance has no shader in any scope yet (`mask_ready: false`) until
/// the unified layer engine (Phase 3) lands it everywhere.
static COLOR_SLIDERS: [SliderSpec; 4] = [
    SliderSpec {
        id: AdjustmentId("saturation"),
        label: "Saturation",
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        get: |s| s.saturation,
        set: |s, v| s.saturation = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: false,
        mask_ready: true,
        global_reason: "Global Saturation arrives with the unified layer engine (Phase 3)",
        mask_reason: "",
    },
    SliderSpec {
        id: AdjustmentId("hue"),
        label: "Hue",
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        get: |s| s.hue,
        set: |s, v| s.hue = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: false,
        mask_ready: true,
        global_reason: "Global Hue arrives with the unified layer engine (Phase 3)",
        mask_reason: "",
    },
    SliderSpec {
        id: AdjustmentId("vibrance"),
        label: "Vibrance",
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        get: |s| s.vibrance,
        set: |s, v| s.vibrance = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: false,
        mask_ready: false,
        global_reason: "Vibrance arrives with the unified layer engine (Phase 3)",
        mask_reason: "Vibrance arrives with the unified layer engine (Phase 3)",
    },
    SliderSpec {
        id: AdjustmentId("color_amount"),
        label: "Color",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: false,
        get: |s| s.color.amount,
        set: |s, v| s.color.amount = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: false,
        mask_ready: true,
        global_reason: "Global color overlay arrives with the unified layer engine (Phase 3)",
        mask_reason: "",
    },
];

/// The Color tab's COLOR MIX specs, in display order.
pub fn color_sliders() -> &'static [SliderSpec] {
    &COLOR_SLIDERS
}

/// The Effects tab's SHARPENING/NOISE REDUCTION/DEHAZE specs, in display order
/// (design 2026-07-28 §2 table; Task 6's invariant tests iterate this). NR is
/// `global_ready: false, mask_ready: false` on all four rows — noise
/// reduction has no GPU pass wired in any scope yet, so it renders honestly
/// greyed everywhere (replacing the pre-registry enabled-but-dead sliders).
/// Sharpen/Dehaze are `global_ready: true, mask_ready: false` — live today
/// only on the global `AdjustmentSet` (`ops_edit::set_sharpen`/`set_dehaze`'s
/// former data path, now reached through `scoped.write`); per-mask arrives
/// with the per-mask neighborhood passes (Phase 4). The Sharpen "Detail"
/// slider from the pre-registry hand-rolled block is dropped here: it mapped
/// to no field and no planned shader parameter (YAGNI — the registry makes
/// re-adding trivial once a shader defines it).
static EFFECTS_SLIDERS: [SliderSpec; 8] = [
    SliderSpec {
        id: AdjustmentId("sharpen_amount"),
        label: "Amount",
        min: 0.0,
        max: 2.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: false,
        get: |s| s.sharpen.amount,
        set: |s, v| s.sharpen.amount = v,
        kind: ferrolite_pipeline::OpKind::Sharpen,
        global_ready: true,
        mask_ready: false,
        global_reason: "",
        mask_reason: "Per-mask Sharpening arrives with the per-mask neighborhood passes (Phase 4)",
    },
    SliderSpec {
        id: AdjustmentId("sharpen_radius"),
        label: "Radius",
        min: 1.0,
        max: 8.0,
        default: 1.0,
        step: 1.0,
        decimals: 0,
        unit: " px",
        bipolar: false,
        get: |s| s.sharpen.radius as f32,
        set: |s, v| s.sharpen.radius = v.round() as u32,
        kind: ferrolite_pipeline::OpKind::Sharpen,
        global_ready: true,
        mask_ready: false,
        global_reason: "",
        mask_reason: "Per-mask Sharpening arrives with the per-mask neighborhood passes (Phase 4)",
    },
    SliderSpec {
        id: AdjustmentId("nr_luminance"),
        label: "Luminance",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: false,
        get: |s| s.noise_reduction.luminance,
        set: |s, v| s.noise_reduction.luminance = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: false,
        mask_ready: false,
        global_reason: "Noise reduction is not wired yet — coming with its GPU pass",
        mask_reason: "Noise reduction is not wired yet — coming with its GPU pass",
    },
    SliderSpec {
        id: AdjustmentId("nr_detail"),
        label: "Detail",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: false,
        get: |s| s.noise_reduction.detail,
        set: |s, v| s.noise_reduction.detail = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: false,
        mask_ready: false,
        global_reason: "Noise reduction is not wired yet — coming with its GPU pass",
        mask_reason: "Noise reduction is not wired yet — coming with its GPU pass",
    },
    SliderSpec {
        id: AdjustmentId("nr_color"),
        label: "Color",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: false,
        get: |s| s.noise_reduction.color,
        set: |s, v| s.noise_reduction.color = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: false,
        mask_ready: false,
        global_reason: "Noise reduction is not wired yet — coming with its GPU pass",
        mask_reason: "Noise reduction is not wired yet — coming with its GPU pass",
    },
    SliderSpec {
        id: AdjustmentId("nr_color_detail"),
        label: "Color Detail",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: false,
        get: |s| s.noise_reduction.color_detail,
        set: |s, v| s.noise_reduction.color_detail = v,
        kind: ferrolite_pipeline::OpKind::LocalAdjustments,
        global_ready: false,
        mask_ready: false,
        global_reason: "Noise reduction is not wired yet — coming with its GPU pass",
        mask_reason: "Noise reduction is not wired yet — coming with its GPU pass",
    },
    SliderSpec {
        id: AdjustmentId("dehaze_amount"),
        label: "Dehaze",
        min: -1.0,
        max: 1.0,
        default: 0.0,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: true,
        get: |s| s.dehaze.amount,
        set: |s, v| s.dehaze.amount = v,
        kind: ferrolite_pipeline::OpKind::Dehaze,
        global_ready: true,
        mask_ready: false,
        global_reason: "",
        mask_reason: "Per-mask Dehaze arrives with the per-mask neighborhood passes (Phase 4)",
    },
    SliderSpec {
        id: AdjustmentId("dehaze_radius"),
        label: "Radius",
        min: 1.0,
        max: 24.0,
        default: ferrolite_pipeline::DEHAZE_DEFAULT_RADIUS as f32,
        step: 1.0,
        decimals: 0,
        unit: " px",
        bipolar: false,
        get: |s| s.dehaze.radius as f32,
        set: |s, v| s.dehaze.radius = v.round() as u32,
        kind: ferrolite_pipeline::OpKind::Dehaze,
        global_ready: true,
        mask_ready: false,
        global_reason: "",
        mask_reason: "Per-mask Dehaze arrives with the per-mask neighborhood passes (Phase 4)",
    },
];

/// The Effects tab's SHARPENING/NOISE REDUCTION/DEHAZE specs, in display order.
pub fn effects_sliders() -> &'static [SliderSpec] {
    &EFFECTS_SLIDERS
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{AdjustmentSet, OpKind, OpStack};

    fn exposure_spec() -> SliderSpec {
        SliderSpec {
            id: AdjustmentId("exposure"),
            label: "Exposure",
            min: -5.0,
            max: 5.0,
            default: 0.0,
            step: 0.01,
            decimals: 2,
            unit: " EV",
            bipolar: true,
            get: |s| s.exposure,
            set: |s, v| s.exposure = v,
            kind: OpKind::Exposure,
            global_ready: true,
            mask_ready: false,
            global_reason: "",
            mask_reason: "Not available on masks yet",
        }
    }

    #[test]
    fn adjustment_id_equality_is_by_name() {
        assert_eq!(AdjustmentId("exposure"), AdjustmentId("exposure"));
        assert_ne!(AdjustmentId("exposure"), AdjustmentId("contrast"));
    }

    #[test]
    fn readiness_uses_scope_specific_ready_and_reason() {
        let spec = exposure_spec();
        assert_eq!(readiness(EditScope::Global, &spec), (true, ""));
        assert_eq!(
            readiness(EditScope::Mask(0), &spec),
            (false, "Not available on masks yet")
        );
    }

    #[test]
    fn mask_none_is_always_disabled_regardless_of_spec_readiness() {
        // Even a spec that marks both scopes ready must be disabled for MaskNone.
        let mut spec = exposure_spec();
        spec.mask_ready = true;
        assert_eq!(
            readiness(EditScope::MaskNone, &spec),
            (false, "Create or select a mask first")
        );
    }

    #[test]
    fn scoped_slider_enabled_path_renders_without_edit_when_untouched() {
        let spec = exposure_spec();
        let doc = OpStack::default();
        let scoped = ScopedEdit::new(EditScope::Global, &doc);

        let ctx = egui::Context::default();
        let mut out = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                out = scoped_slider(ui, &spec, &scoped);
            });
        });
        assert!(out.is_none());
        assert!(!scoped.adjusting.get());
    }

    #[test]
    fn scoped_slider_disabled_path_never_edits() {
        let spec = exposure_spec();
        let doc = OpStack::default();
        let scoped = ScopedEdit::new(EditScope::Mask(0), &doc); // mask_ready: false ⇒ disabled

        let ctx = egui::Context::default();
        let mut out = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                out = scoped_slider(ui, &spec, &scoped);
            });
        });
        assert!(out.is_none());
    }

    #[test]
    fn scoped_slider_mask_none_never_edits_even_with_permissive_spec() {
        let mut spec = exposure_spec();
        spec.mask_ready = true;
        let doc = OpStack::default();
        let scoped = ScopedEdit::new(EditScope::MaskNone, &doc);

        let ctx = egui::Context::default();
        let mut out = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                out = scoped_slider(ui, &spec, &scoped);
            });
        });
        assert!(out.is_none());
    }

    #[test]
    fn slider_spec_get_set_round_trip_the_adjustment_set() {
        let spec = exposure_spec();
        let mut set = AdjustmentSet::default();
        (spec.set)(&mut set, 2.5);
        assert_eq!((spec.get)(&set), 2.5);
    }

    #[test]
    fn color_registry_rows_and_gating() {
        let specs = crate::develop::adjustments::color_sliders();
        let ids: Vec<&str> = specs.iter().map(|s| s.id.0).collect();
        assert_eq!(ids, vec!["saturation", "hue", "vibrance", "color_amount"]);
        assert!(
            specs.iter().all(|s| !s.global_ready),
            "none global-live until Phase 3"
        );
        let vib = specs.iter().find(|s| s.id.0 == "vibrance").unwrap();
        assert!(!vib.mask_ready, "vibrance has no shader in any scope yet");
        assert!(specs
            .iter()
            .filter(|s| s.id.0 != "vibrance")
            .all(|s| s.mask_ready));
    }

    #[test]
    fn effects_registry_rows_and_gating() {
        let specs = crate::develop::adjustments::effects_sliders();
        let ids: Vec<&str> = specs.iter().map(|s| s.id.0).collect();
        assert_eq!(
            ids,
            vec![
                "sharpen_amount",
                "sharpen_radius",
                "nr_luminance",
                "nr_detail",
                "nr_color",
                "nr_color_detail",
                "dehaze_amount",
                "dehaze_radius"
            ]
        );
        assert!(specs
            .iter()
            .filter(|s| s.id.0.starts_with("nr_"))
            .all(|s| !s.global_ready && !s.mask_ready));
        assert!(specs
            .iter()
            .filter(|s| s.id.0.starts_with("sharpen") || s.id.0.starts_with("dehaze"))
            .all(|s| s.global_ready && !s.mask_ready));
    }

    /// Registry invariant (design 2026-07-28 §6.2): every `AdjustmentId` across
    /// the three tabs is unique. `scoped_slider`/callers key controls by id, so
    /// a duplicate would make two rows indistinguishable to any id-based lookup.
    #[test]
    fn all_registry_ids_are_unique_across_the_three_tabs() {
        let ids: Vec<&str> = light_sliders()
            .iter()
            .chain(color_sliders())
            .chain(effects_sliders())
            .map(|s| s.id.0)
            .collect();
        let unique: std::collections::BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "duplicate AdjustmentId(s) found across the registry: {ids:?}"
        );
    }

    /// Registry invariant (design 2026-07-28 §6.2): a spec that is not ready in
    /// a scope must carry a non-empty reason for that scope, since `readiness`
    /// surfaces `*_reason` verbatim as the disabled control's hover tooltip —
    /// an empty reason would silently show a blank tooltip.
    #[test]
    fn every_not_ready_spec_carries_a_reason() {
        for spec in light_sliders()
            .iter()
            .chain(color_sliders())
            .chain(effects_sliders())
        {
            if !spec.global_ready {
                assert!(
                    !spec.global_reason.is_empty(),
                    "{}: global_ready=false must carry a non-empty global_reason",
                    spec.id.0
                );
            }
            if !spec.mask_ready {
                assert!(
                    !spec.mask_reason.is_empty(),
                    "{}: mask_ready=false must carry a non-empty mask_reason",
                    spec.id.0
                );
            }
        }
    }

    /// Registry invariant (design 2026-07-28 §6.2): every spec's `default` is
    /// the identity value for the field(s) it writes — applying it to a fresh
    /// `AdjustmentSet::default()` and normalizing must round-trip back to
    /// `AdjustmentSet::default()`. This is what makes the per-control reset
    /// affordance (which writes `spec.default`) an actual reset to identity.
    #[test]
    fn resetting_every_spec_to_its_default_yields_the_identity_set() {
        for spec in light_sliders()
            .iter()
            .chain(color_sliders())
            .chain(effects_sliders())
        {
            let mut set = AdjustmentSet::default();
            (spec.set)(&mut set, spec.default);
            let normalized = set.normalized();
            assert_eq!(
                normalized,
                AdjustmentSet::default(),
                "{}: applying its own default should normalize back to the identity set",
                spec.id.0
            );
        }
    }
}
