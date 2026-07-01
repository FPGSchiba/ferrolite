//! Pure before/after split-divider math (spec §7.2): fraction-of-canvas position
//! in `[MIN_POS, MAX_POS]`, screen-x mapping, pointer→position, and hit-testing.
//! No egui types — egui only routes pointer events into these functions.

/// Keep the divider clear of the extreme edges so a sliver of each side stays
/// visible and the handle is always grabbable.
pub const MIN_POS: f32 = 0.03;
pub const MAX_POS: f32 = 0.97;
/// Pointer-to-divider distance (screen px) treated as "on the handle".
pub const HANDLE_TOL: f32 = 10.0;

/// Clamp a fractional position into the usable range.
pub fn clamp_pos(pos: f32) -> f32 {
    pos.clamp(MIN_POS, MAX_POS)
}

/// Screen x of the divider inside a canvas at `left` with `width`.
pub fn divider_x(left: f32, width: f32, pos: f32) -> f32 {
    left + clamp_pos(pos) * width
}

/// Fractional position (clamped) for a pointer at screen x `pointer_x`.
pub fn pos_from_pointer(left: f32, width: f32, pointer_x: f32) -> f32 {
    if width <= 0.0 {
        return 0.5;
    }
    clamp_pos((pointer_x - left) / width)
}

/// True when `pointer_x` is within `tol` px of the divider.
pub fn hit_divider(left: f32, width: f32, pos: f32, pointer_x: f32, tol: f32) -> bool {
    (pointer_x - divider_x(left, width, pos)).abs() <= tol
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_within_bounds() {
        assert_eq!(clamp_pos(-1.0), MIN_POS);
        assert_eq!(clamp_pos(2.0), MAX_POS);
        assert!((clamp_pos(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn divider_x_maps_fraction_to_screen() {
        // canvas [100, 300): width 200, pos 0.5 -> x 200.
        assert!((divider_x(100.0, 200.0, 0.5) - 200.0).abs() < 1e-4);
    }

    #[test]
    fn pos_from_pointer_inverts_divider_x_and_clamps() {
        let (left, width) = (100.0, 200.0);
        let x = divider_x(left, width, 0.4);
        assert!((pos_from_pointer(left, width, x) - 0.4).abs() < 1e-4);
        // Far left/right clamp to the bounds.
        assert_eq!(pos_from_pointer(left, width, 0.0), MIN_POS);
        assert_eq!(pos_from_pointer(left, width, 10_000.0), MAX_POS);
    }

    #[test]
    fn hit_test_respects_tolerance() {
        let (left, width, pos) = (0.0, 100.0, 0.5); // divider at x=50
        assert!(hit_divider(left, width, pos, 54.0, HANDLE_TOL));
        assert!(!hit_divider(left, width, pos, 70.0, HANDLE_TOL));
    }
}
