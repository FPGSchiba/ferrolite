//! The Grade tab: four color-grading wheels (Shadows/Midtones/Highlights/Global),
//! each with a Lum slider, plus Blending and Balance sliders. Reuses the shared
//! `color_wheel` widget 4× (id-salted). Renders against a `ScopedEdit` (design
//! 2026-07-28 §2, Phase 2b Task 3), so the same widget drives both the global
//! Color Grading and a selected mask's — writes go through `ScopedEdit::write`,
//! which normalizes identity structures away on the doc side. `MaskNone`
//! renders a faint hint and returns `None`. Per-control reset lives on each
//! wheel (its own reset → neutral) and each `EguiSlider` (its reset column).

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::scope::ScopedEdit;
use crate::icons;
use crate::theme;
use crate::widgets::color_wheel;
use crate::widgets::slider::EguiSlider;
use crate::widgets::WheelEdit;
use ferrolite_pipeline::{ColorGrade, GradeWheel, OpKind};

/// True when any wheel or the blending/balance sliders differ (emit gate).
pub(crate) fn grade_changed(a: &ColorGrade, b: &ColorGrade) -> bool {
    a != b
}

/// Draw one wheel + its Lum slider; mutate `wheel` in place. Returns
/// `(changed, commit)`.
fn wheel_row(
    ui: &mut egui::Ui,
    id_source: &'static str,
    label: &str,
    wheel: &mut GradeWheel,
) -> (bool, bool) {
    let mut changed = false;
    let mut commit = false;
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(label).color(theme::TEXT_FAINT));
        let edit: Option<WheelEdit> = color_wheel(ui, id_source, wheel.hue, wheel.sat);
        if let Some(e) = edit {
            wheel.hue = e.hue;
            wheel.sat = e.sat;
            changed = true;
            commit |= e.commit;
        }
    });
    let mut lum = wheel.lum;
    let r = ui.add(
        EguiSlider {
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
        }
        .label_width(38.0),
    );
    if r.changed() {
        wheel.lum = lum;
        changed = true;
        commit |= r.drag_stopped() || !r.dragged();
    }
    (changed, commit)
}

