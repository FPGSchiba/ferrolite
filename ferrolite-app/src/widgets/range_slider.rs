//! Dual-handle range slider (min/max filter widget). Visual language mirrors
//! `EguiSlider` (`widgets/slider.rs`): label column, track, value readout,
//! per-control reset column. See docs/design/ferrolite-design-system.md §5.

/// Pure: snap `v` to the nearest entry in `detents`, then clamp so that
/// `lo <= hi` holds for whichever handle is being moved (`moving_lo`).
/// `other` is the *other* handle's current value (the one not being moved).
/// Empty `detents` disables snapping (returns `v` unchanged before the clamp).
///
/// Note: the clamp step can leave the moved handle off-detent when `other`
/// itself isn't detent-aligned (e.g. seeded from stored/external state) —
/// the post-condition is "snapped, then clamped to `other`", not
/// unconditionally "lands on a detent".
///
/// Consumed by `RangeSlider` (used by Task 7); allowed dead-code until then.
#[allow(dead_code)]
pub fn snap_and_clamp(v: f32, detents: &[f32], other: f32, moving_lo: bool) -> f32 {
    let snapped = nearest_detent(v, detents);
    if moving_lo {
        snapped.min(other)
    } else {
        snapped.max(other)
    }
}

#[allow(dead_code)]
fn nearest_detent(v: f32, detents: &[f32]) -> f32 {
    let Some(&first) = detents.first() else {
        return v;
    };
    detents.iter().copied().fold(first, |best, d| {
        if (d - v).abs() < (best - v).abs() {
            d
        } else {
            best
        }
    })
}

/// Pure: track fraction for value `v` on `[min, max]`, in 0.0..=1.0.
/// Log-scaled when `log` is true (ISO/aperture-style ranges), linear otherwise.
#[allow(dead_code)]
pub fn track_fraction(v: f32, min: f32, max: f32, log: bool) -> f32 {
    if log {
        debug_assert!(
            min > 0.0 && max > min,
            "log-mode track_fraction requires 0 < min < max, got min={min}, max={max}"
        );
        // Ranges using log scaling (ISO, aperture) are always strictly
        // positive; floor at a tiny positive value to keep `ln` finite.
        // (Release builds fall back to this floor instead of asserting, so a
        // misconfiguration compresses the track rather than panicking.)
        let floor = f32::MIN_POSITIVE;
        let (lv, lmin, lmax) = (v.max(floor).ln(), min.max(floor).ln(), max.max(floor).ln());
        if (lmax - lmin).abs() < f32::EPSILON {
            return 0.0;
        }
        ((lv - lmin) / (lmax - lmin)).clamp(0.0, 1.0)
    } else {
        super::slider::math::fraction(v, min, max)
    }
}

/// Pure: inverse of `track_fraction` — the value at track fraction `frac`
/// on `[min, max]`, log or linear.
#[allow(dead_code)]
fn value_at_fraction(frac: f32, min: f32, max: f32, log: bool) -> f32 {
    let frac = frac.clamp(0.0, 1.0);
    if log {
        let floor = f32::MIN_POSITIVE;
        let (lmin, lmax) = (min.max(floor).ln(), max.max(floor).ln());
        (lmin + frac * (lmax - lmin)).exp()
    } else {
        min + frac * (max - min)
    }
}

/// Dual-handle range slider: filters a value to a `[lo, hi]` sub-range of
/// `[min, max]`. Reset restores the full range (`lo = min, hi = max`).
///
/// Consumed by Task 7 (Library Metadata popup ISO/aperture/focal filters);
/// nothing constructs it yet, hence the blanket `dead_code` allowance below.
#[allow(dead_code)]
pub struct RangeSlider<'a> {
    pub label: &'static str,
    pub lo: &'a mut f32,
    pub hi: &'a mut f32,
    pub min: f32,
    pub max: f32,
    /// Detents to snap handles to (sorted ascending, includes min & max).
    pub detents: &'a [f32],
    /// Log-scaled track position when true (ISO/aperture), linear otherwise.
    pub log: bool,
    pub decimals: usize,
    pub unit: &'static str,
}

use crate::theme;
use egui::{pos2, vec2, Color32, Response, Sense, Stroke, Ui, Widget};

