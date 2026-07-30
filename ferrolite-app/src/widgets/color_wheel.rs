//! Reusable hue-sat color wheel for color grading. Hue = angle (0° at +x,
//! increasing counter-clockwise on screen), sat = radius (0 at center = neutral,
//! 1 at the rim). Drawn as a plain egui `Mesh` (grey center → full-sat rim), with
//! a draggable thumb and its own per-control reset (→ neutral sat 0). All memory
//! is salted with `id_source` so multiple wheels coexist (design §4.3).

use crate::theme;
use crate::widgets::draw_reset_arrow;
use crate::widgets::slider::EguiSlider;
use egui::{pos2, vec2, Color32, Mesh, Pos2, Sense, Shape, Stroke};

const RADIUS: f32 = 44.0;
const SEGMENTS: usize = 48;
const RESET_R: f32 = 4.5;

/// Returns the number of grid columns (1 or 2) for color grading wheels
/// based on available width (2 columns when width >= 280.0 px, 1 column when narrower).
pub fn color_grading_grid_columns(available_width: f32) -> usize {
    if available_width >= 280.0 {
        2
    } else {
        1
    }
}

/// A change emitted by `color_wheel`. `commit` false = live drag preview.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelEdit {
    pub hue: f32,
    pub sat: f32,
    pub commit: bool,
}

/// A change emitted by `color_grading_wheel`. `commit` false = live drag preview.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorGradingEdit {
    pub hue: f32,
    pub sat: f32,
    pub lum: f32,
    pub commit: bool,
}

/// Screen position of the thumb for a given (hue, sat). Screen y is down, so the
/// angle's sine is negated to make hue increase counter-clockwise visually.
fn wheel_pos(center: Pos2, radius: f32, hue: f32, sat: f32) -> Pos2 {
    let a = hue.to_radians();
    center + radius * sat.clamp(0.0_f32, 1.0_f32) * vec2(a.cos(), -a.sin())
}

/// (hue, sat) for a pointer position: sat = distance/radius clamped [0,1], hue =
/// screen angle (y-down inverted) in [0,360).
fn wheel_from_pos(center: Pos2, radius: f32, p: Pos2) -> (f32, f32) {
    let d = p - center;
    let sat = (d.length() / radius).clamp(0.0_f32, 1.0_f32);
    let mut hue = (-d.y).atan2(d.x).to_degrees();
    if hue < 0.0_f32 {
        hue += 360.0_f32;
    }
    (hue, sat)
}

/// HSV → egui Color32 (h in degrees, s/v in [0,1]) for the disc mesh (UI only).
fn hsv_color(h_deg: f32, s: f32, v: f32) -> Color32 {
    let h = h_deg.rem_euclid(360.0_f32) / 60.0_f32;
    let c = v * s;
    let x = c * (1.0_f32 - ((h % 2.0_f32) - 1.0_f32).abs());
    let m = v - c;
    let (r, g, b) = match h.floor() as i32 {
        0 => (c, x, 0.0_f32),
        1 => (x, c, 0.0_f32),
        2 => (0.0_f32, c, x),
        3 => (0.0_f32, x, c),
        4 => (x, 0.0_f32, c),
        _ => (c, 0.0_f32, x),
    };
    Color32::from_rgb(
        ((r + m) * 255.0_f32) as u8,
        ((g + m) * 255.0_f32) as u8,
        ((b + m) * 255.0_f32) as u8,
    )
}

