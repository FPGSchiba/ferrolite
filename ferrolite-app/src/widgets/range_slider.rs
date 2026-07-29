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
pub fn snap_and_clamp(v: f32, detents: &[f32], other: f32, moving_lo: bool) -> f32 {
    let snapped = nearest_detent(v, detents);
    if moving_lo {
        snapped.min(other)
    } else {
        snapped.max(other)
    }
}

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

/// Pure: format the two-value readout, e.g. `"2.8\u{2013}8.0"` or, with a
/// non-empty `prefix`, `"f/2.8\u{2013}f/8.0"`. `prefix` is printed before
/// BOTH values; `unit` is printed once, after the second value.
fn format_range_readout(lo: f32, hi: f32, decimals: usize, unit: &str, prefix: &str) -> String {
    format!("{prefix}{lo:.decimals$}\u{2013}{prefix}{hi:.decimals$}{unit}")
}

/// Pure: inverse of `track_fraction` — the value at track fraction `frac`
/// on `[min, max]`, log or linear.
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

/// Pure: parse a manually-typed "lo\u{2013}hi" range entry (double-click on the
/// value readout, mirroring `EguiSlider`'s edit-on-double-click). Accepts
/// `"-"`, the en dash `"\u{2013}"`, `".."`, or bare whitespace as the
/// two-value separator (e.g. `"100-400"`, `"2.8 \u{2013} 8.0"`, `"24..70"`,
/// `"24 70"`). A single number with no separator sets BOTH ends to that value
/// (a point filter, e.g. typing `"400"` narrows the range to exactly 400).
/// Each parsed end is clamped to `[min, max]`; if the parsed low end would
/// exceed the high end the two are swapped so the result always satisfies
/// `lo <= hi`. Returns `None` if the trimmed input doesn't parse as one or two
/// finite numbers (parse failure is the caller's cue to keep the old values).
///
/// Manually-entered values are NOT snapped to the widget's `detents` — detents
/// are a drag affordance to make aiming the handles easier; typed input is
/// assumed to already be the precise value the user wants.
pub fn parse_range_entry(s: &str, min: f32, max: f32) -> Option<(f32, f32)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let parts: Vec<&str> = if let Some(idx) = s.find("..") {
        vec![&s[..idx], &s[idx + 2..]]
    } else if let Some(idx) = s.find('\u{2013}') {
        vec![&s[..idx], &s[idx + '\u{2013}'.len_utf8()..]]
    } else if let Some(idx) = s.find('-') {
        vec![&s[..idx], &s[idx + 1..]]
    } else {
        s.split_whitespace().collect()
    };

    let clamp = |v: f32| v.clamp(min, max);
    let parse_one = |raw: &str| -> Option<f32> {
        let v: f32 = raw.trim().parse().ok()?;
        v.is_finite().then_some(v)
    };

    match parts.as_slice() {
        [single] => {
            let v = clamp(parse_one(single)?);
            Some((v, v))
        }
        [a, b] => {
            let a = clamp(parse_one(a)?);
            let b = clamp(parse_one(b)?);
            if a <= b {
                Some((a, b))
            } else {
                Some((b, a))
            }
        }
        _ => None,
    }
}

/// Pure: build a sorted, duplicate-free detent ladder from `(start, end,
/// step)` triples, e.g. the Focal-length filter's graduated steps — 1mm in
/// `[8,50)`, 5mm in `[50,200)`, 10mm in `[200,600)`, 50mm in `[600,1200]`:
/// `graduated_detents(&[(8.0,50.0,1.0),(50.0,200.0,5.0),(200.0,600.0,10.0),(600.0,1200.0,50.0)])`.
/// Each triple contributes `start, start+step, ...` up to and including `end`;
/// a boundary shared between adjacent triples (e.g. `50.0`, which ends the
/// first triple and starts the second) is included only once in the result.
/// A triple with a non-positive `step` or `end <= start` is skipped.
pub fn graduated_detents(ranges: &[(f32, f32, f32)]) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::new();
    for &(start, end, step) in ranges {
        if step <= 0.0 || end <= start {
            continue;
        }
        let mut v = start;
        while v < end {
            out.push(v);
            v += step;
        }
        out.push(end);
    }
    out.sort_by(|a, b| a.partial_cmp(b).expect("detent values are finite"));
    out.dedup_by(|a, b| (*a - *b).abs() < f32::EPSILON);
    out
}

