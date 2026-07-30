//! Tone-curve adapter over the reusable `widgets::curve::curve_editor`. Adds a
//! Master/R/G/B channel selector (tinted per channel) above the curve; each
//! channel edits its own `PointCurve` (Master = the legacy `points`/`mode`).
//! Parametric H/S/W/B region controls live in their own REGION TONES section
//! (see `base_tabs::LightTab::show`), not here.
//! Renders against a `ScopedEdit` (design 2026-07-28 §2, Phase 2b Task 3), so
//! the same widget drives both the global Tone Curve and a selected mask's —
//! writes go through `ScopedEdit::write`, which normalizes identity structures
//! away on the doc side. `MaskNone` renders a faint hint and returns `None`.
//! Active channel is UI-only state.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::curve_math;
use crate::develop::scope::{ScopedEdit, MASK_NONE_HINT};
use crate::theme;
use crate::widgets::curve::{curve_editor, CurveStyle};
use egui::Color32;
use ferrolite_pipeline::{CurveMode, OpKind, PointCurve, ToneCurve};

/// Which tone-curve channel the editor is currently editing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channel {
    Master,
    Red,
    Green,
    Blue,
}

impl Channel {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Channel::Master => "Master",
            Channel::Red => "R",
            Channel::Green => "G",
            Channel::Blue => "B",
        }
    }
    const ALL: [Channel; 4] = [Channel::Master, Channel::Red, Channel::Green, Channel::Blue];
}

/// Per-channel curve tint. Master reuses the app accent; R/G/B use their hue.
pub(crate) fn channel_style(ch: Channel) -> CurveStyle {
    match ch {
        Channel::Master => CurveStyle {
            curve_color: theme::ACCENT,
            point_color: theme::ACCENT_BRIGHT,
        },
        Channel::Red => CurveStyle {
            curve_color: Color32::from_rgb(0xe0, 0x6c, 0x6c),
            point_color: Color32::from_rgb(0xf0, 0x9a, 0x9a),
        },
        Channel::Green => CurveStyle {
            curve_color: Color32::from_rgb(0x6c, 0xd0, 0x7c),
            point_color: Color32::from_rgb(0x9a, 0xe6, 0xa6),
        },
        Channel::Blue => CurveStyle {
            curve_color: Color32::from_rgb(0x6c, 0x9c, 0xe0),
            point_color: Color32::from_rgb(0x9a, 0xc0, 0xf0),
        },
    }
}

/// Read the currently-selected channel from egui memory (UI-only; not persisted).
fn read_channel(ui: &egui::Ui, id: egui::Id) -> Channel {
    ui.memory(|m| m.data.get_temp::<Channel>(id))
        .unwrap_or(Channel::Master)
}

/// Borrow a channel's `(points, mode)` out of the `ToneCurve`. Master = the
/// legacy `points`/`mode`; R/G/B = the matching `PointCurve`.
fn channel_curve(tc: &ToneCurve, ch: Channel) -> (Vec<(f32, f32)>, CurveMode) {
    match ch {
        Channel::Master => (tc.points.clone(), tc.mode),
        Channel::Red => (tc.red.points.clone(), tc.red.mode),
        Channel::Green => (tc.green.points.clone(), tc.green.mode),
        Channel::Blue => (tc.blue.points.clone(), tc.blue.mode),
    }
}

/// Return a new `ToneCurve` with `ch`'s points+mode replaced.
fn with_channel(
    mut tc: ToneCurve,
    ch: Channel,
    points: Vec<(f32, f32)>,
    mode: CurveMode,
) -> ToneCurve {
    match ch {
        Channel::Master => {
            tc.points = points;
            tc.mode = mode;
        }
        Channel::Red => tc.red = PointCurve { points, mode },
        Channel::Green => tc.green = PointCurve { points, mode },
        Channel::Blue => tc.blue = PointCurve { points, mode },
    }
    tc
}

pub fn show(ui: &mut egui::Ui, scoped: &ScopedEdit) -> Option<EditOutcome> {
    let Some(set) = scoped.set() else {
        ui.label(egui::RichText::new(MASK_NONE_HINT).color(theme::TEXT_FAINT));
        return None;
    };
    let tc = set.tone_curve.clone();
    let channel_id = ui.id().with("tone_curve_channel");
    let mut channel = read_channel(ui, channel_id);

    // ── Channel selector: Master / R / G / B, tinted so the active one reads.
    ui.horizontal(|ui| {
        for ch in Channel::ALL {
            let selected = ch == channel;
            if ui.selectable_label(selected, ch.label()).clicked() {
                channel = ch;
                ui.memory_mut(|m| m.data.insert_temp(channel_id, channel));
            }
        }
    });

    let (points, stored_mode) = channel_curve(&tc, channel);
    // A never-edited channel (empty points) starts in Smooth — the new-curve
    // default (Linear only exists for pre-feature master sidecars).
    let display_points = if points.is_empty() {
        curve_math::identity_points()
    } else {
        points
    };
    let display_mode = if curve_math::is_identity(&display_points) {
        CurveMode::Smooth
    } else {
        stored_mode
    };

    let mut out: Option<EditOutcome> = None;

    if let Some(edit) = curve_editor(
        ui,
        ("tone_curve", channel.label()),
        &display_points,
        display_mode,
        &channel_style(channel),
    ) {
        // Set adjusting whenever a curve point is being dragged (not committed),
        // before checking for identity to ensure we suppress the mask overlay
        // during mid-drag pauses.
        if !edit.commit {
            scoped.adjusting.set(true);
        }
        // Reset OR an edit that lands on identity → clear this channel.
        let new_points = if edit.reset || curve_math::is_identity(&edit.points) {
            Vec::new()
        } else {
            edit.points
        };
        let new_tc = with_channel(tc.clone(), channel, new_points, edit.mode);
        let mut new_set = set.clone();
        new_set.tone_curve = new_tc;
        if let Some(o) = scoped.write(new_set, OpKind::ToneCurve, edit.commit) {
            out = Some(o);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_channel_has_a_distinct_tint() {
        let m = channel_style(Channel::Master).curve_color;
        let r = channel_style(Channel::Red).curve_color;
        let g = channel_style(Channel::Green).curve_color;
        let b = channel_style(Channel::Blue).curve_color;
        assert!(
            m != r && r != g && g != b && b != m,
            "channel tints must differ"
        );
    }

    #[test]
    fn channel_label_is_short_and_stable() {
        assert_eq!(Channel::Master.label(), "Master");
        assert_eq!(Channel::Red.label(), "R");
        assert_eq!(Channel::Green.label(), "G");
        assert_eq!(Channel::Blue.label(), "B");
    }
}
