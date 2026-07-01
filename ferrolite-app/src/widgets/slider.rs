//! Lightroom-style horizontal slider. See docs/design/ferrolite-design-system.md §5.

/// Pure value math, independent of egui — unit tested.
pub mod math {
    pub fn fraction(value: f32, min: f32, max: f32) -> f32 {
        if (max - min).abs() < f32::EPSILON {
            return 0.0;
        }
        ((value - min) / (max - min)).clamp(0.0, 1.0)
    }

    pub fn snap(value: f32, step: f32, min: f32, max: f32) -> f32 {
        let snapped = if step > 0.0 {
            (value / step).round() * step
        } else {
            value
        };
        snapped.clamp(min, max)
    }

    pub fn value_at(fraction: f32, min: f32, max: f32, step: f32) -> f32 {
        let raw = min + fraction.clamp(0.0, 1.0) * (max - min);
        snap(raw, step, min, max)
    }

    /// Returns (left, width) in 0..=1 of the filled portion of the track.
    pub fn fill(frac: f32, min: f32, max: f32, bipolar: bool) -> (f32, f32) {
        if bipolar {
            let zero = fraction(0.0, min, max);
            let a = zero.min(frac);
            let b = zero.max(frac);
            (a, b - a)
        } else {
            (0.0, frac)
        }
    }

    pub fn format(value: f32, decimals: usize, unit: &str, signed: bool) -> String {
        let sign = if signed && value > 0.0 { "+" } else { "" };
        format!("{sign}{value:.decimals$}{unit}")
    }

    /// Parse a user-typed value, clamped to [min,max] and snapped to `step`.
    /// Returns None if the trimmed input isn't a finite number.
    pub fn parse_entry(s: &str, min: f32, max: f32, step: f32) -> Option<f32> {
        let v: f32 = s.trim().parse().ok()?;
        if !v.is_finite() {
            return None;
        }
        Some(snap(v, step, min, max)) // snap already clamps to [min,max]
    }
}

/// The widget handle (egui rendering added in Task 4).
pub struct EguiSlider<'a> {
    pub label: &'a str,
    pub value: &'a mut f32,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub step: f32,
    pub decimals: usize,
    pub unit: &'a str,
    pub bipolar: bool,
    pub signed: bool,
}

use crate::theme;
use egui::{pos2, vec2, Color32, Response, Sense, Stroke, Ui, Widget};

// Design-system §5 slider tokens.
const TRACK: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x3a);
const FILL_IDLE: Color32 = Color32::from_rgb(0x58, 0x58, 0x58);
const HANDLE_IDLE: Color32 = Color32::from_rgb(0x9a, 0x9a, 0x9a);
const HANDLE_BORDER: Color32 = Color32::from_rgb(0x16, 0x16, 0x16);
const LABEL: Color32 = Color32::from_rgb(0x8c, 0x8c, 0x8c);
const VALUE_IDLE: Color32 = Color32::from_rgb(0xbd, 0xbd, 0xbd);

const LABEL_W: f32 = 74.0;
const VALUE_W: f32 = 48.0;
const ROW_H: f32 = 22.0;
const RESET_W: f32 = 16.0;