/// Dual-handle range slider: filters a value to a `[lo, hi]` sub-range of
/// `[min, max]`. Reset restores the full range (`lo = min, hi = max`).
/// Double-clicking the value readout opens a small inline `TextEdit` seeded
/// with the current "lo\u{2013}hi" text (see `parse_range_entry`); Escape
/// cancels, Enter/lost-focus commits. Manually-typed values are clamped to
/// `[min, max]` but are NOT snapped to `detents` — detents are a drag
/// affordance, not a constraint on precise typed input.
///
/// Consumed by the Library Metadata popup's ISO/aperture/focal filters
/// (`library::toolbar`).
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
    /// Printed before EACH of the two values in the readout (e.g. `"f/"` for
    /// aperture: `"f/2.8\u{2013}f/8.0"`). Empty for ranges with no such
    /// marker (ISO, focal length).
    pub value_prefix: &'static str,
}

use crate::theme;
use egui::{pos2, vec2, Color32, Response, Sense, Stroke, Ui, Widget};

// Design-system §5 slider tokens (mirrors slider.rs; widened value column to
// fit the two-number "{lo}\u{2013}{hi}{unit}" readout).
const TRACK: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x3a);
const FILL_IDLE: Color32 = Color32::from_rgb(0x58, 0x58, 0x58);
const HANDLE_IDLE: Color32 = Color32::from_rgb(0x9a, 0x9a, 0x9a);
const HANDLE_BORDER: Color32 = Color32::from_rgb(0x16, 0x16, 0x16);
const LABEL: Color32 = Color32::from_rgb(0x8c, 0x8c, 0x8c);
const VALUE_IDLE: Color32 = Color32::from_rgb(0xbd, 0xbd, 0xbd);

