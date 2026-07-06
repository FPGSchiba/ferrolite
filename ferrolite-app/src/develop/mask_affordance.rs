//! Pure hit-test / handle-drag / stroke-capture / eyedropper math for the mask
//! tools, in normalized SOURCE coordinates ([0,1]²). No egui, no GPU — the
//! canvas overlay only routes pointer events into these (crop-overlay discipline,
//! Spec 2 §8). `p`/handles are already inverse-mapped to source coords by the
//! caller via `display_to_source`.

use ferrolite_mask::{BrushNode, Rgb, Vec2};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinHandle {
    Start,
    End,
    Body,
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Brush dab params captured from the current UI slider values, threaded into
/// each appended node.
#[derive(Clone, Copy)]
pub struct BrushParams {
    pub radius: f32,
    pub hardness: f32,
    pub flow: f32,
}

/// Minimum spacing between captured dab nodes, as a fraction of the brush radius.
/// Matches the engine's dab spacing philosophy (`ferrolite_mask::SPACING_FRAC`)
/// so captured nodes aren't denser than the rasterizer needs.
const CAPTURE_SPACING_FRAC: f32 = 0.25;

/// Append a dab node at `p` (source coords) iff it is at least
/// `CAPTURE_SPACING_FRAC · radius` from the last node (or the list is empty).
/// Returns whether a node was appended.
pub fn append_brush_node(nodes: &mut Vec<BrushNode>, p: (f32, f32), params: BrushParams) -> bool {
    let min_d = (CAPTURE_SPACING_FRAC * params.radius).max(1e-4);
    if let Some(last) = nodes.last() {
        if dist((last.pos.x, last.pos.y), p) < min_d {
            return false;
        }
    }
    nodes.push(BrushNode {
        pos: Vec2::new(p.0, p.1),
        radius: params.radius,
        hardness: params.hardness,
        flow: params.flow,
    });
    true
}

/// Distance from point `p` to segment `a→b`.
fn point_seg_dist(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let len2 = abx * abx + aby * aby;
    if len2 < 1e-12 {
        return dist(a, p);
    }
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0);
    dist((a.0 + t * abx, a.1 + t * aby), p)
}

/// Which linear-gradient handle (if any) is within `r` of `p`. Endpoints win over
/// the body; the body matches anywhere within `r` of the axis segment.
pub fn linear_hit_test(
    start: (f32, f32),
    end: (f32, f32),
    p: (f32, f32),
    r: f32,
) -> Option<LinHandle> {
    if dist(start, p) <= r {
        return Some(LinHandle::Start);
    }
    if dist(end, p) <= r {
        return Some(LinHandle::End);
    }
    if point_seg_dist(start, end, p) <= r {
        return Some(LinHandle::Body);
    }
    None
}

/// Move the targeted endpoint to `p` (Start/End). Body is handled by `linear_drag_body`.
pub fn linear_drag(
    start: (f32, f32),
    end: (f32, f32),
    h: LinHandle,
    p: (f32, f32),
) -> ((f32, f32), (f32, f32)) {
    match h {
        LinHandle::Start => (p, end),
        LinHandle::End => (start, p),
        LinHandle::Body => (start, end),
    }
}

/// Translate the whole axis by a source-space delta.
pub fn linear_drag_body(
    start: (f32, f32),
    end: (f32, f32),
    d: (f32, f32),
) -> ((f32, f32), (f32, f32)) {
    ((start.0 + d.0, start.1 + d.1), (end.0 + d.0, end.1 + d.1))
}

