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
