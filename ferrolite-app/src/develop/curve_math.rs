//! Pure tone-curve control-point editing (normalized [0,1] space, x-ascending,
//! endpoints pinned at x=0 and x=1). Routed by `curve_widget`.

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

pub fn identity_points() -> Vec<(f32, f32)> {
    vec![(0.0, 0.0), (1.0, 1.0)]
}

pub fn is_identity(points: &[(f32, f32)]) -> bool {
    points.is_empty()
        || (points.len() == 2
            && (points[0].0 - 0.0).abs() < 1e-6
            && (points[0].1 - 0.0).abs() < 1e-6
            && (points[1].0 - 1.0).abs() < 1e-6
            && (points[1].1 - 1.0).abs() < 1e-6)
}

pub fn nearest_point(points: &[(f32, f32)], target: (f32, f32), max_dist: f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, p) in points.iter().enumerate() {
        let d = ((p.0 - target.0).powi(2) + (p.1 - target.1).powi(2)).sqrt();
        if d <= max_dist && best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

pub fn insert_point(points: &[(f32, f32)], p: (f32, f32)) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = points.to_vec();
    out.push((clamp01(p.0), clamp01(p.1)));
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

pub fn move_point(points: &[(f32, f32)], idx: usize, p: (f32, f32)) -> Vec<(f32, f32)> {
    let mut out = points.to_vec();
    if idx >= out.len() {
        return out;
    }
    let y = clamp01(p.1);
    let last = out.len() - 1;
    let x = if idx == 0 {
        0.0
    } else if idx == last {
        1.0
    } else {
        // Keep strictly between neighbors so x stays ascending.
        let lo = out[idx - 1].0 + 1e-4;
        let hi = out[idx + 1].0 - 1e-4;
        let x = clamp01(p.0);
        // Guard degenerate case: when neighbors are within 2e-4 of each other,
        // lo > hi and f32::clamp would panic. Pin to lo in that case.
        if lo <= hi {
            x.clamp(lo, hi)
        } else {
            lo
        }
    };
    out[idx] = (x, y);
    out
}

pub fn delete_point(points: &[(f32, f32)], idx: usize) -> Vec<(f32, f32)> {
    // Endpoints (first/last) are not deletable.
    if idx == 0 || idx + 1 >= points.len() {
        return points.to_vec();
    }
    let mut out = points.to_vec();
    out.remove(idx);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-5 && (a.1 - b.1).abs() < 1e-5
    }

    #[test]
    fn identity_is_two_corner_points() {
        assert!(is_identity(&identity_points()));
        assert!(is_identity(&[]), "empty is identity too");
        assert!(!is_identity(&[(0.0, 0.0), (0.5, 0.7), (1.0, 1.0)]));
    }

    #[test]
    fn insert_keeps_x_sorted_and_clamps() {
        let pts = insert_point(&identity_points(), (1.5, -0.2));
        assert!(pts.windows(2).all(|w| w[0].0 <= w[1].0), "x ascending");
        assert!(pts
            .iter()
            .all(|p| (0.0..=1.0).contains(&p.0) && (0.0..=1.0).contains(&p.1)));
    }

    #[test]
    fn nearest_finds_within_radius_else_none() {
        let pts = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        assert_eq!(nearest_point(&pts, (0.52, 0.48), 0.1), Some(1));
        assert_eq!(nearest_point(&pts, (0.3, 0.9), 0.05), None);
    }

    #[test]
    fn move_interior_clamps_between_neighbors() {
        let pts = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        // Try to drag the middle point past the right endpoint in x.
        let moved = move_point(&pts, 1, (1.4, 0.8));
        assert!(
            moved[1].0 < moved[2].0,
            "x stays left of the right neighbor"
        );
        assert!(
            moved[1].0 > moved[0].0,
            "x stays right of the left neighbor"
        );
    }

    #[test]
    fn move_endpoints_keep_x_fixed() {
        let pts = identity_points();
        let m0 = move_point(&pts, 0, (0.3, 0.4));
        assert!(
            approx((m0[0].0, m0[0].1), (0.0, 0.4)),
            "left endpoint x pinned at 0"
        );
        let last = pts.len() - 1;
        let m1 = move_point(&pts, last, (0.7, 0.2));
        assert!(
            approx((m1[last].0, m1[last].1), (1.0, 0.2)),
            "right endpoint x pinned at 1"
        );
    }

    #[test]
    fn delete_keeps_endpoints() {
        let pts = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        assert_eq!(delete_point(&pts, 1).len(), 2, "interior deletable");
        assert_eq!(
            delete_point(&pts, 0).len(),
            3,
            "left endpoint not deletable"
        );
        assert_eq!(
            delete_point(&pts, 2).len(),
            3,
            "right endpoint not deletable"
        );
    }

    #[test]
    fn move_interior_with_near_neighbors_does_not_panic() {
        // Index 2's immediate neighbors (idx 1 = 0.50000, idx 3 = 0.50010) are only
        // 1e-4 apart (< 2e-4), so lo (0.50010) > hi (0.50000): the degenerate guard
        // branch is exercised. Without the guard, f32::clamp(min>max) would panic.
        let pts = vec![
            (0.0, 0.0),
            (0.50000_f32, 0.5),
            (0.50005_f32, 0.5),
            (0.50010_f32, 0.5),
            (1.0, 1.0),
        ];
        let moved = move_point(&pts, 2, (0.9, 0.3)); // reaching this line proves no panic
                                                     // Result is finite and within the unit square (guard pins to `lo`).
        assert!(moved[2].0.is_finite());
        assert!(
            (0.0..=1.0).contains(&moved[2].0),
            "x in [0,1]; got {}",
            moved[2].0
        );
    }

    #[test]
    fn delete_two_point_curve_keeps_both_endpoints() {
        let two = identity_points();
        assert_eq!(
            delete_point(&two, 0).len(),
            2,
            "left endpoint of 2-point curve not deletable"
        );
        assert_eq!(
            delete_point(&two, 1).len(),
            2,
            "right endpoint of 2-point curve not deletable"
        );
    }
}