/// Sample the source image at a normalized source point (nearest pixel). Used by
/// the color-range eyedropper. Coords are clamped into range.
pub fn sample_source(img: &ferrolite_image::LinearRgbaF32, src_norm: (f32, f32)) -> Rgb {
    let x = ((src_norm.0.clamp(0.0, 1.0) * img.width as f32) as u32).min(img.width - 1);
    let y = ((src_norm.1.clamp(0.0, 1.0) * img.height as f32) as u32).min(img.height - 1);
    let i = ((y * img.width + x) * 4) as usize;
    Rgb::new(img.pixels[i], img.pixels[i + 1], img.pixels[i + 2])
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RadHandle {
    Center,
    RadiusX,
    RadiusY,
    /// Create-only: grows both radii together from the drag origin (center),
    /// so drag-to-create yields an ellipse fit to the drag instead of a thin
    /// horizontal line. Never returned by `radial_hit_test` — only used by the
    /// overlay's drag-to-create gesture.
    Both,
}

/// Which radial handle is within `r` of `p`. Rotation is ignored for hit-testing
/// the axis endpoints in P1 (axis-aligned handles; rotation via a later handle).
pub fn radial_hit_test(
    center: (f32, f32),
    radius: (f32, f32),
    _rot: f32,
    p: (f32, f32),
    r: f32,
) -> Option<RadHandle> {
    if dist(center, p) <= r {
        return Some(RadHandle::Center);
    }
    if dist((center.0 + radius.0, center.1), p) <= r {
        return Some(RadHandle::RadiusX);
    }
    if dist((center.0, center.1 + radius.1), p) <= r {
        return Some(RadHandle::RadiusY);
    }
    None
}

/// Apply a radial drag: Center moves the center; RadiusX/Y set the extent to
/// `|p − center|` on that axis (clamped ≥ a tiny epsilon so the ellipse stays valid).
pub fn radial_drag(
    center: (f32, f32),
    radius: (f32, f32),
    _rot: f32,
    h: RadHandle,
    p: (f32, f32),
) -> ((f32, f32), (f32, f32)) {
    match h {
        RadHandle::Center => (p, radius),
        RadHandle::RadiusX => (center, ((p.0 - center.0).abs().max(1e-3), radius.1)),
        RadHandle::RadiusY => (center, (radius.0, (p.1 - center.1).abs().max(1e-3))),
        RadHandle::Both => (
            center,
            (
                (p.0 - center.0).abs().max(1e-3),
                (p.1 - center.1).abs().max(1e-3),
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_brush_node_gates_on_min_distance() {
        use ferrolite_mask::BrushNode;
        let mut nodes: Vec<BrushNode> = vec![];
        let params = BrushParams {
            radius: 0.05,
            hardness: 0.5,
            flow: 1.0,
        };
        // First sample always appends.
        assert!(append_brush_node(&mut nodes, (0.1, 0.1), params));
        assert_eq!(nodes.len(), 1);
        // A sample closer than spacing*radius does NOT append.
        assert!(!append_brush_node(&mut nodes, (0.101, 0.1), params));
        assert_eq!(nodes.len(), 1);
        // A sample far enough appends.
        assert!(append_brush_node(&mut nodes, (0.3, 0.1), params));
        assert_eq!(nodes.len(), 2);
        assert!((nodes[1].radius - 0.05).abs() < 1e-6 && (nodes[1].flow - 1.0).abs() < 1e-6);
    }

    #[test]
    fn linear_hit_test_finds_endpoints() {
        let (s, e) = ((0.2f32, 0.5f32), (0.8f32, 0.5f32));
        assert_eq!(
            linear_hit_test(s, e, (0.2, 0.5), 0.04),
            Some(LinHandle::Start)
        );
        assert_eq!(
            linear_hit_test(s, e, (0.8, 0.5), 0.04),
            Some(LinHandle::End)
        );
        // Near the line but between endpoints => Body (move whole).
        assert_eq!(
            linear_hit_test(s, e, (0.5, 0.5), 0.04),
            Some(LinHandle::Body)
        );
        // Far away => None.
        assert_eq!(linear_hit_test(s, e, (0.5, 0.9), 0.04), None);
    }

    #[test]
    fn linear_drag_moves_the_targeted_handle() {
        let (s, e) = ((0.2f32, 0.5f32), (0.8f32, 0.5f32));
        let (ns, ne) = linear_drag(s, e, LinHandle::End, (0.9, 0.6));
        assert_eq!(ns, s, "start unchanged");
        assert!((ne.0 - 0.9).abs() < 1e-6 && (ne.1 - 0.6).abs() < 1e-6);
    }

    #[test]
    fn linear_drag_body_translates_both() {
        let (s, e) = ((0.2f32, 0.5f32), (0.8f32, 0.5f32));
        // Body drag carries a delta (dx,dy); model as pointer delta from grab.
        let (ns, ne) = linear_drag_body(s, e, (0.1, -0.05));
        assert!((ns.0 - 0.3).abs() < 1e-6 && (ne.0 - 0.9).abs() < 1e-6);
        assert!((ns.1 - 0.45).abs() < 1e-6 && (ne.1 - 0.45).abs() < 1e-6);
    }

    #[test]
    fn radial_hit_test_center_and_axes() {
        let c = (0.5f32, 0.5f32);
        let rad = (0.3f32, 0.2f32);
        assert_eq!(
            radial_hit_test(c, rad, 0.0, (0.5, 0.5), 0.04),
            Some(RadHandle::Center)
        );
        // +x axis edge at center + (rx, 0) = (0.8, 0.5).
        assert_eq!(
            radial_hit_test(c, rad, 0.0, (0.8, 0.5), 0.04),
            Some(RadHandle::RadiusX)
        );
        // +y axis edge at (0.5, 0.7).
        assert_eq!(
            radial_hit_test(c, rad, 0.0, (0.5, 0.7), 0.04),
            Some(RadHandle::RadiusY)
        );
        assert_eq!(radial_hit_test(c, rad, 0.0, (0.1, 0.1), 0.04), None);
    }

    #[test]
    fn radial_drag_center_moves_center_only() {
        let (c, r) = radial_drag((0.5, 0.5), (0.3, 0.2), 0.0, RadHandle::Center, (0.4, 0.45));
        assert!((c.0 - 0.4).abs() < 1e-6 && (c.1 - 0.45).abs() < 1e-6);
        assert_eq!(r, (0.3, 0.2), "radius unchanged when moving center");
    }

    #[test]
    fn radial_drag_radius_x_sets_x_extent() {
        let (c, r) = radial_drag((0.5, 0.5), (0.3, 0.2), 0.0, RadHandle::RadiusX, (0.9, 0.5));
        assert_eq!(c, (0.5, 0.5));
        assert!((r.0 - 0.4).abs() < 1e-6, "rx = |px - cx| = 0.4");
        assert!((r.1 - 0.2).abs() < 1e-6, "ry unchanged");
    }

    #[test]
    fn radial_drag_both_sets_both_extents() {
        // Drag-to-create: both radii should grow to |pointer - center| per axis,
        // center unchanged. Guards against the "horizontal line" bug where only
        // radius.x grew during create.
        let (c, r) = radial_drag((0.5, 0.5), (1e-3, 1e-3), 0.0, RadHandle::Both, (0.8, 0.7));
        assert_eq!(c, (0.5, 0.5), "center unchanged");
        assert!((r.0 - 0.3).abs() < 1e-6, "rx = |0.8 - 0.5| = 0.3");
        assert!((r.1 - 0.2).abs() < 1e-6, "ry = |0.7 - 0.5| = 0.2");
    }

    #[test]
    fn sample_source_reads_the_nearest_pixel() {
        use ferrolite_image::LinearRgbaF32;
        // 2x1: left = red, right = green.
        let img = LinearRgbaF32::new(2, 1, vec![1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0]).unwrap();
        let left = sample_source(&img, (0.0, 0.5));
        assert!(
            (left.r - 1.0).abs() < 1e-6 && left.g < 1e-6,
            "left pixel is red"
        );
        let right = sample_source(&img, (0.99, 0.5));
        assert!(
            right.r < 1e-6 && (right.g - 1.0).abs() < 1e-6,
            "right pixel is green"
        );
    }
}