/// Helper drawing the 88px circular color wheel disc.
fn draw_wheel_disc(
    ui: &mut egui::Ui,
    center: Pos2,
    hue: f32,
    sat: f32,
    resp: &egui::Response,
) -> Option<WheelEdit> {
    // Hue-sat disc as a triangle fan: grey center + full-sat rim.
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, Color32::from_gray(0x80));
    for i in 0..=SEGMENTS {
        let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let deg = a.to_degrees();
        mesh.colored_vertex(
            center + RADIUS * vec2(a.cos(), -a.sin()),
            hsv_color(deg, 1.0_f32, 1.0_f32),
        );
    }
    for i in 1..=SEGMENTS as u32 {
        mesh.add_triangle(0, i, i + 1);
    }
    let painter = ui.painter();
    painter.add(Shape::mesh(mesh));
    painter.circle_stroke(center, RADIUS, Stroke::new(1.0_f32, theme::BORDER_STRONG));

    // Thumb at the current (hue, sat).
    let thumb = wheel_pos(center, RADIUS, hue, sat);
    painter.circle(
        thumb,
        5.0_f32,
        Color32::WHITE,
        Stroke::new(1.5_f32, Color32::BLACK),
    );

    // Interaction: drag/click sets hue+sat; release commits.
    let mut result: Option<WheelEdit> = None;
    if let Some(p) = resp.interact_pointer_pos() {
        if resp.dragged() {
            let (h, s) = wheel_from_pos(center, RADIUS, p);
            result = Some(WheelEdit {
                hue: h,
                sat: s,
                commit: false,
            });
        } else if resp.clicked() {
            let (h, s) = wheel_from_pos(center, RADIUS, p);
            result = Some(WheelEdit {
                hue: h,
                sat: s,
                commit: true,
            });
        }
    }
    if resp.drag_stopped() {
        result = Some(WheelEdit {
            hue,
            sat,
            commit: true,
        });
    }

    result
}

pub fn color_wheel(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    hue: f32,
    sat: f32,
) -> Option<WheelEdit> {
    let size = RADIUS * 2.0_f32 + 4.0_f32;
    // Extra height below the disc for the reset affordance.
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size + 18.0_f32), Sense::click_and_drag());
    let base_id = ui.id().with(id_source);
    let center = pos2(rect.center().x, rect.top() + RADIUS + 2.0_f32);

    let mut result = draw_wheel_disc(ui, center, hue, sat, &resp);

    // Per-control reset (→ neutral sat 0), dim when already neutral.
    let reset_rect = egui::Rect::from_center_size(
        pos2(center.x, rect.bottom() - 8.0_f32),
        vec2(16.0_f32, 16.0_f32),
    );
    let reset_resp = ui.interact(reset_rect, base_id.with("wheel_reset"), Sense::click());
    let modified = sat > 0.0_f32;
    let reset_color = if modified {
        if reset_resp.hovered() {
            theme::ACCENT_BRIGHT
        } else {
            theme::TEXT_FAINT
        }
    } else {
        theme::BORDER_STRONG
    };
    draw_reset_arrow(ui.painter(), reset_rect.center(), RESET_R, reset_color);
    if reset_resp.clicked() && modified {
        result = Some(WheelEdit {
            hue,
            sat: 0.0_f32,
            commit: true,
        });
    }

    result
}