// Design-system §5 slider tokens (mirrors slider.rs; widened value column to
// fit the two-number "{lo}\u{2013}{hi}{unit}" readout). Unused until Task 7
// constructs a `RangeSlider` and adds it to a `Ui`.
#[allow(dead_code)]
const TRACK: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x3a);
#[allow(dead_code)]
const FILL_IDLE: Color32 = Color32::from_rgb(0x58, 0x58, 0x58);
#[allow(dead_code)]
const HANDLE_IDLE: Color32 = Color32::from_rgb(0x9a, 0x9a, 0x9a);
#[allow(dead_code)]
const HANDLE_BORDER: Color32 = Color32::from_rgb(0x16, 0x16, 0x16);
#[allow(dead_code)]
const LABEL: Color32 = Color32::from_rgb(0x8c, 0x8c, 0x8c);
#[allow(dead_code)]
const VALUE_IDLE: Color32 = Color32::from_rgb(0xbd, 0xbd, 0xbd);

#[allow(dead_code)]
const LABEL_W: f32 = 74.0;
#[allow(dead_code)]
const VALUE_W: f32 = 92.0;
#[allow(dead_code)]
const ROW_H: f32 = 22.0;
#[allow(dead_code)]
const RESET_W: f32 = 16.0;

impl<'a> Widget for RangeSlider<'a> {
    fn ui(self, ui: &mut Ui) -> Response {
        let full = ui.available_width();
        let (rect, mut response) =
            ui.allocate_exact_size(vec2(full, ROW_H), Sense::click_and_drag());

        let label_w = if self.label.is_empty() { 0.0 } else { LABEL_W };
        let track_left = rect.left() + label_w + 8.0;
        let right_m = VALUE_W + 12.0 + RESET_W + 8.0;
        let track_right = rect.right() - right_m;
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

        let mut lo = *self.lo;
        let mut hi = *self.hi;

        let frac_lo = track_fraction(lo, self.min, self.max, self.log);
        let frac_hi = track_fraction(hi, self.min, self.max, self.log);
        let hx_lo = track_left + frac_lo * track_w;
        let hx_hi = track_left + frac_hi * track_w;

        // Which handle is being moved is decided once, on pointer-down /
        // click, by nearest-handle-to-pointer; it is then persisted across
        // the drag's remaining frames so the pointer crossing the other
        // handle mid-drag never flips which one is moving.
        let handle_id = response.id.with("active_handle");
        if let Some(p) = response.interact_pointer_pos() {
            if (response.dragged() || response.clicked()) && p.x <= track_right + 8.0 {
                let moving_lo = if response.drag_started() || response.clicked() {
                    let choice = (p.x - hx_lo).abs() <= (p.x - hx_hi).abs();
                    ui.data_mut(|d| d.insert_temp(handle_id, choice));
                    choice
                } else {
                    ui.data_mut(|d| d.get_temp::<bool>(handle_id).unwrap_or(true))
                };

                let frac = ((p.x - track_left) / track_w).clamp(0.0, 1.0);
                let raw = value_at_fraction(frac, self.min, self.max, self.log);
                if moving_lo {
                    let new_lo = snap_and_clamp(raw, self.detents, hi, true);
                    if (new_lo - lo).abs() > f32::EPSILON {
                        lo = new_lo;
                        response.mark_changed();
                    }
                } else {
                    let new_hi = snap_and_clamp(raw, self.detents, lo, false);
                    if (new_hi - hi).abs() > f32::EPSILON {
                        hi = new_hi;
                        response.mark_changed();
                    }
                }
            }
        }

        let reset_resp = ui.interact(reset_rect, response.id.with("reset"), Sense::click());
        let modified = (lo - self.min).abs() > f32::EPSILON || (hi - self.max).abs() > f32::EPSILON;
        if reset_resp.clicked() && modified {
            lo = self.min;
            hi = self.max;
            response.mark_changed();
        }

        *self.lo = lo;
        *self.hi = hi;

        // Commit-on-release, mirroring EguiSlider: the drag applies live
        // (marked changed on each moved frame, so callers see previews), but
        // the drag-STOP frame moves no pointer and matches none of the
        // mark-changed branches above, so it must be marked changed
        // explicitly — it's the only frame where `drag_stopped()` is true,
        // which is what commit-style callers gate persistence on.
        if response.drag_stopped() {
            response.mark_changed();
            ui.data_mut(|d| d.remove::<bool>(handle_id));
        }

        let active = response.dragged();
        let frac_lo = track_fraction(lo, self.min, self.max, self.log);
        let frac_hi = track_fraction(hi, self.min, self.max, self.log);

        {
            let painter = ui.painter();
            if !self.label.is_empty() {
                painter.text(
                    pos2(rect.left() + 4.0, mid_y),
                    egui::Align2::LEFT_CENTER,
                    self.label,
                    egui::FontId::proportional(11.0),
                    LABEL,
                );
            }
            // base track line
            painter.line_segment(
                [pos2(track_left, mid_y), pos2(track_right, mid_y)],
                Stroke::new(2.0_f32, TRACK),
            );
            // fill between the two handles (not from track start)
            let fill_color = if active { theme::ACCENT } else { FILL_IDLE };
            painter.line_segment(
                [
                    pos2(track_left + frac_lo * track_w, mid_y),
                    pos2(track_left + frac_hi * track_w, mid_y),
                ],
                Stroke::new(2.0_f32, fill_color),
            );
            // handles
            let handle_color = if active {
                theme::ACCENT_BRIGHT
            } else {
                HANDLE_IDLE
            };
            for frac in [frac_lo, frac_hi] {
                painter.circle(
                    pos2(track_left + frac * track_w, mid_y),
                    5.5,
                    handle_color,
                    Stroke::new(1.0_f32, HANDLE_BORDER),
                );
            }
            // reset icon: dim when already at [min, max]
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

        let value_color = if active { theme::ACCENT } else { VALUE_IDLE };
        ui.painter().text(
            value_rect.right_center(),
            egui::Align2::RIGHT_CENTER,
            format!(
                "{:.*}\u{2013}{:.*}{}",
                self.decimals, lo, self.decimals, hi, self.unit
            ),
            egui::FontId::monospace(11.0),
            value_color,
        );

        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_and_clamp_picks_nearest_detent() {
        let detents = [50.0, 100.0, 200.0, 400.0];
        assert_eq!(snap_and_clamp(120.0, &detents, 400.0, true), 100.0);
        assert_eq!(snap_and_clamp(180.0, &detents, 50.0, false), 200.0);
    }

    #[test]
    fn snap_and_clamp_clamps_lo_to_hi() {
        let detents = [50.0, 100.0, 200.0, 400.0];
        // 250 snaps to 200, but hi is 150 -> lo must clamp down to hi.
        assert_eq!(snap_and_clamp(250.0, &detents, 150.0, true), 150.0);
    }

    #[test]
    fn snap_and_clamp_clamps_hi_to_lo() {
        let detents = [50.0, 100.0, 200.0, 400.0];
        // 80 snaps to 100, but lo is 120 -> hi must clamp up to lo.
        assert_eq!(snap_and_clamp(80.0, &detents, 120.0, false), 120.0);
    }

    #[test]
    fn snap_and_clamp_with_no_detents_only_clamps() {
        assert_eq!(snap_and_clamp(123.4, &[], 100.0, true), 100.0);
        assert_eq!(snap_and_clamp(90.0, &[], 100.0, false), 100.0);
    }

    #[test]
    fn track_fraction_linear_bounds_and_monotone() {
        assert_eq!(track_fraction(0.0, 0.0, 100.0, false), 0.0);
        assert_eq!(track_fraction(100.0, 0.0, 100.0, false), 1.0);
        assert!(track_fraction(25.0, 0.0, 100.0, false) < track_fraction(75.0, 0.0, 100.0, false));
    }

    #[test]
    fn track_fraction_log_bounds() {
        assert!((track_fraction(50.0, 50.0, 102400.0, true) - 0.0).abs() < 1e-6);
        assert!((track_fraction(102400.0, 50.0, 102400.0, true) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn track_fraction_log_matches_formula() {
        let got = track_fraction(100.0, 50.0, 102400.0, true);
        let expected = (100f32.ln() - 50f32.ln()) / (102400f32.ln() - 50f32.ln());
        assert!(
            (got - expected).abs() < 1e-5,
            "got {got}, expected {expected}"
        );
    }

    #[test]
    fn track_fraction_log_is_monotone() {
        let a = track_fraction(200.0, 50.0, 102400.0, true);
        let b = track_fraction(6400.0, 50.0, 102400.0, true);
        assert!(a < b);
    }

    #[test]
    fn value_at_fraction_round_trips_track_fraction_linear() {
        let v = value_at_fraction(0.5, 0.0, 100.0, false);
        assert!((v - 50.0).abs() < 1e-4);
    }

    #[test]
    fn value_at_fraction_round_trips_track_fraction_log() {
        let f = track_fraction(6400.0, 50.0, 102400.0, true);
        let v = value_at_fraction(f, 50.0, 102400.0, true);
        assert!((v - 6400.0).abs() < 1.0, "expected ~6400, got {v}");
    }
}