pub fn show(ui: &mut egui::Ui, scoped: &ScopedEdit) -> Option<EditOutcome> {
    let Some(set) = scoped.set() else {
        ui.label(egui::RichText::new("Create or select a mask first").color(theme::TEXT_FAINT));
        return None;
    };
    let mut cg = set.color_grade;
    let before = cg;

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(icons::GRADE).font(icons::font(14.0)));
        ui.label(egui::RichText::new("Color Grading").color(theme::TEXT_FAINT));
    });

    let mut changed = false;
    let mut commit = false;
    let cols = color_wheel::color_grading_grid_columns(ui.available_width());

    if cols == 2 {
        ui.columns(2, |columns| {
            let (c, m) = wheel_row(&mut columns[0], "grade_shadows", "Shadows", &mut cg.shadows);
            changed |= c;
            commit |= m;
            let (c, m) = wheel_row(
                &mut columns[1],
                "grade_midtones",
                "Midtones",
                &mut cg.midtones,
            );
            changed |= c;
            commit |= m;
        });
        ui.add_space(8.0);
        ui.columns(2, |columns| {
            let (c, m) = wheel_row(
                &mut columns[0],
                "grade_highlights",
                "Highlights",
                &mut cg.highlights,
            );
            changed |= c;
            commit |= m;
            let (c, m) = wheel_row(&mut columns[1], "grade_global", "Global", &mut cg.global);
            changed |= c;
            commit |= m;
        });
    } else {
        let wheels = [
            ("grade_shadows", "Shadows", &mut cg.shadows),
            ("grade_midtones", "Midtones", &mut cg.midtones),
            ("grade_highlights", "Highlights", &mut cg.highlights),
            ("grade_global", "Global", &mut cg.global),
        ];
        for (id, label, wheel) in wheels {
            let (c, m) = wheel_row(ui, id, label, wheel);
            changed |= c;
            commit |= m;
        }
    }

    ui.separator();
    let mut blending = cg.blending;
    let rb = ui.add(EguiSlider {
        label: "Blending",
        value: &mut blending,
        min: 0.0,
        max: 1.0,
        default: 0.5,
        step: 0.01,
        decimals: 2,
        unit: "",
        bipolar: false,
        signed: false,
        custom_label_w: None,
    });
    if rb.changed() {
        cg.blending = blending;
        changed = true;
        commit |= rb.drag_stopped() || !rb.dragged();
    }
    let mut balance = cg.balance;
    let rbal = ui.add(EguiSlider {
        label: "Balance",
        value: &mut balance,
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
    if rbal.changed() {
        cg.balance = balance;
        changed = true;
        commit |= rbal.drag_stopped() || !rbal.dragged();
    }

    if !changed || !grade_changed(&before, &cg) {
        return None;
    }
    let mut new_set = set.clone();
    new_set.color_grade = cg;
    let out = scoped.write(new_set, OpKind::ColorGrade, commit);
    if let Some(o) = &out {
        if !o.commit {
            // Mid-drag on a wheel/slider: suppress the mask overlay the same
            // way a dragged `scoped_slider` does.
            scoped.adjusting.set(true);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::scope::{EditScope, ScopedEdit};
    use ferrolite_pipeline::{ColorGrade, GradeWheel, OpStack};

    #[test]
    fn grade_changed_detects_a_wheel_edit() {
        let a = ColorGrade::default();
        let b = ColorGrade {
            shadows: GradeWheel {
                hue: 210.0,
                sat: 0.3,
                lum: 0.0,
            },
            ..Default::default()
        };
        assert!(grade_changed(&a, &b));
        assert!(!grade_changed(&a, &ColorGrade::default()));
    }

    #[test]
    fn grade_changed_detects_a_slider_edit() {
        let a = ColorGrade::default();
        let b = ColorGrade {
            balance: -0.4,
            ..Default::default()
        };
        assert!(grade_changed(&a, &b));
    }

    #[test]
    fn test_color_grading_grid_2x2_layout_math() {
        assert_eq!(color_wheel::color_grading_grid_columns(200.0), 1);
        assert_eq!(color_wheel::color_grading_grid_columns(279.0), 1);
        assert_eq!(color_wheel::color_grading_grid_columns(280.0), 2);
        assert_eq!(color_wheel::color_grading_grid_columns(350.0), 2);
    }

    #[test]
    fn test_show_renders_in_2col_and_1col_layouts() {
        let ctx = egui::Context::default();
        let stack = OpStack::default();
        let scoped = ScopedEdit::new(EditScope::Global, &stack);

        for width in [240.0_f32, 320.0_f32, 450.0_f32] {
            let screen_rect =
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(width, 600.0));
            let input = egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let outcome = show(ui, &scoped);
                    assert!(outcome.is_none(), "Unmodified grade widget returns None");
                });
            });
        }
    }

    #[test]
    fn test_lum_slider_compact_label_width_in_2x2_grid() {
        let ctx = egui::Context::default();
        let stack = OpStack::default();
        let scoped = ScopedEdit::new(EditScope::Global, &stack);
        let width = 350.0_f32;

        let screen_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(width, 600.0));
        let input = egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };

        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = show(ui, &scoped);
            });
        });

        // In 2x2 grid mode (width=350), columns split panel width into ~170px cells.
        // We verify that Lum slider track segment lines are present and have width > 30px
        // (which requires custom_label_w of 32px; with default 74px label width, track width in 170px col would be < 10px).
        let mut horizontal_track_widths = Vec::new();
        for clipped in &output.shapes {
            if let egui::Shape::LineSegment { points, stroke } = &clipped.shape {
                if stroke.width == 2.0 && (points[0].y - points[1].y).abs() < 0.1 {
                    let seg_w = (points[1].x - points[0].x).abs();
                    if seg_w > 10.0 {
                        horizontal_track_widths.push(seg_w);
                    }
                }
            }
        }

        assert!(
            !horizontal_track_widths.is_empty(),
            "Slider tracks should be rendered"
        );
        for &w in &horizontal_track_widths {
            assert!(
                w >= 30.0,
                "Horizontal slider track width in 2x2 grid column should be expanded (got {w}px)"
            );
        }
    }

    #[test]
    fn test_lum_slider_symmetrical_centering_in_2x2_grid() {
        let ctx = egui::Context::default();
        let stack = OpStack::default();
        let scoped = ScopedEdit::new(EditScope::Global, &stack);
        let width = 350.0_f32;

        let screen_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(width, 600.0));
        let input = egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };

        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = show(ui, &scoped);
            });
        });

        let mut track_midpoints = Vec::new();
        for clipped in &output.shapes {
            if let egui::Shape::LineSegment { points, stroke } = &clipped.shape {
                if stroke.width == 2.0 && (points[0].y - points[1].y).abs() < 0.1 {
                    let seg_w = (points[1].x - points[0].x).abs();
                    if seg_w > 10.0 {
                        let track_left = points[0].x.min(points[1].x);
                        let track_right = points[0].x.max(points[1].x);
                        let track_mid = (track_left + track_right) / 2.0;
                        track_midpoints.push((track_left, track_right, track_mid));
                    }
                }
            }
        }

        assert!(
            track_midpoints.len() >= 4,
            "Should render at least 4 wheel row Lum slider tracks"
        );

        for (track_left, track_right, track_mid) in &track_midpoints[..4] {
            if *track_mid < 175.0 {
                let col_left = 8.0_f32;
                let col_right = 171.0_f32;
                let left_m = track_left - col_left;
                let right_m = col_right - track_right;
                assert!(
                    (left_m - right_m).abs() < 1.0,
                    "Column 0 Lum slider track left margin ({left_m}px) and right margin ({right_m}px) should be symmetrical"
                );
            } else {
                let col_left = 179.0_f32;
                let col_right = 342.0_f32;
                let left_m = track_left - col_left;
                let right_m = col_right - track_right;
                assert!(
                    (left_m - right_m).abs() < 1.0,
                    "Column 1 Lum slider track left margin ({left_m}px) and right margin ({right_m}px) should be symmetrical"
                );
            }
        }
    }
}
