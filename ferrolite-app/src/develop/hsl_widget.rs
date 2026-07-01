//! HSL widget: 8-band swatch row + per-band Hue/Sat/Lum sliders. The canonical
//! band order is red, orange, yellow, green, aqua, blue, purple, magenta.

use crate::develop::adjustment_panel::EditOutcome;
use crate::widgets::slider::EguiSlider;
use ferrolite_pipeline::{Hsl, HslBand, Op, OpKind, OpStack};

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

pub fn show(ui: &mut egui::Ui, stack: &OpStack, band: &mut usize) -> Option<EditOutcome> {
    let mut hsl = stack.hsl().unwrap_or(Hsl {
        bands: [HslBand {
            hue: 0.0,
            sat: 0.0,
            lum: 0.0,
        }; 8],
    });
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
                    egui::Stroke::new(2.0, crate::theme::ACCENT_BRIGHT),
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
    });
    if rh.changed() || rs.changed() || rl.changed() {
        hsl.bands[b] = HslBand { hue, sat, lum };
        let all_zero = hsl
            .bands
            .iter()
            .all(|x| x.hue == 0.0 && x.sat == 0.0 && x.lum == 0.0);
        let s = if all_zero {
            stack.reset(OpKind::Hsl)
        } else {
            stack.set_op(Op::Hsl(hsl))
        };
        let commit = rh.drag_stopped()
            || rs.drag_stopped()
            || rl.drag_stopped()
            || !(rh.dragged() || rs.dragged() || rl.dragged());
        out = Some(EditOutcome {
            stack: s,
            kind: OpKind::Hsl,
            commit,
        });
    }
    out
}