const LABEL_W: f32 = 74.0;
const VALUE_W: f32 = 92.0;
const ROW_H: f32 = 22.0;
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

        // Double-click-to-edit, mirroring `EguiSlider`: the edit-in-progress
        // text (if any) lives in temp data keyed off this widget's id, seeded
        // on the double-click frame with the plain "lo\u{2013}hi" text (no
        // prefix/unit — see `parse_range_entry`).
        let edit_id = response.id.with("range_entry");
        let mut editing = ui.data_mut(|d| d.get_temp::<String>(edit_id));
        let mut newly_entered = false;

        if response.double_clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                if value_rect.contains(p) {
                    let seed = format!("{:.*}\u{2013}{:.*}", self.decimals, lo, self.decimals, hi);
                    ui.data_mut(|d| d.insert_temp(edit_id, seed.clone()));
                    editing = Some(seed);
                    newly_entered = true;
                }
            }
        }

        let frac_lo = track_fraction(lo, self.min, self.max, self.log);
        let frac_hi = track_fraction(hi, self.min, self.max, self.log);
        let hx_lo = track_left + frac_lo * track_w;
        let hx_hi = track_left + frac_hi * track_w;

        // Which handle is being moved is decided once, on pointer-down /
        // click, by nearest-handle-to-pointer; it is then persisted across
        // the drag's remaining frames so the pointer crossing the other
        // handle mid-drag never flips which one is moving.
        let handle_id = response.id.with("active_handle");
        // Suppressed while editing so the text-entry double-click doesn't
        // also register as a drag/click on the handles underneath it.
        if editing.is_none() {
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

        // Value region: inline text entry while editing, plain text otherwise
        // (mirrors `EguiSlider`'s edit-on-double-click value column).
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
                if let Some((new_lo, new_hi)) = parse_range_entry(&buf, self.min, self.max) {
                    lo = new_lo;
                    hi = new_hi;
                    response.mark_changed();
                    *self.lo = lo;
                    *self.hi = hi;
                }
                ui.data_mut(|d| d.remove::<String>(edit_id));
            } else {
                ui.data_mut(|d| d.insert_temp(edit_id, buf));
            }
        } else {
            let value_color = if active { theme::ACCENT } else { VALUE_IDLE };
            ui.painter().text(
                value_rect.right_center(),
                egui::Align2::RIGHT_CENTER,
                format_range_readout(lo, hi, self.decimals, self.unit, self.value_prefix),
                egui::FontId::monospace(11.0),
                value_color,
            );
        }

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
    fn format_range_readout_applies_prefix_to_both_values() {
        assert_eq!(
            format_range_readout(2.8, 8.0, 1, "", "f/"),
            "f/2.8\u{2013}f/8.0"
        );
    }

    #[test]
    fn format_range_readout_empty_prefix_matches_plain_readout() {
        assert_eq!(
            format_range_readout(100.0, 400.0, 0, "", ""),
            "100\u{2013}400"
        );
        assert_eq!(
            format_range_readout(24.0, 70.0, 0, " mm", ""),
            "24\u{2013}70 mm"
        );
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

    // ── parse_range_entry (manual double-click input) ───────────────────────

    #[test]
    fn parse_range_entry_parses_both_ends_with_hyphen() {
        assert_eq!(
            parse_range_entry("100-400", 50.0, 102_400.0),
            Some((100.0, 400.0))
        );
    }

    #[test]
    fn parse_range_entry_parses_both_ends_with_en_dash_and_spaces() {
        assert_eq!(
            parse_range_entry("2.8 \u{2013} 8.0", 0.7, 32.0),
            Some((2.8, 8.0))
        );
    }

    #[test]
    fn parse_range_entry_parses_both_ends_with_dotdot() {
        assert_eq!(parse_range_entry("24..70", 8.0, 1200.0), Some((24.0, 70.0)));
    }

    #[test]
    fn parse_range_entry_parses_both_ends_with_whitespace() {
        assert_eq!(parse_range_entry("24 70", 8.0, 1200.0), Some((24.0, 70.0)));
    }

    #[test]
    fn parse_range_entry_single_value_sets_both_ends() {
        // A bare number with no separator is a "point filter": both handles
        // land on the same value.
        assert_eq!(parse_range_entry("400", 8.0, 1200.0), Some((400.0, 400.0)));
    }

    #[test]
    fn parse_range_entry_swaps_a_reversed_range() {
        assert_eq!(
            parse_range_entry("400-100", 50.0, 102_400.0),
            Some((100.0, 400.0))
        );
    }

    #[test]
    fn parse_range_entry_clamps_each_end_to_min_max() {
        assert_eq!(
            parse_range_entry("1-999999", 8.0, 1200.0),
            Some((8.0, 1200.0))
        );
    }

    #[test]
    fn parse_range_entry_rejects_garbage() {
        assert_eq!(parse_range_entry("abc-def", 0.0, 100.0), None);
        assert_eq!(parse_range_entry("nonsense", 0.0, 100.0), None);
    }

    #[test]
    fn parse_range_entry_rejects_empty() {
        assert_eq!(parse_range_entry("", 0.0, 100.0), None);
        assert_eq!(parse_range_entry("   ", 0.0, 100.0), None);
    }

    // ── graduated_detents (Focal-length filter) ─────────────────────────────

    #[test]
    fn graduated_detents_focal_ladder_is_sorted_with_no_duplicates_and_has_boundaries() {
        let ranges = [
            (8.0, 50.0, 1.0),
            (50.0, 200.0, 5.0),
            (200.0, 600.0, 10.0),
            (600.0, 1200.0, 50.0),
        ];
        let detents = graduated_detents(&ranges);

        // Strictly increasing implies both "sorted" and "no duplicates" in
        // one assertion (a duplicate would fail `w[0] < w[1]`).
        assert!(
            detents.windows(2).all(|w| w[0] < w[1]),
            "detents not strictly increasing: {detents:?}"
        );

        for boundary in [8.0, 50.0, 200.0, 600.0, 1200.0] {
            assert!(
                detents.iter().any(|d| (d - boundary).abs() < f32::EPSILON),
                "missing boundary {boundary} in {detents:?}"
            );
        }

        // Endpoints appear exactly once each (covered by strictly-increasing
        // above, but spelled out for clarity): 1200.0 is the very last entry.
        assert_eq!(detents.last().copied(), Some(1200.0));
        assert_eq!(detents.first().copied(), Some(8.0));
    }

    #[test]
    fn graduated_detents_skips_degenerate_ranges() {
        // Zero/negative step and end<=start ranges are dropped rather than
        // looping forever or producing garbage.
        let detents = graduated_detents(&[(10.0, 20.0, 0.0), (30.0, 20.0, 1.0)]);
        assert!(detents.is_empty());
    }

    #[test]
    fn graduated_detents_single_range_matches_manual_step() {
        let detents = graduated_detents(&[(0.0, 3.0, 1.0)]);
        assert_eq!(detents, vec![0.0, 1.0, 2.0, 3.0]);
    }
}
