//! The scoped-adjustment registry core (design 2026-07-28 §2): a declarative
//! `SliderSpec` (label, range, get/set into an `AdjustmentSet`, per-scope
//! readiness) plus `scoped_slider`, the single render+edit path the three
//! base tabs (Tasks 3-6) drive their sliders through instead of hand-rolling
//! `EguiSlider` + `ops_edit` calls per control.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::scope::{EditScope, ScopedEdit};
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
        EditScope::MaskNone => (false, "Create or select a mask first"),
        EditScope::Mask(_) => (spec.mask_ready, spec.mask_reason),
        EditScope::Global => (spec.global_ready, spec.global_reason),
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
            ui.add(EguiSlider {
                label: spec.label,
                value: &mut value,
                min: spec.min,
                max: spec.max,
                default: spec.default,
                step: spec.step,
                decimals: spec.decimals,
                unit: spec.unit,
                bipolar: spec.bipolar,
                signed: spec.bipolar,
                custom_label_w: None,
            });
        })
        .response
        .on_hover_text(reason);
        return None;
    }

    // SAFETY of the unwrap: `ready` above is only true when `current` is `Some`.
    let set = current.expect("ready implies a set to read");
    let mut value = (spec.get)(set);
    let r = ui.add(EguiSlider {
        label: spec.label,
        value: &mut value,
        min: spec.min,
        max: spec.max,
        default: spec.default,
        step: spec.step,
        decimals: spec.decimals,
        unit: spec.unit,
        bipolar: spec.bipolar,
        signed: spec.bipolar,
        custom_label_w: None,
    });

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
}
