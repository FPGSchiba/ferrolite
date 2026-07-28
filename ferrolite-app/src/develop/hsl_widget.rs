//! HSL widget: 8-band swatch row + per-band Hue/Sat/Lum sliders. The canonical
//! band order is red, orange, yellow, green, aqua, blue, purple, magenta.
//! Renders against a `ScopedEdit` (design 2026-07-28 §2, Phase 2b Task 3), so
//! the same widget drives both the global HSL and a selected mask's; `band` is
//! UI-only state (deliberately shared between scopes). `MaskNone` renders a
//! faint hint and returns `None`.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::scope::ScopedEdit;
use crate::theme;
use crate::widgets::slider::EguiSlider;
use ferrolite_pipeline::{HslBand, OpKind};

const SWATCHES: [(u8, u8, u8); 8] = [
    (0xc7, 0x54, 0x50),
    (0xd8, 0x8c, 0x3a),
    (0xd8, 0xc8, 0x3a),
    (0x4c, 0xaf, 0x71),
    (0x3a, 0xc8, 0xc8),
    (0x6d, 0x97, 0xb5),
    (0x9a, 0x6d, 0xb5),
    (0xb5, 0x6d, 0x9a),
];

pub fn show(ui: &mut egui::Ui, scoped: &ScopedEdit, band: &mut usize) -> Option<EditOutcome> {
    let Some(set) = scoped.set() else {
        ui.label(egui::RichText::new("Create or select a mask first").color(theme::TEXT_FAINT));
        return None;
    };
    let mut hsl = set.hsl;
    let mut out = None;

    ui.horizontal(|ui| {
        for (i, (r, g, b)) in SWATCHES.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
            ui.painter()
                .rect_filled(rect, 2.0, egui::Color32::from_rgb(*r, *g, *b));
            if i == *band {
                ui.painter().rect_stroke(
                    rect,
                    2.0,
                    egui::Stroke::new(2.0_f32, crate::theme::ACCENT_BRIGHT),
                );
            }
            if resp.clicked() {
                *band = i;
            }
        }
    });

    let b = (*band).min(7);
    let mut hue = hsl.bands[b].hue;
    let mut sat = hsl.bands[b].sat;
    let mut lum = hsl.bands[b].lum;
    let rh = ui.add(EguiSlider {
        label: "Hue",
        value: &mut hue,
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
    let rs = ui.add(EguiSlider {
        label: "Sat",
        value: &mut sat,
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
    let rl = ui.add(EguiSlider {
        label: "Lum",
        value: &mut lum,
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
    if rh.dragged() || rs.dragged() || rl.dragged() {
        // Mid-drag on a band slider: suppress the mask overlay the same way a
        // dragged `scoped_slider` does.
        scoped.adjusting.set(true);
    }

    if rh.changed() || rs.changed() || rl.changed() {
        hsl.bands[b] = HslBand { hue, sat, lum };
        let commit = rh.drag_stopped()
            || rs.drag_stopped()
            || rl.drag_stopped()
            || !(rh.dragged() || rs.dragged() || rl.dragged());
        let mut new_set = set.clone();
        new_set.hsl = hsl;
        out = scoped.write(new_set, OpKind::Hsl, commit);
    }
    out
}
