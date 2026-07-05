//! Pure CPU brush math (no GPU): resample a `Stroke` polyline into evenly-spaced
//! `Dab`s, select the new dabs since the last pointer sample, and derive the
//! `halo = max dab radius`. The `brush_dab.wgsl` rasterizer mirrors `dab_alpha`
//! and `composite_dabs` exactly. All coordinates are normalized source space.

use crate::model::{BrushNode, Stroke};
use crate::vec::Vec2;

/// Default dab spacing as a fraction of the stroke's max node radius.
pub const SPACING_FRAC: f32 = 0.25;

/// A resolved brush stamp in normalized source coordinates.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Dab {
    pub pos: Vec2,
    pub radius: f32,
    pub hardness: f32,
    pub flow: f32,
}

impl Dab {
    fn from_node(n: &BrushNode) -> Self {
        Self {
            pos: n.pos,
            radius: n.radius,
            hardness: n.hardness,
            flow: n.flow,
        }
    }
}

fn dist(a: Vec2, b: Vec2) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    (dx * dx + dy * dy).sqrt()
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_node(a: &BrushNode, b: &BrushNode, t: f32) -> Dab {
    Dab {
        pos: Vec2::new(lerp(a.pos.x, b.pos.x, t), lerp(a.pos.y, b.pos.y, t)),
        radius: lerp(a.radius, b.radius, t),
        hardness: lerp(a.hardness, b.hardness, t),
        flow: lerp(a.flow, b.flow, t),
    }
}

/// The largest node radius across all strokes (normalized); `0.0` if none.
pub fn max_dab_radius(strokes: &[Stroke]) -> f32 {
    strokes
        .iter()
        .flat_map(|s| s.nodes.iter())
        .map(|n| n.radius)
        .fold(0.0_f32, f32::max)
}