impl<'a> Widget for EguiSlider<'a> {
    fn ui(self, ui: &mut Ui) -> Response {
        let full = ui.available_width();
        let (rect, mut response) =
            ui.allocate_exact_size(vec2(full, ROW_H), Sense::click_and_drag());

        let track_left = rect.left() + LABEL_W + 8.0;
        let track_right = rect.right() - VALUE_W - 8.0 - RESET_W;
        let track_w = (track_right - track_left).max(1.0);
        let mid_y = rect.center().y;
        let reset_rect = egui::Rect::from_min_max(
            pos2(rect.right() - RESET_W, rect.top()),
            rect.right_bottom(),
        );
        let reset_c = reset_rect.center();
        let value_rect = egui::Rect::from_min_max(
            pos2(track_right + 8.0, rect.top()),
            pos2(rect.right() - RESET_W - 4.0, rect.bottom()),
        );

        let edit_id = response.id.with("slider_entry");
        let mut editing = ui.data_mut(|d| d.get_temp::<String>(edit_id));
        let mut newly_entered = false;

        let mut value = *self.value;
        if response.double_clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                if value_rect.contains(p) {
                    // Enter edit mode; seed with the plain numeric value (no unit/sign).
                    let seed = format!("{:.*}", self.decimals, value);
                    ui.data_mut(|d| d.insert_temp(edit_id, seed.clone()));
                    editing = Some(seed);
                    newly_entered = true;
                } else {
                    value = self.default;
                    response.mark_changed();
                }
            } else {
                value = self.default;
                response.mark_changed();
            }
        } else if editing.is_none() {
            if let Some(p) = response.interact_pointer_pos() {
                if (response.dragged() || response.clicked()) && p.x <= track_right + 8.0 {
                    let frac = ((p.x - track_left) / track_w).clamp(0.0, 1.0);
                    let new = math::value_at(frac, self.min, self.max, self.step);
                    if (new - value).abs() > f32::EPSILON {
                        value = new;
                        response.mark_changed();
                    }
                }
            }
        }

        let reset_resp = ui.interact(reset_rect, response.id.with("reset"), Sense::click());
        let modified = (value - self.default).abs() > f32::EPSILON;
        if reset_resp.clicked() && modified {
            value = self.default;
            response.mark_changed();
        }

        *self.value = value;

        // `active` reflects this frame's interaction state; read after writeback is intentional (writeback doesn't affect `response`).
        let active = response.dragged();
        let frac = math::fraction(value, self.min, self.max);
        let (fill_left, fill_w) = math::fill(frac, self.min, self.max, self.bipolar);

        {
            let painter = ui.painter();
            // label
            painter.text(
                pos2(rect.left() + 4.0, mid_y),
                egui::Align2::LEFT_CENTER,
                self.label,
                egui::FontId::proportional(11.0),
                LABEL,
            );
            // base track line
            painter.line_segment(
                [pos2(track_left, mid_y), pos2(track_right, mid_y)],
                Stroke::new(2.0, TRACK),
            );
            // fill
            let fill_color = if active { theme::ACCENT } else { FILL_IDLE };
            painter.line_segment(
                [
                    pos2(track_left + fill_left * track_w, mid_y),
                    pos2(track_left + (fill_left + fill_w) * track_w, mid_y),
                ],
                Stroke::new(2.0, fill_color),
            );
            // handle
            let hx = track_left + frac * track_w;
            let handle_color = if active {
                theme::ACCENT_BRIGHT
            } else {
                HANDLE_IDLE
            };
            painter.circle(
                pos2(hx, mid_y),
                5.5,
                handle_color,
                Stroke::new(1.0, HANDLE_BORDER),
            );
            // reset icon: small circular-arrow, dim when already at default
            let reset_color = if modified {
                if reset_resp.hovered() {
                    theme::ACCENT_BRIGHT
                } else {
                    HANDLE_IDLE
                }
            } else {
                theme::BORDER_STRONG
            };
            super::draw_reset_arrow(painter, reset_c, 4.5, reset_color);
        }

        // Value region: inline text entry while editing, plain text otherwise.
        if let Some(mut buf) = editing {
            let te = ui.put(
                value_rect,
                egui::TextEdit::singleline(&mut buf)
                    .font(egui::TextStyle::Monospace)
                    .horizontal_align(egui::Align::Max)
                    .desired_width(VALUE_W)
                    .margin(egui::Margin::ZERO),
            );
            if newly_entered {
                te.request_focus();
            }

            let escape_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if escape_pressed {
                // Escape wins over lost_focus-triggered commit.
                ui.data_mut(|d| d.remove::<String>(edit_id));
            } else if te.lost_focus() {
                if let Some(v) = math::parse_entry(&buf, self.min, self.max, self.step) {
                    value = v;
                    response.mark_changed();
                    *self.value = value;
                }
                ui.data_mut(|d| d.remove::<String>(edit_id));
            } else {
                ui.data_mut(|d| d.insert_temp(edit_id, buf));
            }
        } else {
            let value_color = if active { theme::ACCENT } else { VALUE_IDLE };
            ui.painter().text(
                pos2(rect.right() - RESET_W - 4.0, mid_y),
                egui::Align2::RIGHT_CENTER,
                math::format(value, self.decimals, self.unit, self.signed),
                egui::FontId::monospace(11.0),
                value_color,
            );
        }

        response
    }
}