/// Combined color grading wheel widget (88px circular color wheel + aligned luminance
/// slider `-100.0..=100.0` beneath it + label).
#[allow(dead_code)]
pub fn color_grading_wheel(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    label: &str,
    hue: f32,
    sat: f32,
    lum: f32,
) -> Option<ColorGradingEdit> {
    let base_id = ui.id().with(id_source);
    let mut new_hue = hue;
    let mut new_sat = sat;
    let mut new_lum = lum;
    let mut changed = false;
    let mut commit = false;

    let is_modified = sat > 0.0_f32 || lum.abs() > f32::EPSILON;

    ui.vertical_centered(|ui| {
        if !label.is_empty() {
            ui.label(egui::RichText::new(label).color(theme::TEXT_FAINT));
            ui.add_space(2.0_f32);
        }

        let disc_size = RADIUS * 2.0_f32 + 4.0_f32;
        let (disc_rect, disc_resp) =
            ui.allocate_exact_size(vec2(disc_size, disc_size), Sense::click_and_drag());
        let center = disc_rect.center();

        let disc_edit = draw_wheel_disc(ui, center, hue, sat, &disc_resp);
        if let Some(e) = disc_edit {
            new_hue = e.hue;
            new_sat = e.sat;
            changed = true;
            commit |= e.commit;
        }

        ui.add_space(4.0_f32);

        let mut slider_lum = lum;
        let slider_resp = ui.add(EguiSlider {
            label: "Lum",
            value: &mut slider_lum,
            min: -100.0_f32,
            max: 100.0_f32,
            default: 0.0_f32,
            step: 1.0_f32,
            decimals: 0,
            unit: "",
            bipolar: true,
            signed: true,
            custom_label_w: None,
        });

        // Per-control reset arrow (on the right end of the Lum slider row).
        let reset_rect = egui::Rect::from_min_max(
            pos2(slider_resp.rect.right() - 16.0_f32, slider_resp.rect.top()),
            slider_resp.rect.right_bottom(),
        );
        let reset_resp = ui.interact(reset_rect, base_id.with("grading_reset"), Sense::click());

        let pointer_pos = ui.input(|i| i.pointer.interact_pos().or_else(|| i.pointer.latest_pos()));
        let is_reset_clicked = reset_resp.clicked()
            || ((slider_resp.clicked() || slider_resp.changed())
                && pointer_pos.is_some_and(|p| reset_rect.contains(p)));

        if is_modified && is_reset_clicked {
            new_hue = 0.0_f32;
            new_sat = 0.0_f32;
            new_lum = 0.0_f32;
            changed = true;
            commit = true;
        } else if slider_resp.changed() {
            new_lum = slider_lum;
            changed = true;
            commit |= slider_resp.drag_stopped() || !slider_resp.dragged();
        }

        // Over-draw reset arrow with combined modified state
        let reset_color = if is_modified {
            if reset_resp.hovered() || (is_modified && is_reset_clicked) {
                theme::ACCENT_BRIGHT
            } else {
                theme::TEXT_FAINT
            }
        } else {
            theme::BORDER_STRONG
        };
        draw_reset_arrow(ui.painter(), reset_rect.center(), RESET_R, reset_color);
    });

    if changed {
        Some(ColorGradingEdit {
            hue: new_hue,
            sat: new_sat,
            lum: new_lum,
            commit,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn center_maps_to_zero_sat() {
        let c = pos2(50.0_f32, 50.0_f32);
        let (_hue, sat) = wheel_from_pos(c, 40.0_f32, c);
        assert!(sat.abs() < 1e-6, "pointer at center = sat 0");
    }

    #[test]
    fn edge_maps_to_full_sat() {
        let c = pos2(50.0_f32, 50.0_f32);
        let edge = pos2(50.0_f32 + 40.0_f32, 50.0_f32); // one radius to the right
        let (_hue, sat) = wheel_from_pos(c, 40.0_f32, edge);
        assert!((sat - 1.0_f32).abs() < 1e-6, "pointer at the rim = sat 1");
    }

    #[test]
    fn beyond_edge_clamps_sat_to_one() {
        let c = pos2(50.0_f32, 50.0_f32);
        let far = pos2(50.0_f32 + 80.0_f32, 50.0_f32); // two radii out
        let (_h, sat) = wheel_from_pos(c, 40.0_f32, far);
        assert!((sat - 1.0_f32).abs() < 1e-6);
    }

    #[test]
    fn pos_and_from_pos_roundtrip() {
        let c = pos2(50.0_f32, 50.0_f32);
        let r = 40.0_f32;
        for &(hue, sat) in &[
            (0.0_f32, 0.5_f32),
            (90.0_f32, 1.0_f32),
            (210.0_f32, 0.3_f32),
            (330.0_f32, 0.8_f32),
        ] {
            let p = wheel_pos(c, r, hue, sat);
            let (h2, s2) = wheel_from_pos(c, r, p);
            assert!((s2 - sat).abs() < 1e-4, "sat roundtrip {sat} -> {s2}");
            let dh = ((h2 - hue + 180.0_f32).rem_euclid(360.0_f32)) - 180.0_f32;
            assert!(dh.abs() < 1e-3, "hue roundtrip {hue} -> {h2}");
        }
    }

    #[test]
    fn color_grading_edit_struct_creation() {
        let edit = ColorGradingEdit {
            hue: 120.0_f32,
            sat: 0.5_f32,
            lum: -25.0_f32,
            commit: true,
        };
        assert!(edit.commit);
    }

    fn run_grading_wheel_frames(
        hue: f32,
        sat: f32,
        lum: f32,
        inputs: Vec<egui::RawInput>,
    ) -> Option<ColorGradingEdit> {
        let ctx = egui::Context::default();
        let mut last_edit = None;

        // Warm up frame so egui registers layout rects
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = color_grading_wheel(ui, "test_grading", "Shadows", hue, sat, lum);
            });
        });

        for input in inputs {
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if let Some(edit) =
                        color_grading_wheel(ui, "test_grading", "Shadows", hue, sat, lum)
                    {
                        last_edit = Some(edit);
                    }
                });
            });
        }
        last_edit
    }

    #[test]
    fn color_grading_wheel_no_interaction_returns_none() {
        let edit = run_grading_wheel_frames(0.0, 0.0, 0.0, vec![Default::default()]);
        assert!(
            edit.is_none(),
            "Unmodified wheel without interaction returns None"
        );

        let edit_mod = run_grading_wheel_frames(120.0, 0.5, 20.0, vec![Default::default()]);
        assert!(
            edit_mod.is_none(),
            "Modified wheel without interaction returns None"
        );
    }

    #[test]
    fn color_grading_wheel_lum_slider_independence() {
        let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 300.0));
        let p1 = pos2(140.0, 137.0);
        let p2 = pos2(149.0, 137.0);

        let input_down = egui::RawInput {
            screen_rect: Some(screen_rect),
            time: Some(0.1),
            events: vec![
                egui::Event::PointerMoved(p1),
                egui::Event::PointerButton {
                    pos: p1,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
            ..Default::default()
        };
        let input_drag = egui::RawInput {
            screen_rect: Some(screen_rect),
            time: Some(0.2),
            events: vec![
                egui::Event::PointerMoved(p2),
                egui::Event::PointerButton {
                    pos: p2,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
            ..Default::default()
        };
        let input_up = egui::RawInput {
            screen_rect: Some(screen_rect),
            time: Some(0.3),
            events: vec![
                egui::Event::PointerMoved(p2),
                egui::Event::PointerButton {
                    pos: p2,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                },
            ],
            ..Default::default()
        };

        let edit =
            run_grading_wheel_frames(120.0, 0.5, 50.0, vec![input_down, input_drag, input_up]);
        assert!(edit.is_some(), "Editing lum slider produces an edit");
        let edit = edit.unwrap();
        assert_eq!(
            edit.hue, 120.0_f32,
            "Hue must remain 120.0 when Lum slider is edited"
        );
        assert_eq!(
            edit.sat, 0.5_f32,
            "Sat must remain 0.5 when Lum slider is edited"
        );
        assert!(
            (edit.lum - 0.0_f32).abs() <= 1.0_f32,
            "Lum slider edit to center sets lum near 0.0, got {}",
            edit.lum
        );
    }

    #[test]
    fn color_grading_wheel_reset_button_resets_all() {
        let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 300.0));
        let reset_pos = pos2(284.0, 137.0);

        let input_down = egui::RawInput {
            screen_rect: Some(screen_rect),
            time: Some(0.1),
            events: vec![
                egui::Event::PointerMoved(reset_pos),
                egui::Event::PointerButton {
                    pos: reset_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
            ..Default::default()
        };
        let input_up = egui::RawInput {
            screen_rect: Some(screen_rect),
            time: Some(0.2),
            events: vec![
                egui::Event::PointerMoved(reset_pos),
                egui::Event::PointerButton {
                    pos: reset_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                },
            ],
            ..Default::default()
        };

        let edit = run_grading_wheel_frames(120.0, 0.5, 50.0, vec![input_down, input_up]);
        assert!(edit.is_some(), "Clicking reset button produces an edit");
        let edit = edit.unwrap();
        assert_eq!(edit.hue, 0.0_f32, "Reset resets hue to 0");
        assert_eq!(edit.sat, 0.0_f32, "Reset resets sat to 0");
        assert_eq!(edit.lum, 0.0_f32, "Reset resets lum to 0");
        assert!(edit.commit, "Reset commit flag is true");
    }

    #[test]
    fn color_grading_wheel_reset_unmodified_does_nothing() {
        let screen_rect = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 300.0));
        let reset_pos = pos2(284.0, 137.0);

        let input_down = egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(reset_pos),
                egui::Event::PointerButton {
                    pos: reset_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
            ..Default::default()
        };
        let input_up = egui::RawInput {
            screen_rect: Some(screen_rect),
            events: vec![
                egui::Event::PointerMoved(reset_pos),
                egui::Event::PointerButton {
                    pos: reset_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                },
            ],
            ..Default::default()
        };

        let edit = run_grading_wheel_frames(0.0, 0.0, 0.0, vec![input_down, input_up]);
        assert!(
            edit.is_none(),
            "Clicking reset when unmodified returns None"
        );
    }

    #[test]
    fn test_color_grading_grid_columns() {
        assert_eq!(color_grading_grid_columns(200.0), 1);
        assert_eq!(color_grading_grid_columns(279.9), 1);
        assert_eq!(color_grading_grid_columns(280.0), 2);
        assert_eq!(color_grading_grid_columns(350.0), 2);
    }
}