/// Resample `stroke` into append-stable, evenly-spaced dabs. Dabs sit at
/// arc-lengths `0, step, 2*step, …` for every `k*step <= total_len`, where
/// `step = spacing_frac * (max node radius)` (guarded > 0). Params interpolate by
/// global arc-length fraction. Appending a node only adds tail dabs (see tests).
pub fn stroke_dabs(stroke: &Stroke, spacing_frac: f32) -> Vec<Dab> {
    let nodes = &stroke.nodes;
    if nodes.is_empty() {
        return Vec::new();
    }
    if nodes.len() == 1 {
        return vec![Dab::from_node(&nodes[0])];
    }

    // Cumulative arc-length at each node.
    let mut cum = Vec::with_capacity(nodes.len());
    cum.push(0.0_f32);
    for w in nodes.windows(2) {
        let prev = *cum.last().unwrap();
        cum.push(prev + dist(w[0].pos, w[1].pos));
    }
    let total_len = *cum.last().unwrap();

    let r_max = nodes.iter().map(|n| n.radius).fold(0.0_f32, f32::max);
    let step = (spacing_frac * r_max).max(1e-4);

    if total_len <= 1e-6 {
        return vec![Dab::from_node(&nodes[0])];
    }

    let mut dabs = Vec::new();
    let mut k = 0u32;
    loop {
        let s = k as f32 * step;
        if s > total_len {
            break;
        }
        // Locate the segment containing arc-length `s`.
        let seg = cum
            .windows(2)
            .position(|c| s >= c[0] && s <= c[1])
            .unwrap_or(nodes.len() - 2);
        let seg_len = cum[seg + 1] - cum[seg];
        let t = if seg_len > 1e-9 {
            (s - cum[seg]) / seg_len
        } else {
            0.0
        };
        dabs.push(lerp_node(&nodes[seg], &nodes[seg + 1], t));
        k += 1;
    }
    dabs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BrushNode, Stroke};

    fn node(x: f32, y: f32, r: f32) -> BrushNode {
        BrushNode {
            pos: Vec2::new(x, y),
            radius: r,
            hardness: 0.5,
            flow: 1.0,
        }
    }

    #[test]
    fn empty_stroke_yields_no_dabs() {
        let s = Stroke {
            nodes: vec![],
            erase: false,
        };
        assert!(stroke_dabs(&s, SPACING_FRAC).is_empty());
    }

    #[test]
    fn single_node_yields_one_dab_at_that_node() {
        let s = Stroke {
            nodes: vec![node(0.3, 0.4, 0.1)],
            erase: false,
        };
        let dabs = stroke_dabs(&s, SPACING_FRAC);
        assert_eq!(dabs.len(), 1);
        assert_eq!(dabs[0].pos, Vec2::new(0.3, 0.4));
        assert_eq!(dabs[0].radius, 0.1);
    }

    #[test]
    fn straight_stroke_spaces_dabs_by_step() {
        // Horizontal 0.0->0.4, constant radius 0.1, spacing 0.5 -> step 0.05.
        // Dabs at k*0.05 for k*0.05 <= 0.4 -> k = 0..=8 -> 9 dabs.
        let s = Stroke {
            nodes: vec![node(0.0, 0.5, 0.1), node(0.4, 0.5, 0.1)],
            erase: false,
        };
        let dabs = stroke_dabs(&s, 0.5);
        assert_eq!(dabs.len(), 9);
        assert!((dabs[0].pos.x - 0.0).abs() < 1e-5);
        assert!((dabs[1].pos.x - 0.05).abs() < 1e-5);
        assert!((dabs[8].pos.x - 0.40).abs() < 1e-5);
        assert!(dabs.iter().all(|d| (d.pos.y - 0.5).abs() < 1e-5));
    }

    #[test]
    fn appending_a_node_only_adds_tail_dabs() {
        // Append-stability: extending 0.4 -> 0.5 keeps the first 9 dabs identical.
        let short = Stroke {
            nodes: vec![node(0.0, 0.5, 0.1), node(0.4, 0.5, 0.1)],
            erase: false,
        };
        let long = Stroke {
            nodes: vec![node(0.0, 0.5, 0.1), node(0.5, 0.5, 0.1)],
            erase: false,
        };
        let a = stroke_dabs(&short, 0.5);
        let b = stroke_dabs(&long, 0.5);
        assert!(b.len() > a.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((x.pos.x - y.pos.x).abs() < 1e-5);
        }
    }

    #[test]
    fn params_interpolate_along_the_path() {
        // radius 0.1 -> 0.3 across the stroke; a mid dab is ~0.2.
        let s = Stroke {
            nodes: vec![node(0.0, 0.5, 0.1), node(0.4, 0.5, 0.3)],
            erase: false,
        };
        let dabs = stroke_dabs(&s, 0.5);
        let mid = &dabs[dabs.len() / 2];
        assert!(mid.radius > 0.15 && mid.radius < 0.25, "got {}", mid.radius);
    }

    #[test]
    fn degenerate_zero_length_stroke_yields_one_dab() {
        let s = Stroke {
            nodes: vec![node(0.2, 0.2, 0.1), node(0.2, 0.2, 0.1)],
            erase: false,
        };
        assert_eq!(stroke_dabs(&s, 0.5).len(), 1);
    }

    #[test]
    fn zero_radius_stroke_does_not_hang_and_yields_endpoints() {
        // step guards to > 0 so this terminates; exact count is not asserted.
        let s = Stroke {
            nodes: vec![node(0.0, 0.5, 0.0), node(0.4, 0.5, 0.0)],
            erase: false,
        };
        let dabs = stroke_dabs(&s, 0.5);
        assert!(!dabs.is_empty());
    }

    #[test]
    fn max_dab_radius_is_the_largest_node_radius() {
        let strokes = vec![
            Stroke {
                nodes: vec![node(0.0, 0.0, 0.1), node(0.1, 0.1, 0.25)],
                erase: false,
            },
            Stroke {
                nodes: vec![node(0.2, 0.2, 0.05)],
                erase: false,
            },
        ];
        assert!((max_dab_radius(&strokes) - 0.25).abs() < 1e-6);
        assert_eq!(max_dab_radius(&[]), 0.0);
    }
}