#[cfg(test)]
mod tests {
    use super::math::*;

    #[test]
    fn fraction_is_clamped() {
        assert_eq!(fraction(50.0, 0.0, 100.0), 0.5);
        assert_eq!(fraction(-10.0, 0.0, 100.0), 0.0);
        assert_eq!(fraction(200.0, 0.0, 100.0), 1.0);
    }

    #[test]
    fn snap_rounds_to_step_and_clamps() {
        assert_eq!(snap(103.0, 50.0, 50.0, 25600.0), 100.0);
        assert_eq!(snap(40.0, 50.0, 50.0, 25600.0), 50.0); // clamps up to min
    }

    #[test]
    fn value_at_maps_fraction() {
        // ISO slider: min 50, max 25600, step 50, midpoint
        let v = value_at(0.5, 50.0, 25600.0, 50.0);
        assert!((v - 12800.0).abs() <= 50.0);
    }

    #[test]
    fn unipolar_fill_runs_from_left() {
        assert_eq!(fill(0.7, 0.0, 100.0, false), (0.0, 0.7));
    }

    #[test]
    fn bipolar_fill_spans_zero() {
        // min -100, max 100; value -50 -> frac 0.25; zero -> 0.5
        let (left, width) = fill(0.25, -100.0, 100.0, true);
        assert!((left - 0.25).abs() < 1e-6);
        assert!((width - 0.25).abs() < 1e-6);
    }

    #[test]
    fn format_signs_and_units() {
        assert_eq!(format(0.35, 2, " EV", true), "+0.35 EV");
        assert_eq!(format(-46.0, 0, "", true), "-46");
        assert_eq!(format(5450.0, 0, " K", false), "5450 K");
    }

    #[test]
    fn parse_entry_returns_snapped_in_range_value() {
        assert_eq!(parse_entry("50.0", 0.0, 100.0, 0.0), Some(50.0));
    }

    #[test]
    fn parse_entry_clamps_out_of_range_to_min_or_max() {
        assert_eq!(parse_entry("-10", 0.0, 100.0, 1.0), Some(0.0));
        assert_eq!(parse_entry("200", 0.0, 100.0, 1.0), Some(100.0));
    }

    #[test]
    fn parse_entry_snaps_to_step() {
        let got = parse_entry("0.357", 0.0, 1.0, 0.01).expect("finite number");
        assert!((got - 0.36).abs() < 1e-5, "expected ~0.36, got {got}");
        assert_eq!(parse_entry("3.7", 0.0, 10.0, 1.0), Some(4.0));
    }

    #[test]
    fn parse_entry_rejects_garbage() {
        assert_eq!(parse_entry("abc", 0.0, 100.0, 1.0), None);
    }

    #[test]
    fn parse_entry_rejects_empty() {
        assert_eq!(parse_entry("", 0.0, 100.0, 1.0), None);
        assert_eq!(parse_entry("   ", 0.0, 100.0, 1.0), None);
    }

    #[test]
    fn parse_entry_accepts_leading_plus_sign() {
        assert_eq!(parse_entry("+0.5", -1.0, 1.0, 0.0), Some(0.5));
    }
}
